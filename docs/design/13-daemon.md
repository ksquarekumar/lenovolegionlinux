# 13 — Operator daemon: processes, state, bootstrap, resilience

Requirements (2026-10-12): one SQLite database owned by the operator daemon and managed by the bootstrap; producers
and consumers coordinate through a shared write-ahead event log (sagas for multi-step work); every UI defers
entirely to the daemon; the daemon has its own kernel/eBPF toolkit; it bootstraps itself on install, upgrade and
boot; shared memory where it helps; any runnable can restart or exit gracefully and prompt, without bringing the
system down.

## Processes

| Unit | Scope | Owns | API |
| --- | --- | --- | --- |
| `legionctl-bootstrap.service` | system and user (oneshot) | install/upgrade/boot checks, schema migrations, version marker | none; exits |
| `legionctl-system.service` | system (root) | machine control: power, firmware, sysctl, kernel modules, eBPF, initramfs | `/run/legionctl/system.sock`, D-Bus `org.legionctl.System1` with polkit per method |
| `legionctl-operator.service` | user (`systemd --user`) | agent harness: workspaces, hooks, reminders, ACP sessions, gateway | `$XDG_RUNTIME_DIR/legionctl/api.sock` (axum: HTTP + WebSocket) |
| clients | user | nothing | CLI, TUI, GPUI app, MCP server, hook dispatcher |

- Clients are thin: they read and write only through the APIs. With a daemon down they show "legionctl-operator is not
  running" with a start action. There is no direct-read path.
- Privileged changes go from `legionctl-operator` (or a client) to `legionctl-system` over D-Bus; polkit decides per method.
  UIs never run `pkexec` themselves once `legionctl-system` exists.

## State: one database per scope, one writer

- System: `/var/lib/legionctl/system.db` (the Fn+Q ledger). User: `~/.local/state/legionctl/operator.db` (the event
  log; schema v2 dropped the hook tables, which are config now) and `~/.agentworkspace/workspace.db` (dossier
  bindings and writes, 18).
- The owning daemon is the only writer (`legionctl-system` for `system.db`, `legionctl-operator` for the user
  databases); WAL mode lets the bootstrap and tools read concurrently.
- All legionctl state is in SQLite (decided 2026-10-12): the remaining Phase 1a stores (worktree registry JSON,
  reminders TOML) and the Fn+Q ledger (today files in `/run/legionctl`; in `system.db` keyed by boot id, so a reboot
  still clears it) move in, each with a one-time import migration. Configuration stays in files (the profile as
  JSONC, the config layers of 14-sessions); the databases hold state, never configuration.
- Graph data (the workspace work graph, the capability/provider/consumer model, links to code graphs) uses graph
  tables in the same SQLite file: `node(id, kind, key, props)` and `edge(src, dst, kind, props, valid_from,
  valid_to)` with indexes on `(src, kind)` and `(dst, kind)`, queried with recursive CTEs behind a small `Graph`
  trait (`neighbors`, `walk`, `path`). Each repo's ctx code graph `.db` is `ATTACH`ed and joined in place. Chosen over
  a separate embedded graph engine (LadybugDB, the maintained Kuzu fork, was the best alternative) because WAL already
  gives one writer and many reader processes, there is no new dependency, and there is one backup and migration
  path. The trait lets an engine replace the tables if graph algorithms become necessary.

## Event log, outbox and sagas

- **Event log** (`events`): append-only record of every change: time, actor (cli, mcp, hook, daemon, bootstrap,
  kernel-module manager), kind, payload. It is the audit trail and the replication stream.
- **Projections:** tables such as the dossier `bindings` are the current state; they are updated in the same transaction
  as the event, so the log and the state never disagree.
- **Consumers** (OSD, UIs, the kernel-module manager, agent sessions) follow the log by cursor (`last_event_id`) over
  the API's WebSocket, at-least-once; handlers are idempotent (keyed by event id).
- **Outbox** for side effects outside the database (write a sysfs file, rebuild an initramfs, call a provider): the
  intent is committed first, a worker performs it and records the result. A crash between the two replays safely.
- **Sagas** for multi-step operations that span restarts or reboots, for example switching to discrete-GPU mode:

  | Step | Action | Compensation |
  | --- | --- | --- |
  | 1 | write initramfs drop-in with NVIDIA modules | restore previous drop-in |
  | 2 | rebuild initramfs for the default kernel, verify size and signature | rebuild from restored drop-in |
  | 3 | human approval, then set firmware GPU mode | set previous mode |
  | 4 | reboot | none |
  | 5 | after boot: verify panel on NVIDIA, desktop GPU preference | run 3, 2, 1 compensations |

  Each saga is a row with its step, state and inputs; the daemon resumes it after a restart or reboot. Approvals
  are saga steps, so a pending approval survives a restart.

## Bootstrap

`legionctl-bootstrap` runs from the package's post-install/upgrade hook and at every start of the daemons (as their
`ExecStartPre` equivalent, separate unit so a failure is visible):

1. Check binaries, units and the API version expected by installed clients.
2. Back up each database (SQLite online backup) before a migration; apply pending migrations in order, forward only.
3. Check kernel prerequisites (08-kernel inventory, DKMS builds for every installed kernel, BPF/BTF support).
4. Write the running version; on failure, leave the previous version's database in place and start the daemon in
   safe mode (read-only API, firmware defaults), with a desktop notification that says what failed.

Built (2026-10-13, part 1), without a separate `legionctl-bootstrap` binary:
- One schema per database (todo-68, 2026-10-14, after the first full release install): `Db::open` makes the whole
  schema in a new file (`operator.db` v10, `workspace.db` v9, `cache.db` v1); a file at that version opens; a file of
  another version (from before the merge, or from a newer legionctl) is refused with its version and left as it is.
  The steps up to the merge are gone. The next schema change adds steps after these versions again, with a backup
  of the file before the first step.
- The build check: the client sends a fingerprint of the operation registry (`x-legionctl-api`); the operator sends
  its own back. A different build makes the client say once that the operator must restart, and an operation the
  old operator does not know says the same (after a package upgrade the running operator is the old build).
- Safe mode for the machine daemon: `legionctl-system.service` stops after 3 failures in 5 minutes and runs
  `legionctl-safe.service` (`legionctl-system --safe`): the firmware's balanced mode, and the reason in
  `/run/legionctl/safe-mode`, which `doctor` shows first; the next successful start removes it.
- The desktop notification (part 2): the operator's session module reads `/run/legionctl/safe-mode` at its start,
  every 30 s and whenever the daemon's bus name changes owner; a new reason becomes one critical notification
  (`org.freedesktop.Notifications.Notify`: "legionctl: safe mode", the reason and what to run), once per reason.
Not yet: the kernel prerequisite checks as a boot step (they are `doctor` checks).

## Kernel and eBPF toolkit

- Kernel modules and DKMS through the 08-kernel inventory; `acpi_call` use (fan RPM, future WMI calls) only inside
  `legionctl-system`, serialized, because `/proc/acpi/call` has one shared result buffer.
- eBPF programs (aya) loaded by `legionctl-system` and pinned under `/sys/fs/bpf/legionctl/`, so they keep running across daemon
  restarts and are reattached, not reloaded.
- Firmware attributes, platform_profile, RAPL, cpufreq and sysctl through the actuator layer (01-core).

## Shared memory

Use it only where a socket is too slow or too chatty: the telemetry ring (power, temperatures, fan RPM, PSI,
throttle reasons at 1-10 Hz). `legionctl-system` writes it into a sealed `memfd` passed to clients over the socket; clients
map it read-only and read with a sequence counter (seqlock). Everything else, including all writes, goes through the
API and the database; no shared mutable state between processes outside SQLite.

## Failure isolation and graceful exit

- Every runnable is its own unit: `Restart=on-failure`, backoff, `WatchdogSec` with `sd_notify`, memory and CPU
  limits (`MemoryMax=`, `CPUWeight=`), `ProtectSystem=strict` with explicit `ReadWritePaths=`.
- Nothing in the boot path depends on legion: units are ordered after `basic.target`, never required by
  `sysinit.target` or `graphical.target`; a legion failure never blocks boot or login.
- Repeated failure triggers `OnFailure=legionctl-safe.service`: firmware defaults, then a desktop notification ("legion
  stopped after 3 failures: <reason>. Restart / Show log").
- SIGTERM: stop taking requests, bring running sagas to their next checkpoint, flush the outbox state, close the
  database, notify `STOPPING=1`. Clients see the socket close and reconnect with backoff.
- Hook dispatcher and MCP server already isolate failures per action and per tool (Phase 1a).

## Phase 1b work list

1. `legionctl-operator` (user): axum API on the unix socket; move worktrees and reminders into the database; WebSocket
   event stream; CLI, TUI and MCP become clients in the same change, and their direct database code is deleted.
2. `legionctl-bootstrap`: migrations with backup, version marker, safe mode; package hooks call it.
3. `legionctl-system` (system): D-Bus API with polkit; the Python tooling's work moves into it (switch in 15-layout); sagas for GPU-mode
   and initramfs changes.
4. Telemetry ring in a sealed memfd; TUI reads it.

## D-Bus contract (built 2026-10-14, DESIGN/03, todo-61)

`dbus/org.legionctl.System1.xml` is the reviewed interface: a machine test compares it with the running daemon's
`Introspect` output, word for word, and the machine tests call each method the operator uses against the real
daemon on a private bus. Method names match the operations: `ApplyPower` (power apply), `SetPrimaryGpu` (gpu
primary; polkit `org.legionctl.gpu.primary`), `Cleanup`, `Tune`, `RebootWindows` (windows reboot), `Sample`,
`WatchSessions`, `Hold` and `Release` (D1, polkit `org.legionctl.knobs.hold`). The `Version` property is the hash of the committed file; `doctor` says when the daemon and the
operator come from different files. Change events are `machine.<cause in kebab case>` (`machine.apply-power`).
Not done, on purpose: a `SafeMode` property (in safe mode the daemon does not run, so the marker file
`/run/legionctl/safe-mode` stays the channel), the Fn+Q ledger through D-Bus (the operator's dry runs read the same
root-owned `system.db` read-only, with the daemon's own code), and typed structs for `Tune` and `Sample` (they stay
JSON documents of `machine` types; D-Bus has no optional fields).
