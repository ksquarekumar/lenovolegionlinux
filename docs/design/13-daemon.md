# 13 — Operator daemon: processes, state, bootstrap, resilience

Requirements (2026-10-12): one SQLite database owned by the operator daemon and managed by the bootstrap; producers
and consumers coordinate through a shared write-ahead event log (sagas for multi-step work); every UI defers
entirely to the daemon; the daemon has its own kernel/eBPF toolkit; it bootstraps itself on install, upgrade and
boot; shared memory where it helps; any runnable can restart or exit gracefully and prompt, without bringing the
system down.

## Processes

| Unit | Scope | Owns | API |
| --- | --- | --- | --- |
| `legion-bootstrap.service` | system and user (oneshot) | install/upgrade/boot checks, schema migrations, version marker | none; exits |
| `legiond.service` | system (root) | machine control: power, firmware, sysctl, kernel modules, eBPF, initramfs | `/run/legion/system.sock`, D-Bus `org.legion.Control1` with polkit per method |
| `legion-agentd.service` | user (`systemd --user`) | agent harness: worktrees, plans, hooks, reminders, ACP sessions, gateway | `$XDG_RUNTIME_DIR/legion/api.sock` (axum: HTTP + WebSocket) |
| clients | user | nothing | CLI, TUI, GPUI app, MCP server, hook dispatcher |

- Clients are thin: they read and write only through the APIs. With a daemon down they show "legion-agentd is not
  running" with a start action, and read-only commands fall back to direct reads (what Phase 1a does today).
- Privileged changes go from `legion-agentd` (or a client) to `legiond` over D-Bus; polkit decides per method.
  UIs never run `pkexec` themselves once `legiond` exists.

## State: one database per scope, one writer

- System: `/var/lib/legion/system.db`. User: `~/.local/state/legion/legion.db` (exists since Phase 1a, schema v1:
  `events`, `hook_actions`, `hook_bindings`).
- The owning daemon is the only writer; WAL mode lets clients and the bootstrap read concurrently. Until the daemons
  exist, the CLI writes directly and relies on WAL plus a busy timeout.
- Remaining Phase 1a stores move in (worktree registry JSON, reminders TOML) with a one-time import migration.

## Event log, outbox and sagas

- **Event log** (`events`): append-only record of every change: time, actor (cli, mcp, hook, daemon, bootstrap,
  kernel-module manager), kind, payload. It is the audit trail and the replication stream.
- **Projections:** tables such as `hook_bindings` are the current state; they are updated in the same transaction
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

`legion-bootstrap` runs from the package's post-install/upgrade hook and at every start of the daemons (as their
`ExecStartPre` equivalent, separate unit so a failure is visible):

1. Check binaries, units and the API version expected by installed clients.
2. Back up each database (SQLite online backup) before a migration; apply pending migrations in order, forward only.
3. Check kernel prerequisites (08-kernel inventory, DKMS builds for every installed kernel, BPF/BTF support).
4. Write the running version; on failure, leave the previous version's database in place and start the daemon in
   safe mode (read-only API, firmware defaults), with a desktop notification that says what failed.

## Kernel and eBPF toolkit

- Kernel modules and DKMS through the 08-kernel inventory; `acpi_call` use (fan RPM, future WMI calls) only inside
  `legiond`, serialized, because `/proc/acpi/call` has one shared result buffer.
- eBPF programs (aya) loaded by `legiond` and pinned under `/sys/fs/bpf/legion/`, so they keep running across daemon
  restarts and are reattached, not reloaded.
- Firmware attributes, platform_profile, RAPL, cpufreq and sysctl through the actuator layer (01-core).

## Shared memory

Use it only where a socket is too slow or too chatty: the telemetry ring (power, temperatures, fan RPM, PSI,
throttle reasons at 1-10 Hz). `legiond` writes it into a sealed `memfd` passed to clients over the socket; clients
map it read-only and read with a sequence counter (seqlock). Everything else, including all writes, goes through the
API and the database; no shared mutable state between processes outside SQLite.

## Failure isolation and graceful exit

- Every runnable is its own unit: `Restart=on-failure`, backoff, `WatchdogSec` with `sd_notify`, memory and CPU
  limits (`MemoryMax=`, `CPUWeight=`), `ProtectSystem=strict` with explicit `ReadWritePaths=`.
- Nothing in the boot path depends on legion: units are ordered after `basic.target`, never required by
  `sysinit.target` or `graphical.target`; a legion failure never blocks boot or login.
- Repeated failure triggers `OnFailure=legion-safe.service`: firmware defaults, then a desktop notification ("legion
  stopped after 3 failures: <reason>. Restart / Show log").
- SIGTERM: stop taking requests, bring running sagas to their next checkpoint, flush the outbox state, close the
  database, notify `STOPPING=1`. Clients see the socket close and reconnect with backoff.
- Hook dispatcher and MCP server already isolate failures per action and per tool (Phase 1a).

## Phase 1b work list

1. `legion-agentd` (user): axum API on the unix socket; move worktrees and reminders into the database; WebSocket
   event stream; CLI, TUI and MCP become clients with the read-only fallback.
2. `legion-bootstrap`: migrations with backup, version marker, safe mode; package hooks call it.
3. `legiond` (system): D-Bus API with polkit; legionctl's apply/reassert/follow move behind it; sagas for GPU-mode
   and initramfs changes.
4. Telemetry ring in a sealed memfd; TUI reads it.
