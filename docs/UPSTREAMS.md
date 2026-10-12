# Upstream Review

The repo is informed by three deprecated or stale Legion projects, but does not
blindly combine them.

## johnfanv2/LenovoLegionLinux

Source: <https://github.com/johnfanv2/LenovoLegionLinux>

Useful as historical reference for Legion feature names, fan curves, older
userspace behavior, power-profile integration, and fan-controller recovery
concepts. Its extra ACPI examples also identify `Fn+R` as a refresh-rate
toggle idea, but that path is not imported directly because it is marked
unfinished and hardcodes display modes.

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
firmware attributes. It is also the main source for WMI event, keyboard
lighting/RGB, G-Sync, overdrive, Win-key, touchpad, and charge-mode method
names.

Not imported in phase one because its package blacklists in-tree Lenovo drivers
such as `ideapad_laptop` and `lenovo_wmi_*`. On this machine, those in-tree
drivers are currently part of the working path for `platform_profile` and
NVIDIA Dynamic Boost behavior. The in-tree VPC2004 `fan_mode` path is visible
but ineffective as a control path on this machine.

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

See [REFERENCE_COMPARISON.md](REFERENCE_COMPARISON.md) for the baseline-vs-
upstreams capability map, [CAPABILITIES.md](CAPABILITIES.md) for the machine
capability inventory, [FEATURE_MATRIX.md](FEATURE_MATRIX.md) for the active
adoption/rejection list, and [DKMS_PLAN.md](DKMS_PLAN.md) for the kernel-driver
import gate.
