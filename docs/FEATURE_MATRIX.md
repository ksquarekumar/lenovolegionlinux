# Feature Adoption Matrix

The goal is a best-of-three local package, not a mechanical merge. Each feature
must have one owner. If the kernel or NVIDIA already provides a working owner,
this repo should orchestrate it instead of replacing it.

## Already Adopted

| Area | Source influence | Local owner | Decision |
| --- | --- | --- | --- |
| Machine identity | Bolekjar DMI compatibility concept | `profiles/*.json` | Adopted, stricter than upstream: exact stable 82WQ/16IRX8H match with board/BIOS as observed metadata. |
| Central control surface | johnfan Python module/CLI idea | `legionctl` | Adopted, but without GUI and without daemon state split. |
| AC/battery automation | johnfan `legiond` profile idea | `udev` + systemd targets + `legionctl` | Adopted in simpler form using ADP0 only. |
| Power mode/platform profile | johnfan and Bolekjar platform profile work | in-tree kernel + `legionctl` | Adopted through existing `/sys/firmware/acpi/platform_profile`. |
| CPU power limits | johnfan/legiond profiles, Bolekjar RAPL work | standard `intel_rapl` powercap sysfs | Adopted without DKMS RAPL/MMIO sync by default. |
| GPU performance | johnfan notes about NVIDIA power limits | `nvidia-powerd` + persistence | Adopted as firmware-managed Dynamic Boost; no forced `nvidia-smi -pl 150`. |
| VPC fan mode | in-tree `ideapad_laptop`/VPC2004 behavior | `legionctl` | Adopted as firmware fan mode `4` on AC. |
| Conflict detection | local audit findings | `legionctl doctor` | Adopted. |
| Conflict cleanup | local audit findings | `legionctl cleanup` | Adopted for old services; module/package removal requires explicit flags. |

## Candidate Features

| Area | Best source | Why it is useful | Import condition |
| --- | --- | --- | --- |
| Reliable fan RPM/temperature telemetry | Bolekjar `legion-hwmon` and johnfan `legion_hwmon` | Current `thinkpad_acpi` fan RPM is bogus on this machine. | Only after a DKMS test proves it binds without breaking `platform_profile`, VPC2004 `fan_mode`, or NVIDIA Dynamic Boost. |
| Fan table/method support | Bolekjar `legion-wmi-ftable` / `legion-wmi-fm` | Could expose real firmware fan state and possibly fix fan control visibility. | Optional DKMS branch; no custom fan curves until read-only telemetry works. |
| Fan curve read/write | johnfan fan curve implementation | Useful later for controlled profiles. | Deferred; EC fan writes are too risky until telemetry is trustworthy. |
| Maximum fan / dust mode | johnfan fan full-speed controls, Bolekjar fan method | Useful as a manual recovery/test command. | Add only as an explicit command with timeout/failsafe. |
| Hybrid graphics toggle | null-dev WMI method, Bolekjar gamezone | Useful for reboot-time MUX/hybrid state. | Later utility command; never tied to AC policy. |
| Firmware attributes | Bolekjar firmware attributes class | Useful for discoverability and future GUI/API. | DKMS import only if it does not duplicate in-tree Lenovo WMI drivers. |
| WMI event handling | Bolekjar WMI events/gamezone | Useful for Fn+Q/profile event sync. | Only after confirming it can coexist with or safely replace in-tree `lenovo_wmi_*`. |
| GUI | johnfan and Bolekjar apps | Nice but not required for stability. | Not before CLI and DKMS surfaces are stable. |
| RGB/keyboard extras | johnfan GUI, Bolekjar app modules | Unrelated to current power/fan problem. | Out of initial scope. |

## Explicit Rejections

| Area | Source | Reason |
| --- | --- | --- |
| Old `legiond` daemon | johnfan | Crashed locally and duplicated policy across services. |
| `legiond-onresume` fan hook | johnfan/local install | It wrote fan settings independently of the selected policy. |
| `legiond-cpuset` timer | johnfan/local install | Not part of the current performance target and adds another actor. |
| Blanket in-tree driver blacklists | Bolekjar packaging | Would remove drivers currently providing working platform profile and VPC2004 paths. |
| Always-on RAPL/MMIO synchronization | Bolekjar DKMS | Redundant while standard `intel_rapl` powercap works and may fight explicit limits. |
| Direct `nvidia-smi -pl 150` | johnfan-era GPU profile idea | Rejected by this system; 150 W comes from firmware mode plus `nvidia-powerd`. |
| NTFS/fstab management | local audit scripts | Not a power-management concern. |

## Current Redundancy Map

| Existing component | Keep or remove | Reason |
| --- | --- | --- |
| `power-profiles-daemon` | Keep | Desktop UI state; `legionctl` reasserts stronger firmware mode when needed. |
| `ideapad_laptop` | Keep | Provides the current VPC2004 `fan_mode` path. |
| `lenovo_wmi_gamezone` / `lenovo_wmi_events` | Keep | Part of the current working Lenovo WMI path. |
| `nvidia-powerd` | Keep | Required for Dynamic Boost/Swing behavior. |
| `intel_rapl` / `intel_rapl_msr` / `intel_rapl_mmio` | Keep | Standard CPU powercap backend. |
| `thinkpad_acpi` fan telemetry | Ignore | Reports bogus fan data on this Legion. |
| `lenovolegionlinux-*` packages | Remove | Old DKMS/userspace conflicts with this repo's ownership. |
| `LenovoLegion` Bolekjar package as-is | Do not install | Packaging blacklists working in-tree drivers and installs broad GUI/daemon stack. |
