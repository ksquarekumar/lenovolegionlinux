# 04 — eBPF

Kernel support measured: `/sys/kernel/btf/vmlinux`, `CONFIG_BPF_SYSCALL`, `BPF_JIT`, `BPF_LSM`, `HID_BPF`,
`SCHED_CLASS_EXT`, `KPROBES`. Programs are written with **aya** (Rust, CO-RE, no clang at runtime) in the
`legionctl-bpf` crate. eBPF is the only path for these sensors: no `/proc` polling or other second source.

## Worth doing

| Use | Hook | Output |
| --- | --- | --- |
| Workload detection | `sched_process_exec` / `sched_process_exit` tracepoints | `Workload` events (game, build, VM, idle) from a classifier over comm/cgroup |
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
- Programs are pinned under `/sys/fs/bpf/legionctl/`, so a daemon restart reattaches them instead of reloading.
