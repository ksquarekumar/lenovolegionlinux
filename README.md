# Lenovo Legion Control

Local control package for this machine:

- `LENOVO 82WQ`
- `Legion Pro 7 16IRX8H`
- observed board values: `INVALID`, `LNVNB161216`
- observed BIOS values: `KWCN42WW`, `KWCN54WW`

This repository is intentionally machine-specific. It is not a generic Legion
support package and it does not try to guess safe behavior for other models.

## Scope

Owned here:

- AC and battery power policy
- Intel RAPL package limits through standard `powercap` sysfs
- CPU EPP/governor/turbo policy
- Lenovo firmware thermal profile through `platform_profile`
- VPC2004 firmware fan mode
- NVIDIA persistence and `nvidia-powerd` integration
- conflict/status checks

Not owned here:

- NTFS, fstab, or mount management
- GUI
- RGB
- custom fan curves
- direct `thinkpad_acpi` fan control
- blanket blacklisting of in-tree Lenovo kernel drivers
- replacing `nvidia-powerd`

## Commands

Run read-only status checks:

```bash
./src/legionctl/legionctl status
./src/legionctl/legionctl doctor
./src/legionctl/legionctl cleanup --dry-run
```

Apply the AC policy:

```bash
sudo ./src/legionctl/legionctl apply ac
```

Apply the current AC/battery mode automatically:

```bash
sudo ./src/legionctl/legionctl apply auto
```

Dry-run an apply:

```bash
./src/legionctl/legionctl apply ac --dry-run
```

Disable old local power-management units:

```bash
sudo legionctl cleanup
```

Package removal and obsolete module unloading are intentionally explicit:

```bash
sudo legionctl cleanup --unload-modules
sudo legionctl cleanup --remove-packages
```

## Install

For a direct local install:

```bash
sudo make install
sudo udevadm control --reload
sudo systemctl daemon-reload
sudo systemctl enable --now legion-apply.service legion-reassert-ac.timer
sudo systemctl start legion-apply.service
```

For Arch/CachyOS packaging:

```bash
cd packaging/arch
makepkg -si
```

Then enable the same services:

```bash
sudo systemctl enable --now legion-apply.service legion-reassert-ac.timer
sudo systemctl start legion-apply.service
```

## Current AC Targets

- CPU nominal metadata: 55 W
- CPU sustained/loaded long: 70 W
- CPU short: 125 W
- CPU peak: 125 W
- GPU: firmware-managed Dynamic Boost through `nvidia-powerd`, expected 150 W
- Firmware thermal profile: `max-power` when available
- VPC2004 fan mode: `4`
