# 16 — Scope, observability and hackability

## Scope

`legionctl` replaces LenovoLegionLinux (LLL) on this machine and goes further:

| Area | LLL | legionctl |
| --- | --- | --- |
| Machine control | out-of-tree `legion_laptop` module, EC access, Python GUI/CLI | in-tree drivers (`lenovo_wmi_*`, `ideapad_laptop`), WMI through `acpi_call`, one daemon (`legionctl-system`); feature map in 10-gap |
| Policy | presets applied on AC/battery | layered configsets, reconciler, ownership ledger, events (01-core) |
| Agents | none | operator: sessions, tools, skills, MCP, model gateway, Claude/Codex accounts and quotas (12, 14) |
| Developers and superusers | sysfs files and a GUI | the tools below |

One rule for the tools below: each is an operation in the registry (15-layout), so it works from the CLI, TUI,
app and MCP. An agent can diagnose the machine with the same tools as a person.

## See: what is the machine doing, and why

| Operation | Shows |
| --- | --- |
| `legionctl knobs` | catalog of every knob: path, type, firmware min/max, current value, writable, policy (enforce / follow / observe) |
| `legionctl inspect <knob>` | value, desired value, the configset layer that set it ("because" chain), last writer from the ledger (legionctl, firmware, another process), write history |
| `legionctl trace [--filter]` | live stream: uevents, D-Bus signals, Fn keys, ledger decisions, writes, operator events, agent tool calls |
| `legionctl monitor` | telemetry ring: package/GPU power, temperatures, clocks, fan RPM, PSI, throttle reasons, run-queue latency (1–10 Hz) |
| `legionctl events [--since]` | the event log of both engines, with actor and payload |
| `GET /v1/metrics` | the same telemetry in OpenMetrics format, for Prometheus or Grafana |

Built (2026-10-13, M3a): `monitor` (package power from RAPL energy over the sample window, CPU temperature and
clock, the NVIDIA GPU only while it is awake, PSI, and fan RPM and throttle reasons now/logged from
`legionctl-system`'s `Sample()` method, which reads the MSR and the firmware as root), `GET /v1/metrics` (the same
sample in OpenMetrics), `events` (the operator's event log, which now also records the daemon's `Changed`,
`ModeChanged`, `FnLock` and `Key` signals as actor `legionctl-system`), and `trace` (follows the event log). The
recorder runs while the operator runs: from login (or the first client request); machine changes before that are in
the daemon's journal only.

## Check: is the machine correct

`legionctl doctor` runs checks that are data: id, severity, evidence, and a fix operation with a preview diff.
Fixes are applied only through `legionctl approve`.

| Check group | Detects |
| --- | --- |
| Config drift | a knob differs from its desired value; who changed it |
| Power | firmware limits vs RAPL disagreement, throttle reasons at moderate temperature, PL1/PL2 out of firmware range |
| Kernel | module inventory and autoload drift, DKMS not built for an installed kernel, initramfs contents and size, Secure Boot signature (08-kernel) |
| Boot | boot time regression vs the recorded baseline (`systemd-analyze`), failed units, journal error bursts since boot |
| Graphics | primary GPU vs the outputs in use, desktop GPU preference, NVIDIA module and driver version match |
| Load | phantom I/O pressure (iowait with idle disks, named by cgroup), memory pressure |
| Session | OSD extension not enabled, Mutter refresh state vs the layer |
| Virtualization | provider health, guest memory, Docker disk use, NVIDIA runtime vs driver, KVM/IOMMU/VFIO, binfmt (17-virtualization) |
| Agents | setup gaps (ctx, MCP, skills), invalid agent settings, quota near limit, gate denials, stale worktrees |

## Change: tune and experiment safely

- `legionctl plan` shows the exact writes a change makes; `apply` refuses values outside firmware min/max.
- `legionctl undo <event-id>` applies the recorded inverse of a change from the event log.
- `legionctl bench <scenario>`: repeatable workloads (CPU, GPU with gpu-burn, mixed) with telemetry captured and
  stored; `bench compare` shows two runs side by side (for example two scx modes, two power limits).
- `legionctl power tune`: the sweep engine (06-tuning) writes a generated configset layer for review.
- `legionctl wmi`: catalog of the firmware WMI methods this model has (10-gap), each marked read-only or write;
  read-only methods run directly, write methods only through an operation with approval.
- Machine events can be bound to actions with the same hooks manager as agent events: built-in actions or a
  user shell command with a timeout. That is the extension point; there are no plugins.

## Record and replay

`legionctl record <name>` captures a sysroot snapshot of the knobs and the event stream for a time window into
`tests/fixtures/sysroot-82wq-<name>/`. The same recording is:

- a bug report (attach it; `legionctl replay` shows the decisions the daemon made),
- an end-to-end test fixture (07-plan, 15-layout test policy),
- a before/after record of a BIOS or kernel update.
