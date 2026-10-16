# 13 — Operator daemon: processes, state, bootstrap, resilience

Requirements (2026-10-12): one SQLite database owned by the operator daemon and managed by the bootstrap; producers
and consumers coordinate through a shared write-ahead event log (sagas for multi-step work); every UI defers
entirely to the daemon; the daemon has its own kernel/eBPF toolkit; it bootstraps itself on install, upgrade and
boot; shared memory where it helps; any runnable can restart or exit gracefully and prompt, without bringing the
system down.

## Processes

| Unit | Scope | Owns | API |
| --- | --- | --- | --- |
| `legionctl-system.service` | system (root) | machine control: power, firmware, sysctl, kernel modules, eBPF, initramfs | `/run/legionctl/system.sock`, D-Bus `org.legionctl.System1` with polkit per method |
| `legionctl-operator.service` | user (`systemd --user`) | agent harness: workspaces, hooks, reminders, ACP sessions, gateway | `$XDG_RUNTIME_DIR/legionctl/api.sock` (axum: HTTP + SSE) |
| clients | user | nothing | CLI, TUI, GPUI app, MCP server, hook dispatcher |

No `legionctl-bootstrap` unit was built: schema steps run in `Db::open`, safe mode in `legionctl-safe.service` (see
Bootstrap).

- Clients are thin: they read and write only through the APIs. With a daemon down they show "legionctl-operator is not
  running" with a start action. There is no direct-read path.
- Privileged changes go from `legionctl-operator` (or a client) to `legionctl-system` over D-Bus; polkit decides per method.
  UIs never run `pkexec` themselves once `legionctl-system` exists.

## State: one database per scope, one writer

- System: `/var/lib/legionctl/system.db` (the Fn+Q ledger). User: `~/.local/state/legionctl/operator.db` (the event
  log, jobs, and since design 23 P9 the rows that span dossiers: bindings, workspaces, sessions, threads and the
  dossier registry) and each dossier's `~/agentWorkspaces/<dossier>/.state/dossier.db` (its writes, nudges and
  repository facts, 18).
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
- One schema per database, with steps (todo-68, todo-122): `operator.db` v13 (one schema for v1-v10, merged
  2026-10-14, then steps 10-12: shared dossier rows and threads (design 23 P9), pools (P6b), rlcd decisions
  (todo-138)), `dossier.db` v2, `cache.db` v1; `workspace.db` is read only by the one-time migration of P9 (up to
  v14). `Db::open` makes a new database whole in a private file (WAL mode, schema, version) and links it into place,
  so no opener sees it half made. A file at an older version that a step covers gets each step in order, each after
  a copy of the file to `<db>.v<old>.bak`, and not while an operator of another build holds `operator.lock`. A file
  older than the first step or newer than this release is refused with its version and left as it is.
- The build check: the client sends a fingerprint of the operation registry (`x-legionctl-api`); the operator sends
  its own back. A different build makes the client say once that the operator must restart, and an operation the
  old operator does not know says the same (after a package upgrade the running operator is the old build).
- Safe mode for the machine daemon: `legionctl-system.service` stops after 3 failures in 5 minutes and runs
  `legionctl-safe.service` (`legionctl-system --safe`): the firmware's balanced mode, and the reason in
  `/run/legionctl/safe-mode`, which `doctor` shows first; the next successful start removes it. With
  `RestartMode=direct` a failure that restarts does not trigger `OnFailure`; `--safe` acts only on systemd's result
  `start-limit-hit` (`MONITOR_SERVICE_RESULT`), so a stop that failed (the upgrade of 2026-10-16) is no safe mode.
- Stop (todo-125): SIGTERM drops the daemon's event loop at its next wait (an event's plan runs beside it) and the
  runtime does not wait for blocking work; the operator stops at once without waiting for open requests, event
  streams or jobs (a restart resumes cut-off jobs). The policy starts the NVIDIA services with `systemctl start
  --no-block`: a start job queues behind the daemon's own stop, and a wait for it lasted until SIGKILL.
- The desktop notification (part 2): the operator's session module reads `/run/legionctl/safe-mode` at its start,
  every 30 s and whenever the daemon's bus name changes owner; a new reason becomes one critical notification
  (`org.freedesktop.Notifications.Notify`: "legionctl: safe mode", the reason and what to run), once per reason.
Not yet: the kernel prerequisite checks as a boot step (they are `doctor` checks).

## Kernel and eBPF toolkit

- Kernel modules and DKMS through the 08-kernel inventory. The firmware's WMI only through the `legionctl_wmi`
  module's sysfs (fans, platform profile, firmware attributes); the module makes one call at a time. No `acpi_call`.
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
  limits (`MemoryMax=`, `CPUWeight=`), `ProtectSystem=strict` with explicit `ReadWritePaths=` (Sandbox, below).
- Nothing in the boot path depends on legion: units are ordered after `basic.target`, never required by
  `sysinit.target` or `graphical.target`; a legion failure never blocks boot or login.
- Repeated failure triggers `OnFailure=legionctl-safe.service`: firmware defaults, then a desktop notification ("legion
  stopped after 3 failures: <reason>. Restart / Show log").
- SIGTERM: stop taking requests, bring running sagas to their next checkpoint, flush the outbox state, close the
  database, notify `STOPPING=1`. Clients see the socket close and reconnect with backoff.
- Hook dispatcher and MCP server already isolate failures per action and per tool (Phase 1a).

## Sandbox (built 2026-10-15, user decision)

`legionctl-system.service` keeps `ProtectSystem=strict`: the daemon can write only the paths in the table. pacman,
ukify, sbsign and the `efibootmgr` changes need more, so their plans give them as `Step::Unit`, and the step executor
(`policy::execute`, one path for all of them) runs each as a transient unit with no sandbox: `systemd-run --wait --pipe
--collect --quiet --unit=legionctl-step-<pid>-<n> -- <argv>`. The argv goes as it is (no shell). The output and the
exit status come back as for `Step::Run`. The calling method's polkit check comes first, as for each step. A step
that fails stops the plan (todo-142 F-5): the steps after it do not run, and the call fails with the step, its reason
and the lines done before it (the `wrote <path>: <value> (was <old>)` lines that `undo` reads); only a `Step::Try`
may fail by design. The ledger has one writer: the daemon opens `system.db` for writing and fails when it cannot
(locked, corrupt); a reader (a user's status or dry run) reads it read-only, and only a missing file reads as empty
(todo-142 F-7).

| Path or program | Feature | How it is allowed |
| --- | --- | --- |
| `/sys` (platform_profile, firmware attributes, cpufreq, hwmon, battery, vfio bind, `/sys/fs/bpf/legionctl`), `/sys/firmware/efi/efivars` (`LoaderEntryOneShot`, `chattr -i`) | power, fan, battery, GPU, VFIO, eBPF pins, `kernel try`, `kernel cmdline` | `ProtectSystem=strict` keeps `/sys`, `/proc` and `/dev` writable; the unit has no `ProtectKernelTunables=` and no `PrivateDevices=` |
| `/dev/port`, `/dev/vfio/*` | EC byte, VFIO | as above |
| `/sys/bus/pci/devices/<NVIDIA GPU>/power/control` | `gpu power` | as `/sys` above |
| OpenRGB's SDK server (TCP `127.0.0.1:6742`, `LEGIONCTL_OPENRGB`) | knob `rgb.profile` | a connection, not a path: the unit does not limit the network |
| `/run/legionctl` (safe-mode, workload) | safe mode, workload sensor | `RuntimeDirectory=legionctl` |
| `/var/lib/legionctl` (`system.db`, generated configsets, tune reports, `kernel-install`) | ledger, sets, tune, `kernel install` | `StateDirectory=legionctl` |
| `/etc/legionctl` (`configsets`, `kernel/<kernel>.cmdline`, `.old`, `.trial`) | `rules`, boot and kernel lines | `ConfigurationDirectory=legionctl` |
| `/etc/udev/rules.d/61-legionctl-mutter-primary.rules` | `gpu primary` | `ReadWritePaths` |
| `/etc/polkit-1/rules.d/49-legionctl-windows-vm-<user>.rules` | Windows VM autostart | `ReadWritePaths` (polkit, a dependency, makes the folder) |
| `/etc/greetd/config.toml`, `config.toml.before` | `greeter install`, `remove` | `ReadWritePaths`; the package has the folder, so it is there before greetd and before the daemon starts |
| The ESP: `EFI/legionctl`, `EFI/Linux`, `loader/loader.conf` (`mkdir`, `install`, `mv`, `rm` in the daemon) | `boot`, `kernel default`, `kernel cmdline`, sets with boot entries | `ReadWritePaths=-/efi /boot`: the ESP is at `/efi`, `/boot` or `/boot/efi` (under `/boot`); `-` because `/efi` may not exist. Shortcut: with the ESP at `/boot/efi`, the kernel files in `/boot` are writable too. The daemon's mounts are slaves of the host's, so an ESP mounted after the daemon started comes into its namespace. An automount (`x-systemd.automount`, systemd-gpt-auto-generator) shows as `autofs` until it is opened: the daemon opens each place before it reads its mount table (VM test `under_its_own_unit_...`) |
| `pacman -U` | `kernel install` | `Step::Unit`: a package changes the system, and its hooks run mkinitcpio, DKMS and `--boot-rebuild` |
| `ukify`, `sbsign` | `boot install`, `rebuild`, `kernel cmdline`, sets | `Step::Unit`: ukify reads the kernel and initramfs and writes temporary files, which the daemon's `/tmp` (read-only) does not allow |
| `efibootmgr` (changes) | `boot`, `kernel try`, `kernel cmdline`, `windows reboot` | `Step::Unit` (user decision). A read (`efibootmgr` with no argument) runs in the daemon |
| `systemctl`, `loginctl`, `udevadm`, `logger` | greeter, Windows VM and disk, cleanup, GPU, reboot, journal | in the daemon: they ask a manager over a socket, and the manager writes its own files |
| power-profiles-daemon (`ActiveProfile` over D-Bus, `ppd.rs`; not `powerprofilesctl`, a Python program that aborted at exit under Python 3.14) | power | in the daemon: a D-Bus call; the manager writes its own files |
| `modprobe`, `cpupower`, `chattr`, `sbverify`, `bsdtar`, `modinfo`, `dkms status`, `nvidia-smi` | VFIO, power, boot checks, telemetry | in the daemon: they read, or write `/sys` |

Not under the unit: the one-shot modes that pacman hooks run (`--boot-rebuild`, `--remove-check`, `--greeter-remove`)
and `--safe` (`legionctl-safe.service`); they run as root without a sandbox, and their pacman, ukify, sbsign and
efibootmgr steps also run as transient units. Open: in a chroot (no running systemd) `systemd-run` fails, so the
`--boot-rebuild` hook cannot make the images there. Open: a `tune` workload runs as the caller inside the daemon's
sandbox, so it cannot write `/home` or `/tmp`.

Tests: the machine tests give the daemon a stand-in `systemd-run` that refuses other options and logs each program
(`legionctl_boot_comes_beside_the_old_chain_and_is_tried_once_before_it_leads`,
`kernel_install_takes_only_a_built_kernel_and_installs_roots_checked_copies`). The VM test
`under_its_own_unit_the_daemon_writes_the_greeter_the_esp_and_the_rules` installs the unit in Debian 13, starts the
daemon with systemd and calls Greeter install, Boot install, SetWindowsAutostart and SetPrimaryGpu over the system
bus; a path that the unit does not allow fails it.

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
`WatchSessions`, `Hold` and `Release` (D1, polkit `org.legionctl.knobs.hold`), `FanStatus` (no polkit),
`SetFanCurve`, `ResetFanCurve`, `SetFanMax`, `SetFanFullSpeed` (polkit `org.legionctl.fan`) and `SetFanLock` (polkit
`org.legionctl.fan.lock`, no kept authorization; 02-platform, Fan control), `Reload` (no polkit), `SetLayer` and
`SetLayerState` (polkit `org.legionctl.rules`; configsets, 01-core), `SetGpuPower` (gpu power; polkit `org.legionctl.gpu.power`), `Plan` (no polkit, todo-128: the plan of a `boot` or
`greeter` action as lines; the dry runs of boot and greeter and the greeter's refusal before the polkit dialog come
from it, because the daemon reads root-only files such as the MOK key in `/etc/refind.d/keys`). The `Version` property is the hash of the committed file; `doctor` says when the daemon and the
operator come from different files. Change events are `machine.<cause in kebab case>` (`machine.apply-power`).
Not done, on purpose: a `SafeMode` property (in safe mode the daemon does not run, so the marker file
`/run/legionctl/safe-mode` stays the channel), the Fn+Q ledger through D-Bus (the operator's dry runs read the same
root-owned `system.db` read-only, with the daemon's own code), and typed structs for `Tune` and `Sample` (they stay
JSON documents of `machine` types; D-Bus has no optional fields).
