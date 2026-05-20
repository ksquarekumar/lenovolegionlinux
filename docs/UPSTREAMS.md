# Upstream Review

The repo is informed by three deprecated or stale Legion projects, but does not
blindly combine them.

## johnfanv2/LenovoLegionLinux

Source: <https://github.com/johnfanv2/LenovoLegionLinux>

Useful as historical reference for Legion feature names, fan curves, and older
userspace behavior.

Not used as the base package because:

- the old `legiond` service crashed locally
- the old `legion-laptop` DKMS driver needed kernel 7 patching
- the driver did not bind cleanly on this system because the EC device is owned
  by the kernel ACPI EC path
- its userspace duplicates policy that now belongs in `legionctl`

## null-dev/lenovo-legion-wmi-dkms

Source: <https://github.com/null-dev/lenovo-legion-wmi-dkms>

Useful as a compact WMI reference, especially for hybrid graphics methods.

Not part of phase one because hybrid graphics is not the current stability
problem and the kernel surface is too narrow to replace the old stack.

## bolekjar/LenovoLegion

Source: <https://github.com/bolekjar/LenovoLegion>

Best candidate for a future DKMS baseline because the driver is modular and
models newer Lenovo WMI interfaces, RAPL helpers, hwmon, fan methods, and
firmware attributes.

Not imported in phase one because its package blacklists in-tree Lenovo drivers
such as `ideapad_laptop` and `lenovo_wmi_*`. On this machine, those in-tree
drivers are currently part of the working path for `platform_profile`,
VPC2004 `fan_mode`, and NVIDIA Dynamic Boost behavior.

## Policy

The local package should prefer existing kernel interfaces until they are proven
insufficient:

- keep `power-profiles-daemon` for desktop UI state
- keep in-tree Lenovo WMI/platform drivers
- keep `nvidia-powerd`
- keep standard `intel_rapl`/powercap

DKMS should only be introduced behind an explicit package option after it proves
it improves fan telemetry or control without regressing the working platform
profile and GPU power behavior.

