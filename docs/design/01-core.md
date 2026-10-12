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
    Display(DisplayState),               // from legion-agent: outputs, active GPU, refresh
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
/usr/share/legion/configsets/   shipped defaults (from profiles/)
/etc/legion/configsets/         machine admin overrides
/var/lib/legion/configsets/     generated (e.g. tuning results), never hand-edited
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
| `enforce` | Restore our value; log who changed it | sysctl, governor, RAPL fallback |
| `follow` | Accept a firmware/user change as an override until the next condition change | platform_profile (Fn+Q) |
| `observe` | Never write; report only | fan RPM, temperatures, `conservation_mode` unless configured |

## Ownership ledger

Generalises today's `/run/legion-control/firmware-written`: for every knob the reconciler records the last value
it wrote and when. On a change event:

- observed == last written → our own write, ignore;
- otherwise → external change, handled per knob policy, and attributed (firmware event, D-Bus caller, unknown).

The ledger lives in memory and is mirrored to `/run/legion/ledger.json` for debugging and crash recovery.

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

- Built-in plugins are compiled in and registered at startup; optional ones sit behind cargo features.
- External plugins: an executable speaking JSON lines over stdin/stdout (`{"read": knob}` → `{"value": ...}`),
  declared in `/etc/legion/plugins.d/*.toml`. This lets the existing Python pieces be wrapped during migration.
- No dynamic `.so` loading: Rust has no stable ABI and the safety story is worse.

## Redundancy and failure handling

| Concern | Primary | Fallback |
| --- | --- | --- |
| CPU power limits | firmware attributes (`ppt_*`) | RAPL MSR + MMIO |
| Change detection | sysfs `POLLPRI` / uevents | poll in the drift check |
| GPU data | NVML | `nvidia-smi` parsing |
| Fn keys | evdev on the platform device | hwdb remap + session shortcut |
| OSD | GNOME Shell extension | desktop notifications |
| Fan RPM | validated EC/WMI source | none (marked unknown, never the thinkpad value) |

- `Type=notify` with `WatchdogSec=30`; a hung reconciler gets restarted.
- `OnFailure=legion-safe.service`: writes firmware defaults (platform_profile `balanced`, firmware limits back to
  their boot values captured at first start) so a crash never leaves aggressive limits on battery.
- Every write is refused unless DMI matches the profile (today's `require_supported_machine`).
- Structured audit log to journald: knob, old, new, reason, condition set.
- `--dry-run` for the whole daemon: runs sensors and planning, logs intended writes, writes nothing.

## D-Bus API (`org.legion.Control1`, system bus)

- Properties: `PowerSource`, `FirmwareMode`, `Override`, `Conditions`, `DesiredState`, `ObservedState`.
- Methods: `SetOverride(mode)`, `ClearOverride()`, `Plan() → diff`, `Apply()`, `Reload()`, `StartTune(spec)`,
  `RebootToWindows()`.
- Signals: `StateChanged`, `Osd(kind, label, icon, level)`, `Conflict(knob, writer)`.
- polkit: `org.legion.set-override` allowed for the active local session without a password; tuning, Windows VM
  and reboot actions require admin auth.
