# 04 — eBPF

Kernel support measured: `/sys/kernel/btf/vmlinux`, `CONFIG_BPF_SYSCALL`, `BPF_JIT`, `BPF_LSM`, `HID_BPF`,
`SCHED_CLASS_EXT`, `KPROBES`. Programs are written with **aya** (Rust, CO-RE, no clang at runtime) in the
`legion-bpf` crate behind the `bpf` feature. Every eBPF sensor has a non-eBPF fallback.

## Worth doing

| Use | Hook | Output | Fallback |
| --- | --- | --- | --- |
| Workload detection | `sched_process_exec` / `sched_process_exit` tracepoints | `Workload` events (game, build, VM, idle) from a classifier over comm/cgroup | poll `/proc` every few seconds |
| Jank metric | `sched_wakeup` → `sched_switch` run-queue latency histogram, per CPU class (P/E core) | p50/p99 latency in the control center; A/B scx modes with numbers instead of feel | none (metric only) |
| Power attribution | `sched_switch` + `power:cpu_frequency` | estimated per-process CPU energy share ("what is keeping the fans up") | `top`-style CPU time |
| Throttle correlation | `power:cpu_frequency` drops aligned with MSR throttle-status polling | which throttle reason fired during which workload | MSR polling alone |
| Input fixes | HID-BPF on HID devices (keyboard, Logitech receiver) | in-kernel remap/debounce without a userspace daemon | logiops / evdev in legiond |

## Not doing

- **Actuation through eBPF.** Firmware attributes, sysfs and D-Bus are the right write paths; eBPF adds risk
  there for no gain.
- **A custom sched_ext scheduler.** scx_lavd with `--autopower` is maintained upstream; legiond selects
  schedulers and modes through scx_loader. Revisit only if the jank metric shows a gap no existing scheduler
  closes.
- **Ideapad Fn keys via HID-BPF.** That device is a platform/WMI input device, not HID; evdev is the hook.

## Safety

- Load programs only when the `bpf` feature is on and the kernel reports BTF; log and continue otherwise.
- Maps are bounded (LRU hash, fixed histogram buckets); no unbounded per-PID state.
- The daemon keeps running if verification fails; the sensor reports `unavailable`.
