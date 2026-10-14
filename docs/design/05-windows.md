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
Built (2026-10-14, decision D3): the registry checks. `windows status` reads the SYSTEM hive from an NTFS volume of the
mount table or of `/etc/fstab` (this machine has `/mnt/windows-system`: ntfs3, read-only, automount, readable by the
user), so no raw reader and no root. Its own hive reader (`legionctl-machine` `hive`, read-only, every offset checked)
takes the control set of `Select\Current` and reads `RealTimeIsUniversal` and `HiberbootEnabled`; a missing value is
Windows' default (local time, Fast Startup on). Each finding is a hazard with its fix in Windows. Without such a volume
the output says `registry: not checked`. Not replayed: the hive's transaction logs (a value written just before a hard
stop can be missing). Test: a generated hive whose control sets 1 and 2 disagree (a read of the wrong set fails it),
the good values, and a damaged file. Live on this machine (2026-10-14): Windows keeps the RTC in local time (the
cause of the clock jump above); Fast Startup is off.

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

## The Windows VM (decided by the user 2026-10-14; todo-28)

`legionctl windows vm start|stop|status` boots the installed Windows in a VM: the SK hynix NVMe (`0000:06:00.0`,
IOMMU group 18) and the RTX 4080 with its audio function (group 17) through VFIO, so Windows sees its own disk and
GPU; the picture is on the external monitor (all external ports are on the 4080) and the sound on its HDMI/DP audio.
Defaults (`windows.vm`, user's choice): 12 vCPUs on 8 P-core threads and 4 E-core threads (QEMU is confined to
those CPUs; Linux keeps the other 20), 24 GiB (VFIO locks all of it: start is refused below 24 GiB + 2 GiB available,
naming the largest processes; legionctl never stops WinBoat), the Logi Bolt receiver `046d:c548` through USB
passthrough (the laptop keyboard stays with Linux), QEMU user network (e1000e, no driver needed), OVMF (no SMM: this
kernel's KVM resets an SMM guest), `swtpm` as TPM 2.0, the host's SMBIOS type 1/2 and the `MSDM` table (activation).
The daemon (D-Bus `WindowsVmBegin`/`WindowsVmEnd`, polkit `org.legionctl.windows.vm`) refuses: a BootNext into
Windows; Fast Startup on or `hiberfil.sys` that says hibernated; a Windows partition in use after its automount is
stopped; the GPU while Mutter or a process holds it (Mutter ignores the 4080 only after `gpu primary intel
--release-nvidia` and a new login); then it stops `nvidia-persistenced`/`nvidia-powerd`, binds both groups, gives the
caller `/dev/vfio/*` and the receiver's USB node, raises the runner's memory lock, and copies `MSDM` for the caller.
What it changed is in the ledger, so `WindowsVmEnd` (also after an error, or by a restarted daemon) undoes it: groups
back to their drivers, services and automounts started again. Live: the first boot is the user's (drivers for the
virtual hardware install once; the NVIDIA driver may need the GPU's VBIOS as `romfile` on a laptop GPU, error 43).

## Boot, VM and GPU journeys (for review, 2026-10-14; todo-28, todo-80, todo-81)

GPUs have one owner each: `linux`, `windows` or `none` (on vfio-pci, used by no one). The iGPU drives the laptop
panel (hybrid MUX); the RTX 4080 drives every external port (DP-1..3, HDMI). Both OSes always keep a virtual display:
Windows a QXL/SPICE display, Linux its console and SSH (simpledrm). Apps on a GPU that leaves lose it (they may crash).

GPU paths (`legionctl gpu give <igpu|nvidia> <linux|windows|none>`; the first live run of each is the user's):

| Path | Steps | Refused when | Fallback |
| --- | --- | --- | --- |
| nvidia: linux -> none | Mutter told to ignore the 4080 (udev `mutter-device-ignore`; Mutter drops a GPU that goes away), NVIDIA services stopped, `nvidia` unbound, bound to vfio-pci | a process holds a node of group 17 (named) | the 4080 stays with Linux |
| nvidia: none -> windows | QMP `device_add vfio-pci` into the VM's free root port (`gpu-port`); Windows loads its driver; the external monitor shows Windows; `windows vm view` shows it on the panel (Looking Glass) | no Windows VM runs | the 4080 stays `none` |
| nvidia: windows -> none | QMP `device_del`: Windows gets an eject request, releases the GPU, QEMU reports `DEVICE_DELETED`; never forced | Windows does not release it in 30 s (an app holds it: close it) | the 4080 stays with Windows |
| nvidia: none -> linux | `driver_override` cleared, probed: `nvidia` binds; services started; Mutter takes the GPU for the external monitor; PRIME offload for apps | - | `none` |
| nvidia at boot | `windows.vm.gpu_at_boot`: `linux` (today), `none` (vfio-pci claims it in the initramfs, before `nvidia` loads), `windows` (`none`, then the VM takes it at its start) | - | - |
| igpu: linux -> windows (shared) | an SR-IOV virtual function of the iGPU (an out-of-tree i915 SR-IOV driver as DKMS) to the VM; Linux keeps the iGPU and the panel | the driver does not support this kernel | no shared iGPU |
| igpu: linux -> windows (whole) | only when the Linux desktop is not on the iGPU (it then has the 4080's external monitor or the console); the panel goes to Windows | the desktop is on the iGPU | - |
| MUX discrete (firmware, restart) | the panel on the 4080; the iGPU has no output and can go to Windows whole | - | - |

Not possible here: a time-sliced share of the 4080 (no SR-IOV; NVIDIA vGPU is data-center only), moving a running app
between GPUs, one virtual display that renders for both OSes (no paravirtual 3D driver for Windows guests).

Journeys:

1. **Every boot**: the firmware starts systemd-boot (no wait) and the Linux UKI (todo-80); GDM on the iGPU. With
   `windows.vm.autostart` = `login` (or `boot`: lingering user service, before login), the Windows VM starts by itself
   after the guards pass, with SPICE; `off` waits for `legionctl windows vm start`. The 4080's owner at that point is
   `windows.vm.gpu_at_boot`.
2. **Use Windows on the laptop screen**: `legionctl windows vm view` opens the SPICE viewer, or Looking Glass when
   Windows has the 4080. Clipboard: SPICE's agent or Looking Glass; audio: PipeWire (and the 4080's HDMI/DP audio with
   the GPU); keyboard and mouse: the viewer, plus the Bolt receiver through USB passthrough.
3. **Play or render in Windows**: `legionctl gpu give nvidia windows` (P1 + P2); `gpu give nvidia linux` gives it back.
4. **Bare-metal Windows** (anti-cheat games, firmware updates): `legionctl windows reboot`; refused while the VM runs;
   the VM is refused while a one-time boot into Windows is set.
5. **Shut down Linux**: the VM's service sends the power button first and waits up to 2 minutes; then the host is
   given back.
6. **Storage**: Windows owns its whole SK hynix NVMe through VFIO while the VM runs (nothing on the Linux side);
   Linux folders go to Windows with virtiofs (`windows.vm.shares`; Windows needs the virtio-win drivers and WinFsp
   once).
7. **Settings**: every VM setting is config (`windows.vm`: CPUs, memory, reserve, USB devices, shares, autostart, GPU
   at boot, display), shown and changed through the API: `legionctl config`, the app's Windows page and the TUI; each
   with its default to go back to (todo-78).

The graphics stack (todo-81): legionctl checks and keeps the stack that the GPU moves depend on: Mesa (iris, anv),
the NVIDIA driver's libraries matching its kernel module, the Vulkan ICDs and EGL vendor files of both GPUs, PRIME
offload (`gpu run --gpu nvidia -- <cmd>`), switcheroo-control, Mutter's primary and ignore tags, and that sessions and
containers with `--gpu` see both GPUs through Vulkan and OpenCL. Doctor findings with their fix; e2e on recorded
machines.

## Not in scope

A Windows-side agent is dropped (decided 2026-10-12): Lenovo Vantage already manages power and firmware settings
under Windows, and the two Windows fixes above are one-time commands.
