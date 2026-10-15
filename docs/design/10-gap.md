# 10 — Feature gap vs Lenovo Legion Toolkit and Vantage

Compared on 2026-10-12 against Lenovo Legion Toolkit (LLT, README of `LenovoLegionToolkit-Team/LenovoLegionToolkit`)
and Lenovo Vantage / Legion Space. "Path here" is the interface on this machine (BIOS KWCN54WW), taken from
LenovoLegionLinux's `model_kwcn` entry and the in-tree drivers. Every new firmware call starts read-only.

| Feature (Windows) | Linux today | Path here | Priority |
| --- | --- | --- | --- |
| Power modes Quiet / Balance / Performance / Extreme (max-power) | done: `platform_profile`, Fn+Q follow, PPD sync, OSD | `lenovo_wmi_gamezone` | — |
| Custom mode with CPU/GPU power limits | blocked: custom mode accepts `ppt_*` (enforcement measured) but runs the fans at ~6,000 rpm until a fan table is written; policy uses the firmware modes' built-in limits; no RAPL writes | `lenovo-wmi-other` firmware attributes (`ppt_*`, `gpu_*`); verify writability in `custom` | high |
| Fan speed readout | done (validated) | Other Method 17, feature 0x04030001/2 via `acpi_call` | — |
| Custom fan curve | built (`legionctl fan curve`), fixture-tested; live use waits for todo-17 | FAN_METHOD `WMAB` 5/6, levels 1–10, speed-only, custom mode only (02-platform, Fan control) | medium |
| Fan full speed (dust cleaning) | built (`legionctl fan clean`), fixture-tested; live use waits for todo-17 | Other Method `SetFeatureValue` 0x04020000, custom mode only | low |
| Fan max mode (extreme cooling) | built (`legionctl fan max`), set again after resume and at start | GameZone `WMAA(0, 0x0D)`; measured here: fans at 6,000–6,100 rpm at any load (normal maximum 5,100), GPU +10 % power at its temperature limit | medium |
| Hybrid mode / GPU working mode (dGPU only, Hybrid, Hybrid Auto, iGPU only) | detection designed (02-platform) | GameZone GSync 40/41/42 and iGPU mode 63/64/65; restart required | high |
| Deactivate dGPU (kill dGPU processes, nudge runtime PM) | built 2026-10-15: `legionctl gpu power off\|on` (daemon, polkit `org.legionctl.gpu.power`; refused while a program holds `/dev/nvidia*` or the NVIDIA DRM nodes, with the programs named; refused when the GPU is not on `nvidia`); `gpu status` shows `runtime_status`, `control` and the programs; doctor finds `control` = `on` and `NVreg_DynamicPowerManagement=0`; `undo` (knob `nvidia.runtime_pm`) | `/sys/bus/pci/devices/0000:01:00.0/power/control` (`auto`: D3cold when idle); legionctl does not stop programs | — |
| GPU overclock | not built: the driver allows offsets (read-only probe 2026-10-15, driver 580.178.04: NVML `GpcClkMinMaxVfOffset` -1000..1000 MHz, `MemClkMinMaxVfOffset` -2000..6000 MHz, both now 0; `nvidia-settings -q` the same), but the only write path on Wayland is NVML as root (`nvmlDeviceSetClockOffsets`; no `nvidia-smi` option; `nvidia-settings` needs X). That is an FFI to `libnvidia-ml` in the daemon, and the user forbids live GPU clock changes, so no write can be checked: waits for the user's decision | NVML clock offsets | low |
| Battery conservation (~80 %) | built 2026-10-14: `legionctl battery mode conservation` (also `standard`) | `charge_types` = `Long_Life` (one interface with rapid charge) | — |
| Rapid charge | built 2026-10-14: `legionctl battery mode rapid` (refused when the battery does not offer it) | `/sys/class/power_supply/BAT0/charge_types` = `Fast` (the kernel keeps Fast and Long_Life exclusive) | — |
| Battery statistics / health | built 2026-10-14: `legionctl battery status` (health = full vs design, cycles, charge rate) and a daily `battery.sample` for the trend | `/sys/class/power_supply/BAT0` | — |
| USB charging while off | built 2026-10-14: `legionctl battery usb-charging on|off` | `ideapad_acpi` `usb_charging` | — |
| Keyboard RGB (Spectrum per-key / 4-zone), white backlight levels | built 2026-10-15 through OpenRGB: OpenRGB 1.0 drives this keyboard (`LEGION_7GEN8` = USB `048d:c988`, the ITE 8258 here); `legionctl rgb status` (USB, SDK server, devices, profiles); the configset knob `rgb.profile` loads a saved OpenRGB profile through the SDK protocol (TCP 6742) when its conditions change, once per profile and boot; no protocol of our own. Not installed on this host yet (`pacman -S openrgb`; its udev rules give the hidraw device) | ITE 8258 HID device through OpenRGB | — |
| Logo and rear-port lighting | missing | KBBACKLIGHT `\_SB_.GZFD.WMAF`: light id 0x03 Y-logo (set 1 on / 2 off), 0x05 IO port; read first | low |
| Win-key lock, touchpad lock | missing | GameZone `WMAA` 21/22/23 (Win key), 24/25/26 (touchpad); call the IsSupport id first | low |
| Fn lock | read + OSD | `ideapad_laptop` `fn_lock` | — |
| Panel Over Drive | missing | GameZone `WMAA` 49 (supported?), 50 (get), 51 (set) | low |
| Instant boot (power on with AC/lid), flip to start | missing | Other Method 0x03010001/2, 0x00030000; LLL does not enable them for this model; read-only probe only | low |
| Highly customizable vitals OSD | built 2026-10-15: `legionctl overlay on\|off`: the operator sends the key readings of `sensors` (CPU and GPU temperature and power or the GPU asleep, fans, firmware mode, power source) every 2 s to the extension, which pins them top right, over fullscreen windows, click-through. Not built: a choice of readings | GNOME Shell extension `legionctl-osd` | — |
| Actions (triggers → steps, run programs) | partial: power source, Fn+Q, game (planned) | configset conditions + `run` steps (below) | high |
| CLI and quick actions | partial: `legionctl` | `legionctl action <name>`, D-Bus | medium |
| Driver and firmware updates | built 2026-10-15: `legionctl firmware status` (`fwupdmgr get-devices --json`, read-only: version and upgrade per device), `firmware update <id\|name> [--dry-run]` (`fwupdmgr update <id> --assume-yes --no-reboot-check`; fwupd asks polkit itself; event `firmware.change` with the old version). fwupd is not installed on this host; no update ran live | `fwupd` (LVFS) for NVMe, Logitech Bolt, peripherals; DKMS/kernel checks (08-kernel) | — |
| Boot logo | not built: a firmware (EFI variable) write that nobody verified on this BIOS; waits for todo-17 | efivars `LBLDVC-871455d1-…` / `LBLDESP-871455d0-…`; back up both first; image size from LBLDESP | low |
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
Not done: GPU/CPU overclock methods (not declared by this DSDT). LLL's raw EC write `lockfancontroller` is built as
`legionctl fan lock` but refused until the profile sets `fan.ec_fan_lock_verified` (02-platform, Fan control); no
other raw EC write exists.
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

Write test 2026-10-12: WMAA 0x0D is a fan max mode, not a ceiling (6,000–6,100 rpm at any load vs 5,100 rpm normal
maximum; ~+10 % sustained GPU power at the 87 °C limit; 02-platform). It does not survive suspend (S3 through `systemctl suspend`;
`rtcwake -m mem` skips the NVIDIA suspend services and fails with `nv_pmops_suspend returns -5`), so it is
re-asserted after every resume.
