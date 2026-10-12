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
object is `AE` on `PNP0C14:02` = `\_SB_.GZFD.WMAE`. `acpi_call` lets root call it read-only:
`echo '\_SB_.GZFD.WMAE 0 17 b01000304' > /proc/acpi/call; cat /proc/acpi/call`. legionctl-system polls it as a
sensor (2 s). This is the only fan source; a failed read reports the RPM as unknown.

The in-tree `lenovo_wmi_other` module has `expose_all_fans` and `relax_fan_constraint` parameters. If a kernel
update makes it register a fan hwmon on this model without them, that hwmon replaces the acpi_call path in the
same change (check on each kernel update).

## Fan control (from LenovoLegionLinux `model_kwcn`, source HEAD 6cf67c3)

| Control | Firmware call | Rules on this model |
| --- | --- | --- |
| Fan curve | FAN_METHOD `\_SB_.GZFD.WMAB` id 5 `Fan_Get_Table`, id 6 `Fan_Set_Table`; 0x40-byte input, FSS0–FSS9 one byte each at 0x06 + 2·i; header ignored by the DSDT, which copies the 10 bytes to EC RAM F9F0..F9F9 | Speed-only: one table for both fans; no temperature points. Units are **levels 1–10**, not RPM or percent (percent values hard-powered-off an 82WM). Per-point minimum {1,1,1,1,1,1,1,1,3,5}. **Custom mode (255) only**: on the 16IRX8H the EC resets the table when mode 0xE0 is entered and ignores later writes (LLL issue #429). Read back after each write; re-apply after resume and on every custom-mode entry. |
| Level → RPM ladder | WQA3 `LENOVO_FAN_TABLE_DATA`, 15 rows (modes 1, 2, 3, 0xFF, 0xE0 × fan/sensor) | Measured 2026-10-12, the same in every mode. Fan 1: 1700 1900 2100 2200 2600 2800 3300 3500 4400 5100 rpm; fan 2: 1700 1800 2100 2300 2600 2800 3300 3600 4400 5100 rpm (levels 1–10). Fan 1 follows sensors 1 (IC) and 4 (CPU), fan 2 sensor 5 (GPU). Level 10 is 5,100 rpm; fan max mode (next row) runs above it. |
| Fan max mode ("extreme cooling") | GameZone `WMAA` id 0x0D, one-byte arg `b01` on / `b00` off | Not a ceiling: `b01` runs both fans at 6,000–6,100 rpm at once, also at idle (measured 2026-10-12). Under stress-ng (32 threads) + gpu-burn in max-power the normal curve stops at level 10 (5,100 rpm); with fan max mode the GPU holds ~165–170 W instead of ~150 W at its 87 °C limit; CPU package stays at 96–98 °C either way. Use: an explicit "max cooling" action, not a curve setting. No readback on this model: legionctl-system owns the state. |
| Full speed (dust cleaning) | Other Method `SetFeatureValue` (18), feature 0x04020000 | Custom mode only. |
| FAN_METHOD ids 1–4 | — | Not implemented by this firmware; never called. |
| EC fan lock (EC 0xC4AB over port I/O) | raw EC write | Not done: no validation for this EC, can stop fan control. |
## GPU mode, primary GPU and per-app pinning

- **Detect** (no root): which DRM card drives each connected output (`/sys/class/drm/card*-*/status` plus the card's
  driver), whether the NVIDIA eDP connector exists but is disconnected (hybrid / MUX on iGPU) or connected
  (discrete-only), Mutter's primary GPU (journal line "selected primary", or the udev tag), and the dGPU's runtime
  PM state (`/sys/bus/pci/devices/0000:01:00.0/power/runtime_status`).
- **Toggle**: primary GPU via the udev tag (applies at next login; OSD/UI offers "log out now"). MUX hybrid vs
  discrete-only is a firmware setting; the in-tree drivers on this kernel expose no MUX control (LenovoLegionLinux
  writes it through a GameZone WMI method). Plan: read-only detection first, then a WMI write through
  `acpi_call` only after validating the method on this BIOS, always followed by a reboot prompt.
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
