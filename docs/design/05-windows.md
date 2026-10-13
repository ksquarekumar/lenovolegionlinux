# 05 — Dual boot and Windows

## Layout measured on 2026-10-12

| Disk | Partitions | Notes |
| --- | --- | --- |
| `nvme0n1` SK hynix P41 1 TB (PCI 06:00.0, IOMMU group 18, alone) | p1 EFI 260M, p2 MSR, p3 NTFS 256G (C:), p4 NTFS 695G (data, `/mnt/windows-data`), p5 NTFS 2G (recovery) | Not BitLocker. Windows Boot Manager is `Boot0004`. |
| `nvme1n1` Crucial P3 4 TB | ESP, `/`, swap, `/boot`, `/home`, `/data` | Linux; boots via `Boot0003` (grubx64.efi). |

Firmware: Secure Boot enabled (user mode), TPM2, ACPI `MSDM` table present (OEM Windows key).

## Hazards to detect and fix

| Hazard | Evidence | Detection from Linux | Fix |
| --- | --- | --- | --- |
| Windows writes local time to the RTC | clock jumped at boot; timesyncd "Clock change detected" at first sync | read `HKLM\SYSTEM\...\TimeZoneInformation\RealTimeIsUniversal` from the SYSTEM hive (hivex, read-only mount of p3) | `reg add ... RealTimeIsUniversal /t REG_DWORD /d 1` in Windows |
| Fast Startup leaves NTFS hibernated/dirty | `ntfs3: volume is dirty`, `mnt-windows-data.mount` failed | NTFS dirty flag and `hiberfil.sys` state; registry `HiberbootEnabled` | `powercfg /h off` or `HiberbootEnabled=0` in Windows |
| Concurrent access | any NTFS partition mounted while Windows runs (VM case) | mount table + VM state | exclusive lock: the daemon unmounts/blocks automounts before starting the VM |

Writing the Windows registry from Linux is possible with hivex but needs a read-write NTFS mount, which is unsafe
while the volume is dirty or hibernated. So fixes happen inside Windows; Linux only detects.

## Boot to Windows

`RebootWindows()` sets `BootNext=0004` via efivars (efibootmgr equivalent) and reboots. One-shot: the next
boot returns to Linux. Exposed in `legionctl`, the control center and as a polkit-guarded D-Bus method.

Built (2026-10-13, M5 part 1): `legionctl windows status` (the entry whose path is `bootmgfw.efi`, whatever its
label; a `BootNext` already set; NTFS volumes the kernel reported dirty, with the fix) and `legionctl windows reboot
[--dry-run]` (`System1.RebootWindows`, polkit action `org.legionctl.windows.reboot`): the daemon runs
`efibootmgr --bootnext <id>`, and only when that succeeds, `systemctl reboot`. The e2e test uses this machine's
captured `efibootmgr` list and checks the order and that a failed `BootNext` stops the reboot. Not run live (it
restarts the machine).
Not built yet: the registry checks (`RealTimeIsUniversal`, `HiberbootEnabled`). They need a read-only reader of the
SYSTEM hive on the raw NTFS partition (as root, no mount); the only permissive crates found are `ntfs` 0.4 (MIT or
Apache-2.0) and `notatin` (Apache-2.0, a large tree), and `nt-hive` is GPL-2.0-or-later. A test against the real
partition needs root, so this waits for the user.

## Running the installed Windows under Linux

| Option | What you get | Cost / limits |
| --- | --- | --- |
| **WinBoat** (installed, separate Windows in a container VM) | Windows apps as Linux windows via FreeRDP RemoteApp | A second Windows install; files and apps are not your real Windows |
| **Installed Windows under KVM with the NVMe passed through (VFIO)** | Same Windows, same files and apps, near-native disk speed; Windows sees the real NVMe controller | Virtual GPU only (the RTX 4080 drives the external monitor and stays with Linux), so no gaming; first boot installs virtio drivers; Windows Hello PIN may need re-setup when switching between physical TPM and swtpm |
| Raw partition via virtio-blk | Same as above without VFIO | Needs virtio storage driver in Windows before first VM boot; slower |

Recommended path for the VFIO option, given the measurements:

1. Prerequisites in Windows: Fast Startup off, RTC = UTC, virtio-win guest tools installed (network, balloon,
   display) while booted bare-metal.
2. Host: `vfio-pci` bound to `0000:06:00.0` only while the VM runs (dynamic rebind, no boot-time stub), so
   bare-metal Windows and Linux mounts of p4 keep working otherwise.
3. VM: QEMU/libvirt with OVMF Secure Boot vars, `swtpm` vTPM, host SMBIOS type 1/2 values and
   `-acpitable file=/sys/firmware/acpi/tables/MSDM` so the OEM activation still matches.
4. Guard rails in legionctl-system: refuse to start if any `nvme0` partition is mounted, if Windows is hibernated, or if
   `BootNext` points at Windows; release the device and remount automounts on shutdown.
5. Display: SPICE or RDP; seamless apps through FreeRDP RemoteApp (WinBoat's approach) against the same VM.

Open questions: does `swtpm` + Windows Hello behave acceptably for daily use; does Windows Update treat the VM
boot as a hardware change that triggers re-activation despite MSDM.

## Not in scope

A Windows-side agent is dropped (decided 2026-10-12): Lenovo Vantage already manages power and firmware settings
under Windows, and the two Windows fixes above are one-time commands.
