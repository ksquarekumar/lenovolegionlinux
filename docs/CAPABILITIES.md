# Legion Capability Inventory

This file separates three states:

- verified here: currently working or observed on the 82WQ machine
- source-derived: present in one of the reference projects but not verified here
- deferred: intentionally not owned until the lower-risk surface is proven

The current installed package is a stable power-policy layer, not full Legion
feature coverage yet.

The upstream capability comparison is in
[REFERENCE_COMPARISON.md](REFERENCE_COMPARISON.md).

## Verified Here

| Capability | Current local owner | State |
| --- | --- | --- |
| AC power policy | `legionctl` + systemd/udev | Working. Applies CPU RAPL, CPU governor/EPP, `platform_profile`, NVIDIA persistence, and `nvidia-powerd`. |
| CPU RAPL limits | standard `intel_rapl`/`intel_rapl_mmio` powercap | Working at `70 W long / 125 W short / 125 W peak` on AC. |
| NVIDIA Dynamic Boost limit | firmware + `nvidia-powerd` | Working. `nvidia-smi -q -d POWER` reports 150 W current/requested limit on AC. |
| Firmware platform profile | in-tree Lenovo ACPI/platform stack | Working. AC target is `max-power` when available. |
| VPC2004 `fan_mode` | none | Measured ineffective. Writing `4` immediately reads back as `0`; this is status-only. |
| `thinkpad_acpi` fan telemetry | ignored | Measured bogus. It reports `4 RPM` / `0% MANUAL CONTROL` and is not trusted. |

## Hotkeys

| Hotkey | Expected function | Evidence | Local state |
| --- | --- | --- | --- |
| `Fn+Q` | Firmware thermal/power mode switch | Captured 2026-10-12: firmware cycles `platform_profile` itself and emits a `platform-profile` change uevent per press (plus WMI `PNP0C14:02` 0xe3/0xe7). | Implemented: udev runs `legionctl follow`, which keeps the chosen mode as an override (PPD + governor from `firmware_follow`) until the power source changes, reboot, or `apply`. |
| `Fn+R` | Refresh-rate toggle | Captured 2026-10-12: `KEY_REFRESH_RATE_TOGGLE` (562) on `Ideapad extra buttons`; keycodes above 255 cannot be bound by GNOME/XKB. | `legion-refresh-toggle` switches the built-in panel between its highest fixed refresh and ~60 Hz via Mutter D-Bus. `hwdb/90-legion-keys.hwdb` remaps scancode 0x110 to F18 (XF86Launch9); `legion-osd` (user service) reads the Fn-key device directly and runs the toggle, applied as a temporary display change so GNOME does not ask to confirm. |
| `Fn+Space` | Keyboard lighting/RGB cycle | Captured 2026-10-12: no input event, only WMI 0xd0 notifications; backlight changes. | Firmware-handled and working; nothing to do. |
| `Fn+Esc` | Fn-lock toggle | Captured 2026-10-12: no input event, only WMI 0xd0 notifications; Fn-lock toggles. | Firmware-handled and working; nothing to do. |

## DKMS-Backed Candidates

| Capability | Best source | Why it matters | Import rule |
| --- | --- | --- | --- |
| Real fan RPM and temperatures | Bolekjar `legion-hwmon` | Replaces bogus `thinkpad_acpi` fan data. | First DKMS target, read-only only. |
| WMI thermal/Fn+Q events | Bolekjar `legion-wmi-events` | Lets `legionctl` notice firmware mode changes instead of only reasserting policy. | Import after it coexists with in-tree `lenovo_wmi_*`. |
| Fan max/full-speed | johnfan controls, Bolekjar fan methods | Useful as a bounded fan test/recovery command. | Explicit command with timeout only after telemetry works. |
| Fan curves | johnfan fan curve model, Bolekjar fan table/methods | Possible custom mode later. | Deferred until read-only fan telemetry is trusted. |
| Hybrid graphics / G-Sync / MUX | null-dev hybrid method, Bolekjar gamezone | Useful for explicit graphics mode changes. | Utility command only; never automatic AC policy. |
| Keyboard lighting/RGB | Bolekjar RGB and keyboard light methods, johnfan backlight notes | Covers `Fn+Space` adjacent behavior. | Discover first; do not import GUI/daemon stack wholesale. |
| Win-key/touchpad locks | Bolekjar gamezone methods | Optional convenience controls. | Explicit status/control only. |
| Battery conservation/rapid charge | johnfan userspace, Bolekjar charge events | Optional battery health controls. | Keep separate from performance policy. |
| Display overdrive | Bolekjar OD methods | Optional panel setting. | Explicit status/control only after support check. |

## Measurement Tasks

Use [MEASUREMENT.md](MEASUREMENT.md) for the capture workflow. Before adding
write support for any missing feature, capture:

1. Hotkey events while pressing `Fn+Q`, `Fn+R`, `Fn+Space`, and `Fn+Esc`.
2. Live `udevadm monitor --kernel --udev --property` output during those keys.
3. Input events through `libinput debug-events` or `evtest`.
4. WMI/event messages from `journalctl -kf`.
5. Current display output and available panel modes from the active desktop stack.
6. Fan RPM from a read-only DKMS hwmon prototype and compare it to physical fan behavior under load.

Until those are captured, the only fully installed/owned scope is stable
CPU/GPU/firmware performance policy.
