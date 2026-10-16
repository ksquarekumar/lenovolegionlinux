# 01 — Core: events, configsets, reconciler

## Event bus

A single `tokio::sync::broadcast` channel carries typed events:

```rust
enum Event {
    PowerSource(Source),                 // ADP0 online/offline (udev power_supply)
    FirmwareMode { value: String },      // platform_profile changed (uevent / sysfs notify)
    Sleep { entering: bool },            // logind PrepareForSleep
    Key(FnKey),                          // Fn+Q / Fn+R / ... from evdev, debounced at the source
    Workload(WorkloadChange),            // eBPF exec/exit classification (built: Event::Workload)
    Display(DisplayState),               // from legionctl-operator: outputs, active GPU, refresh
    Thermal(ThermalSample),              // periodic sensor snapshot
    ConfigReloaded,                      // D-Bus Reload / inotify on the configset folders (built: Event::Reload)
    Drift { knob: KnobId, observed: String },
    Tick,                                // slow safety-net drift check (default 120 s)
}
```

Sensors publish; the policy task consumes. Debounce and coalescing happen at the sensor (Fn+R bursts) and in
the policy task (a 300 ms settle window before reconciling, so a burst of uevents yields one pass).

## Configsets

Built 2026-10-14 (todo-74). The desired state is a merge of JSONC layers. The conditions of the machine now select
the layers. A later layer wins per knob. The user's holds (D1) stay on top of the merge.

### Layers and folders

A layer is a file `<two digits>-<name>.jsonc`: `{ "when": {...}, "set": { "<knob id>": value } }`. The knob ids are
the ids of `legionctl knobs list` (`cpu.epp`, `firmware.ppt_pl1_spl`, `sysctl.vm.swappiness`, `ppd`,
`platform_profile` as a preference list, and the actions `cpu.idle_states` and `nvidia.services`). The folders, lowest
first:

| Folder | Who writes it | Notes |
| --- | --- | --- |
| `/usr/share/legionctl/configsets/<profile id>/` | the package (`configsets/<profile id>/` in the repository) | `rules disable` turns a layer off; nobody changes it |
| `/var/lib/legionctl/configsets/` | legionctl (`50-tuned` from `legionctl tune`) | off until `legionctl rules enable tuned` |
| `/etc/legionctl/configsets/` | the admin, or the daemon for `rules edit --system` (polkit `org.legionctl.rules`) | can list knobs in `"locked"` |
| `~/.config/legionctl/configsets/` | the user, or `rules edit` | only the user of the active local session (logind seat0) |

In a folder, the order number sorts. The daemon reads the user's folder as that user's files: the folder and each
file must belong to that user, a link is never followed (`O_NOFOLLOW` on the folder and on each file), and a file
above 64 KiB is not read. A user layer cannot set a knob that a used `/etc` layer locks.

The shipped layers are the policy that was in the profile before (`10-base`, `20-ac`, `20-battery`,
`30-fnq-<mode>` for each Fn+Q mode). The profile keeps identity, DMI match, paths, capabilities, conflicts, the fan
block and the NVIDIA power limit that `doctor` expects. There is no converter and no second source.

### Conditions

- `power`: `ac` or `battery`.
- `fnq`: the firmware mode that the user chose with Fn+Q. The ledger keeps it as the override until the power
  source changes, the next start, or `power apply`. While the policy follows a choice, the firmware mode and EPP
  stay as the firmware and power-profiles-daemon set them, as before. A mode without an `fnq` layer leaves the CPU
  policy alone.
- `workload`: `game`, `build` or `vm`, from the eBPF workload sensor (note 04). The daemon writes the active kinds
  to `/run/legionctl/workload`, so the daemon and each planner of the operator (`plan`, `knobs`, dry runs) see the
  same conditions. When the sensor cannot load, the daemon writes the reason to `/run/legionctl/workload.error`,
  and `doctor` shows it.
- `kernel`: the package of the running kernel (`linux-cachyos`, `linux-cachyos-bore-lto`), from
  `/usr/lib/modules/<release>/pkgbase` of the release in `/proc/sys/kernel/osrelease` (todo-84). A layer for a
  kernel that is not installed is a check fault (a `doctor` finding); `validate` cannot see the installed kernels and
  checks only that the value is not empty.

An unknown condition is a check fault, so a layer cannot wait for a sensor that does not exist.

### Checks

The daemon checks each layer when it reads the layers (each event), and `legionctl validate` checks the shipped
layers without the machine. A layer that fails is not used; `doctor` shows the file and the reason. The checks:

- the name, the conditions and their values;
- knob ids of the catalog; values within the knob's choices, or the firmware's min and max (when the machine
  publishes them);
- `locked` only in `/etc`; a user layer that sets a locked knob fails;
- each knob that a `power` layer sets is also set for the other power source, or in a layer without `power`. If
  not, a change of the power source leaves the value of the other source behind. The rules between layers repeat
  until no more layer drops out.

### Because chain and plan

The merge keeps each layer that set a knob: the layer, its conditions and its value. `legionctl knobs show <knob>`
shows the chain (`shipped/10-base (always): 150`, `user/50-mine (always): 60  <- wins`, then the hold).
`legionctl plan` shows the conditions, each knob a layer sets with its chain, and the writes the daemon would make
now. `legionctl rules list|show` show the layers, their state and which apply now.

### Reload

inotify watches the shipped folder, the generated folder, `/etc` (the daemon makes the last two when they are not
there), the user's folder (looked up again every 30 s and after each change, because the user and the folder can
change) and `/run/systemd/seats` (another user in front). A change runs `Event::Reload` once the folders stayed
unchanged for 1 s (todo-125: a package upgrade replaces the files one by one). `Event::Workload` comes from
the sensor. Both run the same apply as after resume: the full policy, and the Fn+Q choice stays (a layer edit or a
build is no reason to drop the user's choice; the approved text said "the same apply as a power change", which
clears the choice).

### D-Bus and CLI

- `Reload()`: no polkit (it applies the configured policy). `rules edit` and `rules reset` call it after they change
  the user's own files.
- `SetLayer(name, content)`: writes `/etc/legionctl/configsets/<name>.jsonc` (empty content removes it); polkit
  `org.legionctl.rules`. A layer that fails its own checks is refused.
- `SetLayerState(name, state)`: `on`, `off`, or `default` (generated off, the others on); kept in `system.db`
  (`layers`, across boots); polkit `org.legionctl.rules`.
- `legionctl rules list|show|enable|disable|edit|reset` and `legionctl plan`. `rules edit <layer> knob=value ...
  [--when power=ac] [--system]`: a shipped layer's name makes your layer of the same name with the same conditions,
  after it. `knob=` removes a knob; a layer that sets nothing goes. `rules reset [--knob K]` removes your layers (or
  the knob from them) and your holds; with `--system`, the `/etc` layers (or the knob from them) and the on/off
  states. After an edit, `rules edit` shows when a rule between layers keeps the layer out.

`Layers()` and `Plan()` on D-Bus (point 9 of the design) are not built: each planner reads the same files (the
workload too), so the operator gives the same answer without a second path.

### Saved sets

Built 2026-10-15 (todo-85, note 22). A saved set is a named copy of the layer states and the holds, in the table
`sets` of `system.db`. `legionctl rules sets save <name> [--boot]`, `rules sets switch <name> [--dry-run]` (one
change; `switch previous` is the undo) and `rules sets remove <name>` go through the daemon's `Sets(action, name,
boot)` (polkit `org.legionctl.rules`). With `--boot`, each kernel's image gets a hidden boot entry for the set, and
the daemon applies `legionctl.set=<name>` from the kernel line for that start only.

### Migration

The shipped profile is in the package, and holds stay in `system.db`, so nothing moves on the machine. The proof:
`tests/fixtures/plans-82wq.txt` holds each step that the planner makes on the recorded machine: as recorded and with
each policy file scrambled, on AC and on battery, with no Fn+Q choice and with five kinds of choice, for start, power
change, resume, tick and Fn+Q, for `power apply` of each source, and each knob's wanted value. The planner of the
profile wrote it first (the commit before configsets); `configsets_plan_exactly_the_writes_the_profile_planned`
plans the same states with the layers and compares each step. A plan change that is meant writes the file again
(`LEGIONCTL_WRITE_PLANS=1`), and its diff is read before the commit.
The `daemon_events` tests pass with unchanged expectations.

### Open

- `tuned` sets firmware limits for AC only (the sweep runs on AC). The power rule keeps it out until a layer gives
  the same knobs for battery (`rules edit 55-tuned-battery --when power=battery firmware.ppt_pl1_spl=...`).
- The sensor classifies by the program's name only, not by its cgroup (`shortcut:` in `workload.rs`).

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
