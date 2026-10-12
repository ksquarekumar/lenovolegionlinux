# legionctl design notes

Working notes for redesigning this repo as `legionctl`: one operator, one API, machine and agent engines (15-layout). They are written as
the design is explored and are meant to change; decisions and open questions are marked inline.

| Note | Covers |
| --- | --- |
| [00-overview.md](00-overview.md) | Goals, constraints measured on this machine, architecture, crate layout |
| [01-core.md](01-core.md) | Event bus, configsets, reconciler, ownership ledger, plugins, redundancy and safety |
| [02-platform.md](02-platform.md) | Every hardware/OS control surface: firmware attributes, RAPL, cpufreq, PPD, sched_ext, NVIDIA, display, fans |
| [03-session.md](03-session.md) | Operator in the graphical session, animated OSD (GNOME Shell extension), GPUI app, display actions |
| [04-ebpf.md](04-ebpf.md) | Where eBPF earns its place and where it does not |
| [05-windows.md](05-windows.md) | Dual boot: safety checks, boot-to-Windows, running the installed Windows under KVM |
| [06-tuning.md](06-tuning.md) | Moving the RAPL sweep into the daemon and tuning firmware limits instead |
| [07-plan.md](07-plan.md) | Phased migration from the Python tooling, with parity gates |
| [08-kernel.md](08-kernel.md) | Kernel module and DKMS inventory, drift found, validation and repair |
| [09-ux.md](09-ux.md) | UI structure: progressive disclosure levels, views, interaction model |
| [12-operator.md](12-operator.md) | Operator and agent harness: one core and API for CLI, TUI, GPUI app, MCP and plugins; worktrees, sandboxes, ACP, plans, accounts, gateway |
| [13-daemon.md](13-daemon.md) | Operator daemon: processes, one-writer SQLite, event log, outbox and sagas, bootstrap, eBPF toolkit, shared memory, failure isolation |
| [14-sessions.md](14-sessions.md) | Sessions: COW workdirs, git state, packaged skills/MCP with call proxy and cache, sessions as model calls with quotas, pause/offload/resume, git gates, layered config |
| [15-layout.md](15-layout.md) | One name, one API, one operator: processes, operation registry, crates, paths, command tree, Python switch, test policy |
| [16-observability.md](16-observability.md) | Scope vs LenovoLegionLinux; see, check, change, record and replay tools for developers and superusers |
| [issues.md](issues.md) | Issues found on this machine and cleanups to do |

Evidence for every machine-specific claim was collected on 2026-10-12 (CachyOS, kernel 7.2.9, GNOME 51,
BIOS KWCN54WW, EC 1.51).
