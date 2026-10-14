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
| `legionctl knobs list` | catalog of every knob: path, type, firmware min/max, current value, writable, policy (enforce / follow / observe) |
| `legionctl knobs show <knob>` | value, desired value, the configset layer that set it ("because" chain), last writer from the ledger (legionctl, firmware, another process), write history |
| `legionctl trace` | live stream of the event log (`GET /v1/events`, server-sent events with Durable Streams semantics: `offset=<id>` or `now`, catch-up then live, `Last-Event-ID` to go on; for the user token only): machine daemon changes, Fn keys, operator audit, agent tool calls |
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

Built (2026-10-13, M3b): `knobs` lists each knob that legionctl reads or sets: the platform profile, the CPU knobs
(a per-CPU knob shows one value, or `value×count` when the CPUs differ), power-profiles-daemon, the profile's
sysctls and the `lenovo-wmi-other` firmware attributes with their firmware range. For each knob it shows the current
value, the value the profile wants for the current power source (the Fn+Q choice when one is active) and the
policy: `enforce` (the profile sets it), `follow` (the platform profile: an Fn+Q choice is accepted) or `observe`.
`inspect <knob>` shows one knob and the events that name its file (the daemon's writes and warnings). The
"because" chain comes with the configset layers (M3e).

Built (2026-10-15, todo-94): `legionctl sensors [--watch] [--json]` shows every sensor in one table, with its unit
and its source. It takes one telemetry sample (the same `sample()` as `monitor`) and reads nothing of its own:

| Source | Rows |
| --- | --- |
| `hwmonN <name>` | Each channel of each `/sys/class/hwmon` device: `temp*` (°C), `fan*` (RPM), `power*` (W), `in*` (V), `curr*` (A), `freq*` (MHz); the label, else the channel name. A device whose `device/power/runtime_status` is not `active` (or `unsupported`) is not read, because a read wakes it: the row says `asleep`. |
| `firmware acpi_call` | Fan RPM from `legionctl-system`. When the `legionctl-wmi` module is loaded, its hwmon device `legionctl_wmi` (`fan1_input`, `fan2_input`) is the fan source: the daemon reads it in place of `acpi_call`, and the view shows its hwmon rows only. No code change is necessary. |
| `nvidia-smi`, `PCI runtime PM` | The NVIDIA GPU's power, temperature, utilization and clock only while it is `active`; else only its runtime state. legionctl never wakes it. |
| `RAPL`, `cpufreq`, `PSI`, `eBPF runq` | Package power, mean CPU clock, pressure, run-queue latency p99 per CPU class. |
| `MSR`, `legionctl-system`, `eBPF workload`, `power_supply` | Throttle reasons now and since boot; the firmware mode, fan max mode and the fan table; the workloads that run; the power source and the battery. |

History: the operator keeps a telemetry ring of the last 600 samples that `monitor`, `sensors` and `/v1/metrics`
take. Each number has its values from the ring (`history`, oldest first); the text shows their range. The ring
fills only while something takes samples (for example `sensors --watch`, the app, or Prometheus). `--watch` shows the
view again each second (with `--json`, one line per view). `/v1/metrics` has each hwmon channel as
`legionctl_hwmon_<celsius|rpm|watts|volts|amperes|mhz>{device,sensor}` and each active throttle reason as
`legionctl_throttle_active{reason}`. The TUI shows the view with `s`; the app shows it on the Now page.

## For other tools: system events

Built (2026-10-15, todo-95): `legionctl-system` compares the state that other tools follow with the last state it
announced. It does this after each event (start, power source, Fn+Q, resume, the 30 s tick, reload, workload) and
after each change that sends `Changed`. Each difference gives one D-Bus signal on `org.legionctl.System1` and one
structured journal entry (through the journal's native socket, `/run/systemd/journal/socket`):

| What changed | D-Bus signal | `LEGIONCTL_EVENT` | Fields |
| --- | --- | --- | --- |
| Firmware mode (platform profile, Fn+Q) | `ModeChanged(mode s, fn_q_choice b, source s)` | `mode` | `LEGIONCTL_MODE`, `LEGIONCTL_FN_Q_CHOICE` (1 or 0), `LEGIONCTL_SOURCE` |
| Power source | `PowerSourceChanged(source s)` | `power-source` | `LEGIONCTL_SOURCE` (`ac`, `battery`) |
| Workloads that run (eBPF) | `WorkloadChanged(kinds as)` | `workload` | `LEGIONCTL_WORKLOAD` (`build,game`, or `none`) |
| Throttling starts, changes or stops (MSR) | `ThrottleChanged(reasons as)`; empty: stopped | `throttle` | `LEGIONCTL_REASONS` (empty: stopped) |
| Fan max mode | `FanModeChanged(max b)` | `fan-mode` | `LEGIONCTL_FAN_MAX` (`on`, `off`) |

Each entry has `SYSLOG_IDENTIFIER=legionctl-system`, `PRIORITY=6` and a `MESSAGE` (for example `power source
battery`). To follow them:

```sh
journalctl -f -o json -t legionctl-system LEGIONCTL_EVENT=power-source
busctl monitor --system --match "type='signal',interface='org.legionctl.System1'"
```

Limits: throttling is compared at the events above, so a throttle period shorter than the 30 s tick between two
events is not seen. Full fan speed (`fan clean`) has no state that the daemon can read, so it has no event.

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

Built (2026-10-13, M3b): besides the policy, kernel, conflict, NTFS and GPU checks, `doctor` now finds failed
system and user units, the OSD extension when it is not active, setup gaps, dossier bindings idle past
`dossier.stale_hours`, and workspaces that drifted from their record or have conflicts. The findings are still
lines; checks as data with fix operations come with `approve` (M3g).

## Change: tune and experiment safely

- `legionctl plan` shows the exact writes a change makes; `apply` refuses values outside firmware min/max.
- `legionctl undo <event-id>` applies the recorded inverse of a change from the event log.
  Built (2026-10-14, D1 decided 2026-10-13: a user override layer): each write the daemon does is the line
  `wrote <path>: <new> (was <old>)`; `legionctl undo <event-id>` holds each written file at what it replaced
  (`Hold` on D-Bus, polkit `org.legionctl.knobs.hold`), and the daemon's plans leave held files alone (a plan says
  `<path> is held at ...`) until `legionctl knobs release <knob>`, which gives them back to the policy of the current
  power source. `knobs hold <knob> <value>` holds one directly (within its choices or range). Only knobs and files the
  policy itself writes can be held: the daemon writes them as root. Holds live in `system.db` across boots;
  `knobs list` and `knobs show` (were `knobs` and `inspect`) show them.
- `legionctl bench <scenario>`: repeatable workloads (CPU, GPU with gpu-burn, mixed) with telemetry captured and
  stored; `bench compare` shows two runs side by side (for example two scx modes, two power limits).
  Built (2026-10-13, M3d): `bench run cpu|gpu|mixed --seconds N [--label L]` runs stress-ng
  (`--cpu 0 --metrics-brief`), gpu-burn, or both, takes a telemetry sample each second, and stores one
  `bench.result` event: the scores (stress-ng bogo ops/s in real time, gpu-burn Gflop/s from the last progress
  line), a summary (package W mean/max, CPU °C max, CPU MHz mean, GPU W mean, GPU °C max, fan rpm max, throttle
  reasons) and every knob value at the start (with the `sched_ext` scheduler). Failed stressors and gpu-burn
  computation errors are problems: the command exits 1. `bench list` and `bench compare --a A --b B` (each number
  with its change, then the knobs that differ) read the event log.
- `legionctl power tune`: the sweep engine (06-tuning) writes a generated configset layer for review.
- `legionctl wmi`: catalog of the firmware WMI methods this model has (10-gap), each marked read-only or write;
  read-only methods run directly, write methods only through an operation with approval. Parked: the measured
  method table is in 10-gap, but the exact `acpi_call` argument strings for WMAA/WMAF were not recorded; they must
  be verified with the user before legionctl calls them.
- Machine events can be bound to actions with the same hooks manager as agent events: built-in actions or a
  user shell command with a timeout. That is the extension point; there are no plugins.

## Record and replay

`legionctl record <name>` captures a sysroot snapshot of the knobs and the event stream for a time window into
`tests/fixtures/sysroot-82wq-<name>/`. The same recording is:

- a bug report (attach it; `legionctl replay` shows the decisions the daemon made),
- an end-to-end test fixture (07-plan, 15-layout test policy),
- a before/after record of a BIOS or kernel update.

Built (2026-10-13, M3c): `legionctl record <new dir>` runs `status`, `doctor`, `knobs` and the `power apply`
plan through a traced sysroot, and writes each path that a read found (files, directories, the driver links) under
`<dir>/root`, with the installed profile and the Fn+Q ledger. It also writes the last `--minutes` (default 10) of
machine events to `<dir>/events.json`: the daemon's signals and the machine operations only, never agent payloads,
because a recording is shared. `legionctl replay <dir>` gives the plan, findings and knobs on the recording, and
`LEGIONCTL_SYSROOT=<dir>/root` runs every read operation on it. The e2e test requires that a replay gives the same
decisions and `status` as the live machine, so a read that bypasses the sysroot fails it. Limits: the results of
commands (systemctl, nvidia-smi, journalctl) and the root-only values (MSR, fan RPM) are not in the tree; a replay
runs those commands again where it runs. A recording does not overwrite a directory that has files.
