# 02 — Platform control surfaces

Every knob the daemon touches, with the source of truth measured on this machine.

## Firmware (Lenovo WMI)

| Surface | Path | Notes |
| --- | --- | --- |
| Thermal mode | `/sys/firmware/acpi/platform_profile` (choices: low-power balanced performance max-power custom) | Fn+Q changes it in firmware and emits a `platform-profile` uevent. Policy `follow`. |
| Firmware limits | `/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes/*` | `ppt_pl1_spl`, `ppt_pl2_sppt`, `ppt_pl1_tau`, `ppt_cpu_cl`, `cpu_temp`, `gpu_temp`, `gpu_nv_ac_offset`, `gpu_nv_ctgp`, `gpu_nv_ppab`. Each has `current_value`/`min_value`/`max_value`. |
| Fn lock | `/sys/bus/platform/devices/VPC2004:00/fn_lock` | Observe; OSD on change. |
| Battery conservation | `.../VPC2004:00/conservation_mode` (currently 1) | Configurable knob, default `observe`; OSD + status. |
| Camera power | `.../VPC2004:00/camera_power` | Observe; OSD on change (privacy). |
| USB charging | `.../VPC2004:00/usb_charging` | Configurable. |

**Open question:** are `ppt_*` writable in every platform_profile, or only in `custom` (as on Windows, where
Vantage switches to Custom mode to apply them)? Test: write `ppt_pl1_spl` in `max-power`, read back, then in
`custom`. The answer decides whether the AC layer uses `max-power` + limits or `custom` + limits.

**Measured 2026-10-12:** in `max-power`, writes to `ppt_pl1_spl` and `ppt_pl2_sppt` fail with `EBUSY` ("Device or
resource busy") and the values stay 90/135 W: the firmware limits are writable only in `custom` mode. In `custom` mode
the fans run at ~6,000 rpm even at idle (measured 2026-10-12: smart-fan mode 0xFF, fan table levels 1–10, fan max
mode off), so custom mode needs a tested fan-table write (`Fan_Set_Table`) before it can carry limits. Decision
(2026-10-12): the policy uses the firmware modes' built-in limits (max-power on AC: 90/135 W; balanced on battery:
to be measured) and writes no limits. Ranges: `ppt_pl1_spl` 30–140 W, `ppt_pl2_sppt`
55–190 W (so the Python battery cap of 25/35/45 W cannot be expressed; lowest is 30/55). Custom-mode defaults: 70/119
W, `ppt_cpu_cl` 55, `ppt_pl1_tau` 56, `gpu_nv_ctgp` 80, `gpu_nv_ppab` 15.
**Enforcement measured 2026-10-12** (custom mode, stress-ng on 32 threads, reassert timer stopped): `ppt_pl2_sppt`
55 held the package at 55 W, then `ppt_pl1_spl` 30 took over after 69 s (28–30 W, CPU 79 → 61 °C); with
`ppt_pl1_spl` 60 and `ppt_pl2_sppt` 55 the package held 55 W (the lower limit wins). Each write sets the MMIO
RAPL PL1 at once, so the firmware enforces through MMIO; no other writer may touch RAPL. Writes to `ppt_pl1_tau`,
`gpu_nv_ctgp` and `gpu_nv_ppab` (the attributes with no published range) are refused.
**Battery, measured 2026-10-12:** in `max-power` on battery the firmware itself sets MMIO RAPL PL1 to 45 W (90 W
on AC; MSR zone 140/190 W); under full load the package runs ~80 W for ~30 s, then 45 W sustained; the battery
discharges at 78–105 W. In `balanced` on battery (measured 2026-10-13, legionctl-system applied it on unplug) the
limits are the same: MMIO PL1 45 W, ~80 W for ~30 s, then 45 W sustained; the battery discharges at up to 124 W
during the burst. On battery the firmware caps the CPU at 45 W in both modes.

## CPU

| Knob | Path | Notes |
| --- | --- | --- |
| RAPL (read only) | `/sys/class/powercap/intel-rapl:0` and `intel-rapl-mmio:0` | Energy counters for watts. Limits are set only through firmware attributes; firmware mode changes rewrite MMIO, so legionctl never writes RAPL. |
| Governor, EPP | `/sys/devices/system/cpu/cpufreq/policy*/` | PPD writes EPP on profile changes; the daemon either drives PPD (preferred) or enforces. |
| Turbo, max_perf_pct, ignore_ppc | `intel_pstate/*`, `processor/parameters/ignore_ppc` | enforce |
| C-states | `cpupower idle-set` / `cpuidle/state*/disable` | keep all enabled (deep idle frees turbo headroom) |
| Throttle reasons | MSR `IA32_PACKAGE_THERM_STATUS` (0x1B1) and `IA32_THERM_STATUS` (0x19C) via `/dev/cpu/*/msr`; `thermal_throttle/*_count` | Sensor: decode PROCHOT, thermal, power-limit, current-limit log bits; explains the 33k throttle events. |

## Scheduling and kernel

- **sched_ext**: scx_loader D-Bus (`org.scx.Loader`): `SwitchScheduler(name, mode)`. Layers choose mode
  (auto/gaming/powersave). `scx_lavd --autopower` follows EPP when no explicit mode is set.
- **sysctl**: generic `/proc/sys` actuator; values from configsets; policy `enforce` with conflict logging (the
  zram udev rule that sets `vm.swappiness=150` agrees today; `sysctl --system` reset it to 100 once).
- **I/O**: NVMe `kyber`, APST and irqbalance stay as they are (decided 2026-10-12); exposed as `observe` knobs.

## GPU and display

| Knob | Path / API | Notes |
| --- | --- | --- |
| NVIDIA persistence, powerd | systemd units, NVML | as today |
| NVIDIA power | firmware `gpu_nv_ctgp` / `gpu_nv_ppab` / `gpu_nv_ac_offset`; NVML limits as read-only check | Dynamic Boost moves the limit between ctgp and ctgp+ppab (150–175 W). |
| Internal refresh | Mutter `org.gnome.Mutter.DisplayConfig.ApplyMonitorsConfig` (method TEMPORARY) via legionctl-operator | Persistent method triggers GNOME's confirmation dialog. Keep VRR state. |
| Primary GPU | udev tag `mutter-device-preferred-primary` on the card that drives the active outputs | **Issue:** Mutter picked i915 because the panel exists, while the 144 Hz external is on NVIDIA. Rule: if an external output on card0 is active, prefer NVIDIA; on battery with only eDP, prefer i915 so the dGPU can suspend. Takes effect on next Mutter start (re-login), so the daemon can only recommend/prepare, not hot-switch. |
| MUX / GPU mode | Lenovo WMI gamezone (hybrid / dGPU-only) | Read-only first; switching needs reboot. |

## Fans

There is no fan hwmon on this machine (`thinkpad_acpi`, which reported a bogus fan, is no longer autoloaded;
`hwmon4` is now `spd5118`).

**Path (implemented and validated 2026-10-12):** idle 2,200/2,300 rpm in max-power, 2,900/2,900 rpm after 40 s of
gpu-burn (GPU 60 → 70 °C); readings move with load in the firmware's 100 rpm steps. The firmware publishes RPM
through the Lenovo "Other Method" WMI interface, `GetFeatureValue` (method 17) with feature `0x04030001`/`0x04030002`
(value = FANS × 100), the path LenovoLegionLinux uses for this model (`model_kwcn`, `ACCESS_METHOD_WMI3`). The WMI
object is `AE` on `PNP0C14:02` = `\_SB_.GZFD.WMAE`. Since 2026-10-16 (todo-79, the switch part) our module
`legionctl_wmi` makes this call and gives the RPM as `fan1_input` and `fan2_input` of its hwmon device
`legionctl_wmi` (found by its name; the number changes from start to start). This is the only fan source: without
the module the fan commands fail with `legionctl_wmi is not loaded: see docs/design/08-kernel.md`, and the views show
the RPM as unknown with that reason. `acpi_call` is not used any more.

## Fan control (from LenovoLegionLinux `model_kwcn`, source HEAD 6cf67c3)

| Control | Firmware call | Rules on this model |
| --- | --- | --- |
| Fan curve | FAN_METHOD `\_SB_.GZFD.WMAB` id 5 `Fan_Get_Table`, id 6 `Fan_Set_Table`; 0x40-byte input, FSS0–FSS9 one byte each at 0x06 + 2·i; header ignored by the DSDT, which copies the 10 bytes to EC RAM F9F0..F9F9 | Speed-only: one table for both fans; no temperature points. Units are **levels 1–10**, not RPM or percent (percent values hard-powered-off an 82WM). Per-point minimum {1,1,1,1,1,1,1,1,3,5}. **Custom mode (255) only**: on the 16IRX8H the EC resets the table when mode 0xE0 is entered and ignores later writes (LLL issue #429). Read back after each write; re-apply after resume and on every custom-mode entry. |
| Level → RPM ladder | WQA3 `LENOVO_FAN_TABLE_DATA`, 15 rows (modes 1, 2, 3, 0xFF, 0xE0 × fan/sensor) | Measured 2026-10-12, the same in every mode. Fan 1: 1700 1900 2100 2200 2600 2800 3300 3500 4400 5100 rpm; fan 2: 1700 1800 2100 2300 2600 2800 3300 3600 4400 5100 rpm (levels 1–10). Fan 1 follows sensors 1 (IC) and 4 (CPU), fan 2 sensor 5 (GPU). Level 10 is 5,100 rpm; fan max mode (next row) runs above it. |
| Fan max mode ("extreme cooling") | GameZone `WMAA` id 0x0D, one-byte arg `b01` on / `b00` off | Not a ceiling: `b01` runs both fans at 6,000–6,100 rpm at once, also at idle (measured 2026-10-12). Under stress-ng (32 threads) + gpu-burn in max-power the normal curve stops at level 10 (5,100 rpm); with fan max mode the GPU holds ~165–170 W instead of ~150 W at its 87 °C limit; CPU package stays at 96–98 °C either way. Use: an explicit "max cooling" action, not a curve setting. No readback on this model: legionctl-system owns the state. Does not survive suspend (measured 2026-10-12: 6,000 rpm before S3, back on the normal curve 5 s after resume), so legionctl-system re-asserts it after every resume. |
| Full speed (dust cleaning) | Other Method `SetFeatureValue` (18), feature 0x04020000 | Custom mode only. |
| FAN_METHOD ids 1–4 | — | Not implemented by this firmware; never called. |
| EC fan lock (EC 0xC4AB over port I/O) | raw EC write: 0x08 locks, 0x00 releases; through `/dev/port`, index 0x4E / data 0x4F (LLL `ecram_portio_write`) | Not validated for this EC; an incorrect write can stop fan control. Built, but refused unless the profile sets `fan.ec_fan_lock_verified` (false in the shipped profile). |

### What is built (todo-77, 2026-10-14; through `legionctl_wmi` since todo-79, 2026-10-16)

`legionctl fan status | curve set <10 levels> | curve reset | max on|off | clean on|off | lock on|off`, each with
`--dry-run` for the changes. The CLI and the operator only plan and check; `legionctl-system` writes the files of the
`legionctl_wmi` hwmon device (08-kernel, sysfs ABI) after polkit `org.legionctl.fan` (`org.legionctl.fan.lock` for the
EC lock, no kept authorization). D-Bus: `FanStatus`, `SetFanCurve`, `ResetFanCurve`, `SetFanMax`, `SetFanFullSpeed`,
`SetFanLock`. The module makes the firmware calls above (Fan_Get_Table, Fan_Set_Table, WMAA 0x0D, SetFeatureValue
0x04020000) and nothing else; the VM test of the module checks each call byte for byte against a stand-in SSDT.

| Control | File of hwmon `legionctl_wmi` |
| --- | --- |
| Fan table | `pwm1_auto_point1_pwm` .. `pwm1_auto_point10_pwm` (levels 1–10) |
| Full speed | `pwm1_enable`: 0 full speed, 2 the table |
| Fan max mode | `fan_max_mode`: 1 on, 0 off (the state that the module set; the firmware has no read) |
| Fan RPM | `fan1_input`, `fan2_input` |

- **Checks before any write**: the same rules as the module: 10 levels, each 1–10, not below the point's minimum,
  not lower than the point before; the operator checks before polkit asks, and the daemon checks again. A curve write
  or full speed outside custom mode is refused with the mode and how to enter custom mode.
- **Point by point**: the module takes one point per write and refuses a write that makes the table invalid. The
  daemon writes the changed points in two passes: from point 10 down the higher of the old and the new level, then
  from point 1 up the new level; each table between them is valid. The result line lists the writes in order
  (`writes 10:10 9:8 ... 5:3`), then the daemon reads the table back.
- **Kept state**: the user's curve and fan max mode are in `system.db` (table `fan`), across restarts and reboots.
- **Again after events**: the module sets fan max mode again after resume, and the last table written through sysfs
  and full speed on each entry to custom mode. The daemon adds what a new module does not have: at start, fan max mode
  when the user has it on and the module has it off; at start, after resume and at each `platform_profile` change,
  the user's curve (else the default) when the mode after the event is custom (the read-back makes a second write a
  no-op). Without the module, the event's plan has a warning and goes on.
- **No module**: each fan command fails with `legionctl_wmi is not loaded: see docs/design/08-kernel.md`; no other
  path to the firmware is tried.
- **Default curve**: the profile's `fan.default_curve`, `1 2 3 4 5 6 7 8 9 10` (each point at its own level, 1,700 to
  5,100 rpm). It is not the table read at custom-mode entry: the EC resets the table in the other modes, and custom
  mode runs the fans at ~6,000 rpm until a table is written, so that read is not a sane default. `fan curve reset`
  forgets the user's curve and writes the default (in custom mode; otherwise at the next custom-mode entry).
- **Tests** (`crates/legionctl/tests/machine.rs`, `fan_control` and
  `entering_custom_mode_writes_the_users_fan_curve_or_the_default`): the recorded `legionctl_wmi` tree of this machine
  (`tests/fixtures/sysfs-legionctl-wmi-82wq-2026-10-16`, read-only) is the firmware. Its files are plain files: the
  module's own refusals are checked in the module's VM test (08-kernel).

**The first live writes are the user's (todo-17 step 4, 08-kernel, First live load).** Until the profile sets
`fan.curve_verified` (false in the shipped profile), legionctl makes no table or full-speed write: `curve set`,
`curve reset` and `clean` are refused with the reason (their dry runs show the writes), and custom mode gets no
automatic write. The reads (`fan status`, the table, the RPM) work. Fan max mode is not gated (WMAA 0x0D was
write-tested on 2026-10-12). After step 4 is right, the profile in the repository sets `fan.curve_verified` to true
(a new package). Do not set `fan.ec_fan_lock_verified` until the EC lock is proven on this EC.
## GPU mode, primary GPU and per-app pinning

- **Detect** (no root): which DRM card drives each connected output (`/sys/class/drm/card*-*/status` plus the card's
  driver), whether the NVIDIA eDP connector exists but is disconnected (hybrid / MUX on iGPU) or connected
  (discrete-only), Mutter's primary GPU (journal line "selected primary", or the udev tag), and the dGPU's runtime
  PM state (`/sys/bus/pci/devices/0000:01:00.0/power/runtime_status`).
- **Toggle**: primary GPU via the udev tag (applies at next login; OSD/UI offers "log out now"). MUX hybrid vs
  discrete-only is a firmware setting; the in-tree drivers on this kernel expose no MUX control (LenovoLegionLinux
  writes it through a GameZone WMI method). Plan: read-only detection first, then a WMI write as a new
  `legionctl_wmi` call (todo-79, Windows parity) only after validating the method on this BIOS, always followed by a
  reboot prompt.
- **Pin apps to a GPU** (hybrid mode): a desktop-entry override in `~/.local/share/applications/` that sets
  `PrefersNonDefaultGPU=true` (GNOME launches it on the other GPU through switcheroo-control), or for explicit
  control an `Exec` prefix: NVIDIA via PRIME offload (`__NV_PRIME_RENDER_OFFLOAD=1 __GLX_VENDOR_LIBRARY_NAME=nvidia
  __VK_LAYER_NV_optimus=NVIDIA_only`), Intel via `__EGL_VENDOR_LIBRARY_FILENAMES=.../50_mesa.json` and
  `MESA_VK_DEVICE_SELECT`. `legionctl run --gpu nvidia -- cmd` for one-off launches.

## Process placement: cores and priority

Generic, works on any machine where the package is installed:

- **Cores**: P-cores and E-cores from `/sys/devices/cpu_core/cpus` and `/sys/devices/cpu_atom/cpus` (here 0–15 and
  16–31). Placement through systemd: launch in a transient scope with `AllowedCPUs=` (`systemd-run --user --scope
  -p AllowedCPUs=0-15 cmd`), or move running PIDs with `taskset`. Rules apply automatically at exec once the eBPF
  workload sensor exists (04-ebpf); until then via desktop-entry wrappers and `legionctl run --cores p|e`.
- **Priority**: nice, I/O class and latency-nice through ananicy-cpp, which is already running with CachyOS rules.
  legionctl-system writes one owned rules file (`/etc/ananicy.d/99-legion/legion.rules`) from the configset instead of a
  second mechanism, and reloads ananicy.

## Input

- Fn keys: evdev on `Ideapad extra buttons` (found by name), debounced (Fn+R: 1.5 s). The daemon reads the device
  as root, so the user no longer needs to be in `input`; the operator performs session-side actions.
- The hwdb remap `KEYBOARD_KEY_110=f18` and its GNOME shortcut are deleted in the switch (15-layout).
- Logitech: logiops (logid) stays external; HID-BPF is an option for in-kernel remaps later (04-ebpf).

## Graphics stack checks (built 2026-10-14, todo-81 part 1)

`doctor` names each gap of the stack that rendering on both GPUs needs, with its fix (`legionctl-machine` graphics):
the NVIDIA libraries (`libnvidia-glcore.so.<version>`) that do not match the loaded module (`/proc/driver/nvidia/
version`), NVIDIA's DRM without modesetting, a missing Vulkan ICD or EGL vendor file for a loaded GPU driver, with
both GPUs switcheroo-control not running (GNOME's "Launch using Discrete Graphics Card") and `OpenCL` for the iGPU only
through rusticl (`RUSTICL_ENABLE=iris`, or intel-compute-runtime), and Electron/Chromium apps whose flags file has no
Wayland flag (they draw through XWayland, blurry at fractional scales). e2e on the recorded machine: each gap found,
each gone once fixed (the version compare mutation-checked). Found here on 2026-10-14: switcheroo-control inactive,
Intel OpenCL only through rusticl. Part 2 (built 2026-10-14): `legionctl display status` reads the monitors from Mutter's `DisplayConfig`
(the session bus): each active monitor's resolution, refresh rate and the highest at that resolution, scale, pixel
density (from the EDID's physical size), VRR and HDR (offered, on); `doctor` and `display status` name an external
monitor below its highest rate (the panel is left out: Fn+R lowers it on purpose), grayscale antialiasing where
subpixel is sharper (integer scales below 160 PPI), and Electron/Chromium apps through XWayland only at a fractional
scale. Lesson (user report 2026-10-14, on the MSI 34" at scale 1.0, 111 PPI): grayscale antialiasing and the Wayland
flags for VS Code and Thorium both made text worse; at integer scales XWayland is not blurry, and Chromium on Wayland
draws grayscale text. Both were reverted. e2e with a stand-in Mutter: both scales, the findings that belong to each
(the scale gate mutation-checked). Part 3 (built 2026-10-14): each finding has a stable id, in brackets in `doctor`
and `display status` (`refresh-DP-3`, `antialiasing`, `xwayland-code`, `wayland-thorium`, `opencl-iris`,
`switcheroo`, ...). `legionctl display fix <id>|all [--dry-run] [--persistent]` makes the fixes that change only your
own settings and files: the fonts' antialiasing (`font-antialiasing` 'rgba' and `font-rgba-order` 'rgb', where
subpixel is sharper), an app's flags file (the Wayland line in at a fractional scale, out at integer scales), a
monitor's highest rate at its resolution (Mutter `ApplyMonitorsConfig`: temporary until the next login, or with
`--persistent` in GNOME's monitors.xml) and `RUSTICL_ENABLE=iris` for the session (`~/.config/environment.d/
legionctl-opencl.conf`, from the next login). Each line shows the new and the old value. Before the first change, the
event log keeps the old values (event `display.change`: the dconf text, the file's bytes or that there was no file,
the mode, the Mutter method), so `legionctl undo <event>` gives back each exact old value, also after a change that
failed half way; the undo is an event too. Fixes that need root (switcheroo-control, the kernel line, packages) stay
text: they belong to the daemon and the boot takeover; `display fix <id>` refuses them, `all` lists them. A flags file
that is not text stays as it is. e2e with the stand-in Mutter, a stand-in gsettings and a temp HOME: the dry run
changes nothing, the fix changes each value and its findings go, undo gives back each exact value with the method of
the fix, a system change is refused.
