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

### Configsets: design for review (proposed 2026-10-14, todo-74; replaces the text above when approved)

What exists now (mapped 2026-10-14): one profile JSON (`profiles/<model>.json`, `legionctl-machine/src/profile.rs`)
with `sysctl`, `ac` and `battery` (`ModePolicy`: cpu, firmware mode preference, `firmware_attributes`, nvidia, PPD),
and `firmware_follow` (what a Fn+Q mode gets: PPD and governor). `Machine::apply`/`follow` (policy.rs) plan writes
from it; `react` runs them on start, power change, resume, Fn+Q and a 30 s tick (AC). The ledger
(`/var/lib/legionctl/system.db`) keeps our last firmware write and the Fn+Q override per boot, and the user's holds
(D1) across boots. The daemon reads no layer files. The proposal keeps the planner, the triggers, the ledger and the
holds, and changes only where the desired state comes from.

1. **Format: JSONC, not TOML.** The profile and the user config are JSONC with one reader (`read_jsonc`) and
   `config explain`; a second format adds a parser and a second set of rules. (The TOML above is the 2026-10-12
   sketch.)
2. **Layers.** A layer is a file `<order>-<name>.jsonc`: `{ "when": {...}, "set": { "<knob id>": value } }`, knob
   ids as in `legionctl knobs list` (`cpu.epp`, `firmware.ppt_pl1_spl`, `sysctl.vm.swappiness`, `ppd`,
   `platform_profile` as a preference list). Folders, lowest first: `/usr/share/legionctl/configsets/<profile id>/`
   (shipped), `/etc/legionctl/configsets/` (the admin's and the user's own, written by the daemon through polkit),
   `/var/lib/legionctl/configsets/` (generated: `tuned.jsonc` from tune, off until enabled). In a folder the order
   number sorts; a later layer wins per knob.
3. **Conditions (`when`).** First: `power` (ac, battery) and `fnq` (the Fn+Q mode the user chose, which is the
   override that `follow` records now). Later, each with its sensor: `workload` (game, build, vm: the eBPF exec
   sensor), `display` (external connected), `adapter_w` (low-wattage adapter). An unknown condition is a validation
   error, so a layer cannot wait for a sensor that does not exist.
4. **Shipped layers from the profile.** The profile keeps identity, paths, DMI match, capabilities and conflicts.
   Its policy moves, once, into shipped layers: `10-base` (sysctl, shared CPU values), `20-ac`, `20-battery`,
   `30-fnq-<mode>` (from `firmware_follow`). The repository converts it once and deletes the policy part of the
   profile (no runtime converter, no second source).
5. **Holds stay holds.** A hold (D1, `knobs hold`, `undo`) pins one file and is the top of every chain ("Your
   hold"). A layer of your own is the way to change policy for a condition; a hold is the way to fix one value now.
6. **Because chain.** The merge keeps, per knob, every layer that set it (file, condition, value). `knobs show`
   prints it ("10-base: 150, 20-ac: 100 <- wins, hold: none"), `plan` prints it with the diff, and the app shows the
   chips (09-ux).
7. **Sane defaults to go back to (todo-78).** `legionctl rules list|show|enable|disable|edit|reset <layer>`: a
   shipped layer can be disabled but not changed (an edit makes an own layer that follows it); `reset` removes your
   own layers and holds for a knob or all, which leaves the shipped state. `tuned.jsonc` is never on without
   `rules enable tuned`.
8. **Validation** (`validate`, and at every load; a layer that fails is not used and the reason is a doctor
   finding): knob ids from the catalog, values within choices or the firmware's min/max, and every knob that a
   `power` layer sets is set under both power sources or in a layer without `power` (the ac/battery rule of today).
9. **Reload.** inotify on the three folders and D-Bus `Reload()`; a change runs the same `apply` as a power change.
   D-Bus also gets `Layers()` (each with its state and why it is on or off), `Plan()` (merged state, chain, diff)
   and `SetLayer(name, content)`/`RemoveLayer(name)` for `/etc` (polkit `org.legionctl.rules`).
10. **Migration.** The shipped profile is in the package, so a user cannot have changed it; holds stay in
   system.db. So nothing moves on the machine; the first daemon with configsets plans the same writes as today
   (e2e: the recorded machine gets the same plan before and after, for AC, battery, Fn+Q and resume).

Questions for the review: JSONC (1)? `/etc/legionctl/configsets/` for your own layers (2)? Conditions in the first
part only `power` and `fnq` (3)? The name `rules` for the CLI (7)?

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
| CPU power limits | the firmware modes' built-in limits (max-power on AC, balanced on battery; decided 2026-10-12). `custom` mode accepts `ppt_*` writes but runs the fans at ~6,000 rpm until a fan table is written, so it is not used yet. RAPL is read for watts, never written | safe mode, report |
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
  `RebootWindows()`.
- Signals: `StateChanged`, `Osd(kind, label, icon, level)`, `Conflict(knob, writer)`.
- polkit: `org.legion.set-override` allowed for the active local session without a password; tuning, Windows VM
  and reboot actions require admin auth.
