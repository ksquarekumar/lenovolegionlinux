# 01 — Core: events, configsets, reconciler

## Event bus

A single `tokio::sync::broadcast` channel carries typed events:

```rust
enum Event {
    PowerSource(Source),                 // ADP0 online/offline (udev power_supply)
    FirmwareMode { value: String },      // platform_profile changed (uevent / sysfs notify)
    Sleep { entering: bool },            // logind PrepareForSleep
    Key(FnKey),                          // Fn+Q / Fn+R / ... from evdev, debounced at the source
    Workload(WorkloadChange),            // eBPF exec classification (optional)
    Display(DisplayState),               // from legionctl-operator: outputs, active GPU, refresh
    Thermal(ThermalSample),              // periodic sensor snapshot
    ConfigReloaded,                      // SIGHUP / D-Bus Reload / inotify on /etc/legion
    Drift { knob: KnobId, observed: String },
    Tick,                                // slow safety-net drift check (default 120 s)
}
```

Sensors publish; the policy task consumes. Debounce and coalescing happen at the sensor (Fn+R bursts) and in
the policy task (a 300 ms settle window before reconciling, so a burst of uevents yields one pass).

## Configsets

Desired state is a merge of TOML layers selected by the current *conditions*. Later layers win per knob.

```
/usr/share/legionctl/configsets/   shipped defaults (from profiles/)
/etc/legionctl/configsets/         machine admin overrides
/var/lib/legionctl/configsets/     generated (e.g. tuning results), never hand-edited
```

Example:

```toml
[layer.base]
sysctl."net.ipv4.tcp_congestion_control" = "bbr"
sysctl."net.core.default_qdisc" = "fq"
sysctl."vm.swappiness" = "150"
scx = { scheduler = "scx_lavd", mode = "auto" }

[layer.ac]
when = { power = "ac" }
firmware.platform_profile = "max-power"
firmware.ppt_pl1_spl = 90
firmware.ppt_pl2_sppt = 135
firmware.ppt_cpu_cl = 75
cpu = { governor = "performance", epp = "performance" }
ppd = "performance"
display.internal_refresh = "max"

[layer.battery]
when = { power = "battery" }
firmware.platform_profile = "balanced"
cpu = { governor = "powersave", epp = "balance_power" }
ppd = "balanced"
display.internal_refresh = 60

[layer.fnq-balanced]
when = { firmware_override = "balanced" }      # user pressed Fn+Q
cpu.governor = "powersave"
ppd = "balanced"

[layer.gaming]
when = { workload = "game" }                   # eBPF exec classification
scx.mode = "gaming"
```

`legionctl plan` prints the merged desired state and the diff against observed state (like `terraform plan`).
`legionctl validate` checks schema, value bounds (from firmware `min_value`/`max_value` when online), and that
every knob set in one power layer is set in the other (the rule `legionctl validate` enforces today).

## Knob policies

Each knob declares how to treat an external change:

| Policy | Meaning | Examples |
| --- | --- | --- |
| `enforce` | Restore our value; log who changed it | sysctl, governor, firmware limits |
| `follow` | Accept a firmware/user change as an override until the next condition change | platform_profile (Fn+Q) |
| `observe` | Never write; report only | fan RPM, temperatures, `conservation_mode` unless configured |

## Ownership ledger

Generalises today's `/run/legionctl/firmware-written`: for every knob the reconciler records the last value
it wrote and when. On a change event:

- observed == last written → our own write, ignore;
- otherwise → external change, handled per knob policy, and attributed (firmware event, D-Bus caller, unknown).

The ledger lives in memory and is mirrored to `/run/legionctl/ledger.json` for debugging and crash recovery.

## Reconciler

1. Compute active conditions (power source, override, workload, display, sleep).
2. Merge layers → desired state; validate.
3. Read observed state for each knob (actuator `read`).
4. Plan = knobs where observed ≠ desired.
5. Apply in dependency order (firmware mode before firmware limits before CPU policy before PPD/scx), each write
   verified by read-back, bounded, and rate-limited per knob (default ≥ 2 s between writes of the same knob, so a
   fight with another writer shows up as a logged conflict instead of a loop).
6. Emit `StateChanged` and, where relevant, an OSD request.

## Plugins

```rust
trait Sensor: Send { fn id(&self) -> &str; async fn run(self: Box<Self>, bus: Sender<Event>) -> Result<()>; }
trait Actuator: Send + Sync {
    fn knobs(&self) -> &[KnobId];
    fn read(&self, knob: &KnobId) -> Result<Value>;
    fn write(&self, knob: &KnobId, value: &Value) -> Result<()>;
    fn bounds(&self, knob: &KnobId) -> Option<Bounds>;
}
```

- All sensors and actuators are compiled in and registered at startup.
- No external process plugins and no dynamic `.so` loading: one implementation for each knob, in Rust.

## One path for each concern, and failure handling

| Concern | Path | When it fails |
| --- | --- | --- |
| CPU power limits | firmware attributes (`ppt_*`, `gpu_*`) in `custom` mode, the only mode where the firmware accepts them (decided 2026-10-12); RAPL is read for watts, never written | safe mode, report |
| Change detection | sysfs `POLLPRI` / uevents; drift check only for knobs that cannot notify | report the knob |
| GPU data | NVML | sensor `failed`, report |
| Fn keys | evdev on the platform device | report; no key actions |
| OSD | GNOME Shell extension | `doctor` reports it; no OSD |
| Fan RPM | validated WMI source | marked unknown, never the thinkpad value |

- `Type=notify` with `WatchdogSec=30`; a hung reconciler gets restarted.
- `OnFailure=legionctl-safe.service`: writes firmware defaults (platform_profile `balanced`, firmware limits back to
  their boot values captured at first start) so a crash never leaves aggressive limits on battery.
- Every write is refused unless DMI matches the profile (today's `require_supported_machine`).
- Structured audit log to journald: knob, old, new, reason, condition set.
- `--dry-run` for the whole daemon: runs sensors and planning, logs intended writes, writes nothing.

## D-Bus API (`org.legionctl.System1`, system bus)

- Properties: `PowerSource`, `FirmwareMode`, `Override`, `Conditions`, `DesiredState`, `ObservedState`.
- Methods: `SetOverride(mode)`, `ClearOverride()`, `Plan() → diff`, `Apply()`, `Reload()`, `StartTune(spec)`,
  `RebootToWindows()`.
- Signals: `StateChanged`, `Osd(kind, label, icon, level)`, `Conflict(knob, writer)`.
- polkit: `org.legion.set-override` allowed for the active local session without a password; tuning, Windows VM
  and reboot actions require admin auth.
