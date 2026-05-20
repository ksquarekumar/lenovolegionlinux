# Architecture

This repo is a local policy package for one laptop:

```text
LENOVO 82WQ / Legion Pro 7 16IRX8H / LNVNB161216 / KWCN54WW
```

The package refuses to apply policy when the DMI tuple does not match the
profile unless `--force` is supplied.

## Layers

`legionctl` is the single control surface. It reads the machine profile, audits
live state, and applies AC/battery policy.

Systemd and udev only trigger `legionctl`; they do not carry duplicated policy.
This avoids the previous split where separate AC, battery, RAPL, firmware, and
GPU services could disagree.

The kernel driver lane is optional. The first package version does not install
or blacklist any DKMS driver because the in-tree Lenovo stack currently provides
working `platform_profile`, VPC2004 `fan_mode`, and NVIDIA Dynamic Boost
behavior.

## AC Policy

The AC profile writes standard interfaces first:

- `intel_rapl` and `intel_rapl_mmio` package powercap zones
- `intel_pstate` turbo and performance percentage controls
- cpufreq governor and EPP per policy
- ACPI `platform_profile`
- VPC2004 `fan_mode`
- `nvidia-powerd.service`
- NVIDIA persistence mode

The CPU target is encoded as:

```text
nominal: 55 W
long:    70 W
loaded:  70 W
short:  125 W
peak:   125 W
```

The GPU target is firmware-managed Dynamic Boost. The package starts
`nvidia-powerd` and expects the live NVIDIA limit to settle around 150 W. It
does not force `nvidia-smi -pl 150`, because that was rejected on this system.

## Reassertion

The reassert timer writes only firmware-level settings:

- `platform_profile`
- VPC2004 `fan_mode`

It does not rewrite all RAPL and CPU state every 30 seconds. Full CPU/GPU policy
is applied at boot and on ADP0 AC/battery transitions.

## Explicit Non-Goals

This package must not manage:

- `/etc/fstab`
- `/mnt/windows-system`
- `/mnt/windows-data`
- dirty NTFS repair
- CIFS or SSHFS mounts
- old `legiond` fan hooks
- `thinkpad_acpi` fan control

Those are separate system concerns and should not be coupled to power policy.

