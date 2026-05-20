# DKMS Plan

The DKMS target is not "install Bolekjar as-is". The target is a small optional
driver package that adds only the kernel surfaces missing on this machine.

## Baseline Choice

Bolekjar's driver is the best starting point because it is modular:

- WMI gamezone
- WMI events
- WMI capability data
- WMI other methods
- WMI fan table and fan method
- hwmon
- RAPL
- RAPL MMIO
- Intel MSR sysfs
- firmware attributes

That is a better shape than johnfan's older monolithic `legion-laptop.c`, but
the current package is too broad for direct installation.

## Required Changes Before Import

1. Split the driver into feature gates.
2. Default to read-only telemetry first.
3. Do not install `blacklist-lenovo-legion.conf`.
4. Do not load or expose RAPL/MMIO/MSR controls by default.
5. Do not replace `ideapad_laptop` or in-tree `lenovo_wmi_*` until a test proves
   the replacement is strictly better on 82WQ/16IRX8H.
6. Add exact stable DMI match for this machine instead of broad model-prefix matching.
7. Build and load under the current CachyOS kernel before any install hook is
   added.

## Import Phases

Phase 1 is already in this repo: userspace policy through standard kernel
interfaces.

Phase 2 should be a separate branch with a read-only DKMS module:

- exact DMI guard
- hwmon telemetry
- WMI fan table read
- machine information sysfs
- no fan writes
- no RAPL writes
- no blacklists

Phase 3 can add explicit write commands:

- fan full-speed test with timeout
- fan controller unlock if supported and needed
- optional fan curve read/write after telemetry is proven

Phase 4 can add non-critical features:

- hybrid graphics state/toggle
- extra firmware attributes
- GUI or tray integration

## Test Gate

Before enabling a DKMS package by default, all of these must remain true:

- `platform_profile` still exposes `max-power`
- `nvidia-powerd` still raises the GPU limit to around 150 W on AC
- VPC2004 or replacement fan mode still works
- standard RAPL limits remain at `70/125/125` on AC
- suspend/resume does not revert into a conflicting profile
- no in-tree Lenovo module is blacklisted unless the replacement owns the same
  behavior
