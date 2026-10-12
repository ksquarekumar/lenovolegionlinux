# 10 — Feature gap vs Lenovo Legion Toolkit and Vantage

Compared on 2026-10-12 against Lenovo Legion Toolkit (LLT, README of `LenovoLegionToolkit-Team/LenovoLegionToolkit`)
and Lenovo Vantage / Legion Space. "Path here" is the interface on this machine (BIOS KWCN54WW), taken from
LenovoLegionLinux's `model_kwcn` entry and the in-tree drivers. Every new firmware call starts read-only.

| Feature (Windows) | Linux today | Path here | Priority |
| --- | --- | --- | --- |
| Power modes Quiet / Balance / Performance / Extreme (max-power) | done: `platform_profile`, Fn+Q follow, PPD sync, OSD | `lenovo_wmi_gamezone` | — |
| Custom mode with CPU/GPU power limits | partly: firmware limits visible; Python tooling still writes RAPL until the switch | `lenovo-wmi-other` firmware attributes (`ppt_*`, `gpu_*`); verify writability in `custom` | high |
| Fan speed readout | done (validated) | Other Method 17, feature 0x04030001/2 via `acpi_call` | — |
| Custom fan curve | missing | FAN_METHOD `WMAB` 5/6, levels 1–10, speed-only, custom mode only (02-platform, Fan control) | medium |
| Fan full speed (dust cleaning) | missing | Other Method `SetFeatureValue` 0x04020000, custom mode only | low |
| Lift firmware fan ceiling | missing | GameZone `WMAA(0, 0x0D)`; validated on KWCN54WW (LenovoLegionLinux PR #443) | medium |
| Hybrid mode / GPU working mode (dGPU only, Hybrid, Hybrid Auto, iGPU only) | detection designed (02-platform) | GameZone GSync 40/41/42 and iGPU mode 63/64/65; restart required | high |
| Deactivate dGPU (kill dGPU processes, nudge runtime PM) | missing | NVML process list, `power/runtime_status`; only in hybrid with no display on NVIDIA | medium |
| GPU overclock | missing | NVML clock offsets; limited on Wayland | low |
| Battery conservation (~80 %) | read only | `charge_types` = `Long_Life` (one interface with rapid charge) | high (toggle + OSD) |
| Rapid charge | missing | in-tree `/sys/class/power_supply/BAT0/charge_types` = `Fast` (values `Fast Standard Long_Life`; the kernel keeps Fast and Long_Life exclusive) | medium |
| Battery statistics / health | missing | `/sys/class/power_supply/BAT0` (cycle count, full vs design), UPower history | medium |
| Keyboard RGB (Spectrum per-key / 4-zone), white backlight levels | missing | ITE 8258 HID device; check OpenRGB support before writing a protocol | medium |
| Logo and rear-port lighting | missing | KBBACKLIGHT `\_SB_.GZFD.WMAF`: light id 0x03 Y-logo (set 1 on / 2 off), 0x05 IO port; read first | low |
| Win-key lock, touchpad lock | missing | GameZone `WMAA` 21/22/23 (Win key), 24/25/26 (touchpad); call the IsSupport id first | low |
| Fn lock | read + OSD | `ideapad_laptop` `fn_lock` | — |
| Panel Over Drive | missing | GameZone `WMAA` 49 (supported?), 50 (get), 51 (set) | low |
| Instant boot (power on with AC/lid), flip to start | missing | Other Method 0x03010001/2, 0x00030000; LLL does not enable them for this model; read-only probe only | low |
| Highly customizable vitals OSD | partial: event OSD | GNOME Shell extension can add a top-bar indicator (watts, temps, rpm) and a pinned overlay | medium |
| Actions (triggers → steps, run programs) | partial: power source, Fn+Q, game (planned) | configset conditions + `run` steps (below) | high |
| CLI and quick actions | partial: `legionctl` | `legionctl action <name>`, D-Bus | medium |
| Driver and firmware updates | missing | `fwupd` (LVFS) for NVMe, Logitech Bolt, peripherals; DKMS/kernel checks (08-kernel) | medium |
| Boot logo | missing | efivars `LBLDVC-871455d1-…` / `LBLDESP-871455d0-…`; back up both first; image size from LBLDESP | low |
| Warranty status, Windows power-plan sync, disabling Vantage services | out of scope | Windows-specific or low value | — |

## Actions and triggers

LLT's action triggers map onto legionctl-system event sources, so actions become configset layers with an optional
`run` list (commands executed as the user through legionctl-operator, or as root only if declared and polkit-allowed):

| Trigger | Source |
| --- | --- |
| AC connected / low-wattage adapter / disconnected | udev `power_supply` (adapter wattage via `ADP0` `power_now`/USB-PD where exposed) |
| Power mode changed | `platform-profile` uevent (ownership ledger knows who changed it) |
| Game / app started or closed | eBPF exec sensor |
| Lid open/closed, session lock/unlock, user idle/active | logind (`LidClosed`, `LockedHint`, `IdleHint`) |
| Display on/off, external display connected, HDR on/off | legionctl-operator from Mutter `MonitorsChanged`; DRM uevents |
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

## LenovoLegionLinux

Studied against upstream source (HEAD 6cf67c3, `kernel_module/legion-laptop.c` `model_kwcn`; DeepWiki's index
is older). On this model every LLL feature goes through WMI or ACPI methods that `acpi_call` reaches, or through
in-tree drivers, so the out-of-tree `legion_laptop` module is not needed. Loading it would also take GameZone from
`lenovo_wmi_gamezone` (`model_kwcn` does not set `leave_gamezone_wmi_unbound`) and break the firmware attributes.
Not done: LLL's raw EC writes (`lockfancontroller`) and GPU/CPU overclock methods (not declared by this DSDT).
Firmware WMI objects on `\_SB_.GZFD`: WMAA GameZone, WMAB FAN_METHOD, WMAC CPU_METHOD, WMAD GPU_METHOD,
WMAE Other Method, WMAF lights, WQA3 fan table data, WQA5 capdata01, WQA7 discrete data.
Read-only probe on this machine (2026-10-12, `acpi_call`, IsSupport and Get ids only; ids from LLL source):

| Call | Result | Meaning |
| --- | --- | --- |
| WMAA 21 / 23 | 1 / 0 | Win-key lock supported; Win key enabled |
| WMAA 24 / 26 | 1 / 0 | touchpad lock supported; touchpad enabled |
| WMAA 40 / 41 | 2 / 0 | GSync/MUX supported; status 0 = hybrid (matches the detected panel on i915) |
| WMAA 43 / 45, 55 | 6 / 0xE0, 0xE0 | smart-fan modes supported; current mode and thermal mode 0xE0 = max-power |
| WMAA 47 | 1 | power charge mode 1 (on AC) |
| WMAA 49 / 50 | 1 / 1 | panel overdrive supported; on |
| WMAA 63 / 64 | 3 / 0 | iGPU mode supported; mode 0 = default |
| WMAF get 0x03 | byte 0 = 1 | Y-logo light present; on |
| WMAF get 0x05 | 0, 0 | no IO-port light (LLL reads 0 as "not present") |
| WMAF get 0x00 | 0, 1 | white keyboard light id answers; this model has per-key RGB, so not used |
| WQA3 rows 0–14 | ladders in 02-platform | fan level → RPM |

Still unverified: whether the raised fan ceiling survives suspend and whether it goes above level 10 (needs a write
test under load).
