# 04 — eBPF

Kernel support measured: `/sys/kernel/btf/vmlinux`, `CONFIG_BPF_SYSCALL`, `BPF_JIT`, `BPF_LSM`, `HID_BPF`,
`SCHED_CLASS_EXT`, `KPROBES`. Programs are written with **aya** (Rust, CO-RE, no clang at runtime) in the
`legionctl-bpf` crate. eBPF is the only path for these sensors: no `/proc` polling or other second source.

## Built (2026-10-13, M3f): the jank metric

`crates/legionctl-system/bpf/runq.bpf.c` hooks `tp_btf/sched_wakeup`, `sched_wakeup_new` and `sched_switch` (a
preempted task waits again) and counts the wait from enqueue to run in a per-CPU log2 histogram (µs). The one field
it reads (`task_struct.pid`) is a CO-RE relocation, so the program needs no kernel or libbpf headers: clang
compiles it at build time (`build.rs`), and aya loads it in `legionctl-system`. The kernel side is C, not aya's
Rust eBPF, because that needs a nightly toolchain and bpf-linker, which neither this machine nor CI has.

The daemon adds the cumulative histogram per CPU class (performance, efficiency) to `Sample()`; a telemetry sample
reads it at the start and the end of its window and gives `runq_latency` per class: count, p50 and p99 (upper
bound of the log2 bucket). `monitor`, `/v1/metrics` (`legionctl_runq_latency_p99_microseconds`), bench and tune
summaries carry it. A sensor that cannot load (no permission, the verifier's message) is `runq_error` in the sample
and a `doctor` finding; the daemon runs on. Tests: without root the error path (e2e); as root in CI (`sudo`) the
programs pass the verifier of the runner's kernel and count real wakeups.

## Built (2026-10-13): session activity

`crates/legionctl-system/bpf/exec.bpf.c` hooks `tp_btf/sched_process_exec`: when the current cgroup is in its
`watched` map, it puts the pid and the first 512 bytes of the new program's arguments (`mm->arg_start..arg_end`,
CO-RE) into a ring buffer. The operator calls `WatchSessions()` on `org.legionctl.System1` at its start (and again
when the daemon restarts); the daemon notes the caller's uid and connection, and each second points `watched` at that
user's `legionctl-session-<id>.scope` cgroups (`/sys/fs/cgroup/user.slice/user-<uid>.slice/user@<uid>.service`,
the folder's inode is the cgroup id). Each program start goes out as `SessionExec(session, pid, argv)` to that
connection alone (a unicast signal): arguments can hold secrets, and another local user never receives them. The
operator records `session.exec` events; `legionctl session activity <id>` lists them. A daemon without BPF
permission answers `WatchSessions` with the reason, and the operator records nothing. Tests: as root in CI, a
program started inside a scope cgroup is recorded with its arguments and one outside is not; the test asks the
daemon first, so a refused program fails it with the verifier's message.

Pinning (decision D7): each sensor pins its links, then the maps the daemon reads, under
`/sys/fs/bpf/legionctl/<sensor>-<build>` (`<build>` is the FNV-1a of the eBPF object). A daemon that failed and starts
again finds the pinned maps of its build and loads nothing: the programs stay attached and the run-queue histogram
goes on counting; the exec sensor clears the scopes the failed daemon watched. A load removes all other pins of the
sensor (another build, or a load that stopped before its maps were pinned). A stop (SIGTERM, also at a package
upgrade) and the package's removal (`pre_remove`) remove the pins, so nothing stays attached without a daemon. Test:
as root in CI, a killed daemon's successor keeps the same pinned histogram (its inode) and measures, and a SIGTERM
removes the pins.

## Built (2026-10-14, todo-74): workload detection

`crates/legionctl-system/bpf/workload.bpf.c` hooks `tp_btf/sched_process_exec` (the process id and the new name,
`comm`) and `tp_btf/sched_process_exit` (the end of a process: only its main thread), in order in one ring buffer.
It reads no kernel structure. The daemon (`src/workload.rs`) classifies the name: game (`reaper`, the process that
Steam starts each game under, `gamescope`, `wine`, `wine64`, `wineserver`, the wine preloaders), build (`cc1`,
`cc1plus`, `rustc`, `cargo`, `clang`, `gcc`, `g++`, the linkers, `make`, `ninja`), vm (`qemu-system-*`,
VirtualBox). It keeps each classified process until its end comes through the same ring, so the order of the events
removes any race. At its start, after the programs are attached, it looks once in `/proc` for the programs that
started before. A kind is active while one of its processes lives, and for 5 s after the last one (a build starts
short compilers one after another). Each second the daemon compares the active kinds; a change writes them to
`/run/legionctl/workload` and runs `Event::Workload`: the configsets' `workload` condition (note 01). A sensor that
cannot load writes its reason to `/run/legionctl/workload.error` instead, and `doctor` shows it. The pins are as
for the other sensors (`workload-<build>`). The Steam client alone is not a game. Shortcut: the name only, not the
cgroup (a Proton game runs as its `.exe` name under the wine preloader, which is found). Tests: without root the
error path (e2e); as root (CI's eBPF step, and a Debian 13 VM with kernel 7.2 by hand): a build tool that ran before
the daemon and one that starts later make a `workload build` layer apply, and the end of the process stops it.

Not built yet: the other uses below (power attribution, throttle correlation, HID-BPF), and the workload as an input
of the predictor (sched_ext mode, EPP, uclamp).

## Worth doing

| Use | Hook | Output |
| --- | --- | --- |
| Workload detection (built) | `sched_process_exec` / `sched_process_exit` tracepoints | `Workload` events (game, build, vm) from a classifier over the name; the cgroup later |
| Jank metric | `sched_wakeup` → `sched_switch` run-queue latency histogram, per CPU class (P/E core) | p50/p99 latency in the app; compare scx modes with numbers instead of feel |
| Power attribution | `sched_switch` + `power:cpu_frequency` | estimated per-process CPU energy share ("what is keeping the fans up") |
| Throttle correlation | `power:cpu_frequency` drops aligned with MSR throttle-status reads | which throttle reason fired during which workload |
| Session activity | exec and file-open tracepoints, filtered by the session's cgroup | what each agent session ran and touched, in its event log (14-sessions) |
| Input fixes | HID-BPF on HID devices (keyboard, Logitech receiver) | in-kernel remap/debounce, decided per device against logiops (one owner per device) |

## Not doing

- **Actuation through eBPF.** Firmware attributes, sysfs and D-Bus are the right write paths; eBPF adds risk
  there for no gain.
- **A custom sched_ext scheduler.** scx_lavd with `--autopower` is maintained upstream; legionctl-system selects
  schedulers and modes through scx_loader. Revisit only if the jank metric shows a gap no existing scheduler
  closes.
- **Ideapad Fn keys via HID-BPF.** That device is a platform/WMI input device, not HID; evdev is the hook.

## Safety

- `legionctl-bootstrap` checks BTF and BPF support before start (13-daemon); a kernel without it is a reported
  error, not a silent degrade.
- Maps are bounded (LRU hash, fixed histogram buckets); no unbounded per-PID state.
- If a program fails verification, `legionctl-system` keeps running, the sensor reports `failed` with the verifier
  message, and `legionctl doctor` shows it.
- Programs are pinned under `/sys/fs/bpf/legionctl/`, so a daemon that failed reuses them instead of loading again;
  a stop and the package's removal remove the pins.
