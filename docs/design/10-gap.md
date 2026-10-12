# 10 — Feature gap vs Lenovo Legion Toolkit and Vantage

Compared on 2026-10-12 against Lenovo Legion Toolkit (LLT, README of `LenovoLegionToolkit-Team/LenovoLegionToolkit`)
and Lenovo Vantage / Legion Space. "Path here" is the interface on this machine (BIOS KWCN54WW), taken from
LenovoLegionLinux's `model_kwcn` entry and the in-tree drivers. Every new firmware call starts read-only.

| Feature (Windows) | Linux today | Path here | Priority |
| --- | --- | --- | --- |
| Power modes Quiet / Balance / Performance / Extreme (max-power) | done: `platform_profile`, Fn+Q follow, PPD sync, OSD | `lenovo_wmi_gamezone` | — |
| Custom mode with CPU/GPU power limits | partly: RAPL fallback; firmware limits visible | `lenovo-wmi-other` firmware attributes (`ppt_*`, `gpu_*`); verify writability in `custom` | high |
| Fan speed readout | done (validated) | Other Method 17, feature 0x04030001/2 via `acpi_call` | — |
| Custom fan curve | missing | WMI3 `Fan_Get_Table` / `Fan_Set_Table`, speed-only on this model (`wmi_fancurve_speed_only`) | medium |
| Fan full speed (dust cleaning) | missing | WMI3, only in custom mode (`fanfullspeed_requires_custom_powermode`) | low |
| Lift firmware fan ceiling | missing | GameZone `WMAA(0, 0x0D)`; validated on KWCN54WW (LenovoLegionLinux PR #443) | medium |
| Hybrid mode / GPU working mode (dGPU only, Hybrid, Hybrid Auto, iGPU only) | detection designed (02-platform) | GameZone GSync 40/41/42 and iGPU mode 63/64/65; restart required | high |
| Deactivate dGPU (kill dGPU processes, nudge runtime PM) | missing | NVML process list, `power/runtime_status`; only in hybrid with no display on NVIDIA | medium |
| GPU overclock | missing | NVML clock offsets; limited on Wayland | low |
| Battery conservation (~80 %) | read only | `ideapad_laptop` `conservation_mode` | high (toggle + OSD) |
| Rapid charge | missing | `\_SB.PC00.LPCB.EC0.VPC0.SBMC` 0x07/0x08, `GBMD` to read | medium |
| Battery statistics / health | missing | `/sys/class/power_supply/BAT0` (cycle count, full vs design), UPower history | medium |
| Keyboard RGB (Spectrum per-key / 4-zone), white backlight levels | missing | ITE 8258 HID device; check OpenRGB support before writing a protocol | medium |
| Logo and rear-port lighting | missing | GameZone light methods; discover first | low |
| Win-key lock, touchpad lock | missing | GameZone 21/22/23 (Win key) | low |
| Fn lock | read + OSD | `ideapad_laptop` `fn_lock` | — |
| Panel Over Drive | missing | GameZone OD methods (LLT exposes it); discover | low |
| Instant boot (power on with AC/lid), flip to start | missing | LenovoLegionLinux `instant_boot_*`, `flip_to_start`; model support unverified | low |
| Highly customizable vitals OSD | partial: event OSD | GNOME Shell extension can add a top-bar indicator (watts, temps, rpm) and a pinned overlay | medium |
| Actions (triggers → steps, run programs) | partial: power source, Fn+Q, game (planned) | configset conditions + `run` steps (below) | high |
| CLI and quick actions | partial: `legionctl` | `legionctl action <name>`, D-Bus | medium |
| Driver and firmware updates | missing | `fwupd` (LVFS) for NVMe, Logitech Bolt, peripherals; DKMS/kernel checks (08-kernel) | medium |
| Warranty status, boot logo, Windows power-plan sync, disabling Vantage services | out of scope | Windows-specific or low value | — |

## Actions and triggers

LLT's action triggers map onto legiond event sources, so actions become configset layers with an optional
`run` list (commands executed as the user through legion-agent, or as root only if declared and polkit-allowed):

| Trigger | Source |
| --- | --- |
| AC connected / low-wattage adapter / disconnected | udev `power_supply` (adapter wattage via `ADP0` `power_now`/USB-PD where exposed) |
| Power mode changed | `platform-profile` uevent (ownership ledger knows who changed it) |
| Game / app started or closed | eBPF exec sensor, `/proc` fallback |
| Lid open/closed, session lock/unlock, user idle/active | logind (`LidClosed`, `LockedHint`, `IdleHint`) |
| Display on/off, external display connected, HDR on/off | legion-agent from Mutter `MonitorsChanged`; DRM uevents |
| Wi-Fi connected (SSID) | NetworkManager D-Bus |
| Time of day, sunrise/sunset, periodic | timers (sunrise/sunset from configured location) |
| Startup, resume | daemon start, logind `PrepareForSleep(false)` |
| Hybrid mode changed | GameZone status read on resume and after restart |
| Device connected/disconnected | udev (USB/HID) |

## More this machine could get

- **Factory display calibration**: Windows keeps the panel's factory ICC profile under
  `C:\Windows\System32\spool\drivers\color`; import it read-only from `nvme0n1p3` and register it with colord.
- **Firmware updates through fwupd** for the SK hynix and Crucial NVMe drives and the Logitech Bolt receiver.
- **Suspend health**: check `/sys/power/mem_sleep` (s2idle vs deep), measure drain per hour in suspend, and
  report wake sources.
- **Audio**: verify the internal speaker amplifiers are driven correctly (some Legion Gen 8 models need an amp
  quirk and firmware); check before claiming a problem.
- **Battery health tracking** over time (capacity vs design, cycles) in the UI.
- **Wi-Fi regulatory domain** check (`iw reg get`), relevant for 6 GHz bands.
