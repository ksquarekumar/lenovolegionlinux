# Baseline Vs Reference Repositories

This is the explicit comparison between the current local baseline and the
three reference repositories.

Reference revisions used for this pass:

| Reference | Local checkout | Revision |
| --- | --- | --- |
| johnfanv2/LenovoLegionLinux | `/tmp/lll-johnfan` | `7c19579` |
| null-dev/lenovo-legion-wmi-dkms | `/tmp/lll-null-dev` | `c25ebd4` |
| bolekjar/LenovoLegion | `/tmp/lll-bolekjar` | `7e3de9f` |

## How To Read This

The reference repos are good capability catalogs. They are not all good direct
install targets for this 82WQ machine.

- johnfan is the broadest historical userspace/EC feature catalog, but its old
  daemon and DKMS path are not the baseline here.
- null-dev is a narrow WMI hybrid-graphics reference.
- Bolekjar is the best newer DKMS source for WMI/hwmon/event surfaces, but its
  packaged install is too broad because it blacklists or replaces kernel pieces
  that are currently working here.

## Capability Matrix

| Capability | johnfanv2 | null-dev | Bolekjar | Local baseline | Decision |
| --- | --- | --- | --- | --- | --- |
| Machine identity/model matching | Broad Legion support and model notes | One tested model | Compatibility layer in DKMS | Exact stable 82WQ/16IRX8H DMI match | Keep local profile guard. BIOS/EC stay status-only. |
| AC/battery policy orchestration | `legiond` profiles and power-profile integration | none | Daemon/GUI profile management | `legionctl` + systemd/udev | Keep local owner; do not reintroduce old daemons. |
| Platform profile / power mode | quiet/balanced/performance/custom notes; Fn+Q documented | none | quiet/balanced/performance/extreme/custom and platform profile driver | Existing in-tree `platform_profile`, AC target `max-power` | Keep current owner; add event sync after capture. |
| Fn+Q | Documented as hardware power-mode switch | none | WMI thermal-mode event support; UI has custom Fn+Q mode | Not consumed yet | Capture event first, then wire `legionctl` reapply/reassert behavior. |
| CPU RAPL/power limits | Userspace profile concept | none | RAPL, RAPL-MMIO, MSR helpers | Standard `intel_rapl`/powercap at `70/125/125` | Keep standard kernel interface. Do not import RAPL/MSR helpers by default. |
| NVIDIA/GPU power | Notes around NVIDIA power behavior | none | GPU power monitoring/control in broader app | `nvidia-powerd` + persistence, observed 150 W | Keep firmware-managed Dynamic Boost. No forced `nvidia-smi -pl`. |
| Fan RPM and temperature telemetry | `legion_hwmon`, EC fan/temp discussion | none | `legion-hwmon` with CPU/GPU/SYS temp and fan RPM | Missing; `thinkpad_acpi` ignored | First DKMS target, read-only only. |
| Fan curves | Rich fan curve model, presets, profiles | none | fan table/method sysfs and GUI fan curve | Not owned | Deferred until fan RPM is trustworthy. |
| Fan max/full-speed/dust mode | Explicit max fan and fan lock/unlock concepts | none | fan method support | Not owned | Later explicit command with timeout/failsafe. |
| VPC2004 `fan_mode` | Not the reliable surface for this machine | none | package replaces platform stack as part of DKMS | Visible but ineffective; write `4` reads back `0` | Status-only; not a control path. |
| WMI events | Older ACPI event sketches | none | thermal, charge, smart-fan, keylock, utility WMI events | Not consumed | Import event observation after coexistence test. |
| Fn+R refresh-rate toggle | ACPI event/action sketch, but marked unfinished and hardcoded | none | no direct userspace action found | Not owned | Capture event and panel modes, then implement userspace action. |
| Fn+Space keyboard lighting/RGB | Keyboard backlight notes, points to external RGB for 4-zone on older models | none | keyboard feature/light WMI methods plus RGB controller stack | Not verified | Capture event/controller path first. |
| Fn+Esc Fn-lock | Documented Fn-lock control | none | keylock WMI event source exists | Not verified | Optional status/control later. |
| Win-key lock | GUI feature | none | WMI methods for disable Win key | Not owned | Optional explicit command later. |
| Touchpad toggle | GUI feature | none | WMI methods for touchpad enable/disable | Not owned | Optional explicit command later. |
| Battery conservation / rapid charge | Documented GUI controls | none | charge-mode WMI events and power-charge method | Not owned | Optional battery-health command; separate from performance policy. |
| Always-on USB / camera power | Documented GUI feature set | none | not primary DKMS target | Not owned | Low priority unless sysfs/WMI surface is confirmed. |
| Hybrid graphics / MUX | Hybrid/G-Sync notes | Main feature via `kernel.lenovo_legion_wmi.hybrid_graphics` | iGPU-mode and G-Sync WMI methods | Not owned | Later explicit utility command, never automatic AC policy. |
| G-Sync | Feature/method notes | none | WMI G-Sync status/set methods | Not owned | Optional graphics utility command. |
| Display overdrive | Documented GUI feature | none | WMI overdrive status/set methods | Not owned | Optional panel utility command after support check. |
| Keyboard/RGB/Y-logo/IO LEDs | Backlight and LED notes | none | RGB controller, keyboard light methods, light owner methods | Not owned | Discover first; no GUI/daemon import in baseline. |
| CPU frequency/core/SMT controls | Profile/CPU tuning concepts | none | frequency control, CPU control, MSR helpers | Governor/EPP only | Keep minimal. Do not add core/SMT/MSR tuning until needed. |
| Firmware attributes/capdata | Older EC/ACPI mapping | none | firmware attributes, capdata00/capdata01, ddata, machine info | Not owned | Useful for discoverability in DKMS phase, read-only first. |
| GUI/tray | PyQt GUI | none | Qt GUI plus root daemon | Not owned | Defer. CLI and stable services are the local owner. |

## Baseline Gaps By Priority

1. Real fan RPM/temperature telemetry: import the minimum Bolekjar read-only
   hwmon/WMI path needed to replace bogus `thinkpad_acpi` output.
2. WMI event observation: capture and expose Fn+Q/thermal, charge, utility, and
   keylock events without changing policy.
3. Fn+R: implement refresh-rate switching only after local event and display
   mode capture.
4. Fn+Space: determine whether the path is WMI, HID/RGB, input-only, or
   firmware-only on this 82WQ machine.
5. Graphics utilities: hybrid/iGPU/G-Sync/overdrive as explicit commands only.
6. Convenience controls: Fn-lock, Win-key, touchpad, battery conservation, rapid
   charge, and LEDs after their surfaces are confirmed.

## Source Anchors

- johnfan capability catalog: `README.md`, `doc/FEATURES_AND_TESTING.md`,
  `extra/acpi/events/fn-r-refrate`, `extra/acpi/actions/fn-r-refresh-rate.sh`
- null-dev hybrid reference: `README.md`, `lenovo-legion-wmi.c`
- Bolekjar DKMS/event/hwmon reference: `LenovoLegion-dkms/legion-wmi-gamezone.*`,
  `legion-wmi-gamezone-sysfs.c`, `legion-wmi-events.*`, `legion-hwmon.c`,
  `legion-wmi-fm*`, `legion-wmi-ftable*`
