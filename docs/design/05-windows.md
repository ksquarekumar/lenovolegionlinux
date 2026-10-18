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
The daemon (D-Bus `WindowsVmBegin`/`WindowsVmEnd`, polkit `org.legionctl.windows.vm`) starts only on proof that
Windows shut down fully, so it refuses: a BootNext into Windows; Fast Startup on, or a registry that cannot be read;
an NTFS volume of the Windows disk (its `/etc/fstab` entries) whose own `$Volume` flags say dirty, or that cannot be
read (the daemon reads the boot sector and MFT record 3 of the block device, so a volume that a VM run cleaned starts
the next run without a Linux start; doctor and `windows status` still show the kernel log's dirty lines);
`hiberfil.sys` that says hibernated, cannot be read, or has a header other than zeros or `wake` (no file: hibernation
is off); a Windows partition in use after its automount is
stopped; the GPU while Mutter or a process holds it (Mutter ignores the 4080 only after `gpu primary intel
--release-nvidia` and a new login); then it stops `nvidia-persistenced`/`nvidia-powerd`, binds both groups, gives the
caller `/dev/vfio/*` and the receiver's USB node, raises the runner's memory lock, and copies `MSDM` for the caller.
What it changed is in the ledger, so `WindowsVmEnd` (also after an error, or by a restarted daemon) undoes it: groups
back to their drivers, device nodes back to their owner and mode, services and automounts started again. A step that
fails stays in the ledger and the end fails; the next end (`windows vm stop`, or `windows vm start` first) tries it
again, and the record goes only when each step is undone. Live: the first boot is the user's (drivers for the
virtual hardware install once; the NVIDIA driver may need the GPU's VBIOS as `romfile` on a laptop GPU, error 43).

Built (2026-10-14, part 2): the user unit `legionctl-windows-vm.service` (packaged): `windows vm start` starts it
(after giving back what a crashed run left), its stop (`legionctl windows-vm-stop`, also at logout and shutdown)
sends the power button through QMP and waits, after 2 minutes ends the VM, and the runner (which ignores SIGTERM) gives
the host back; `windows vm stop` stops the unit. `windows vm autostart off|login|boot`: on writes, after one polkit
approval, `/etc/polkit-1/rules.d/49-legionctl-windows-vm-<user>.rules` (this user, this action only; one file for
each user, so one user's off keeps another user's rule; removed by off and by the package's removal) and enables the unit; boot also enables lingering. `windows vm view` opens `remote-viewer` on
the SPICE socket as a user service (the desktop's environment). `windows.vm.shares`: one virtiofsd per folder (tag:
the folder's name) and shareable memory (memfd). QMP is one module in the core crate. e2e: the autostart rule's
content, enable/disable and lingering (off mutation-checked), view refused without a VM; as root, the share's devices.

Built (2026-10-14, part 3, todo-28 3b): `gpu give nvidia linux|windows|none`. The owner is read from the facts (on
`nvidia`: Linux; on vfio-pci and `gpu0` in the VM per QMP `qom-list`: Windows; else none). To Windows, a running VM is
checked first (nothing moves without one); from Linux, the daemon's `GpuRelease` stops nvidia-persistenced and
nvidia-powerd (kept in the ledger) and binds group 17, and on a refusal starts them again and names the way out
(`gpu primary intel --release-nvidia`: the udev rule also tags the NVIDIA card `mutter-device-ignore`, from the next
login; refused with `nvidia`, before the polkit dialog); into Windows by QMP `device_add` into `gpu-port`, the other
functions first and function 0 last (it starts the slot's hot-plug); from Windows by `device_del` and a wait of 30 s
for `DEVICE_DELETED` (never forced); to Linux by `GpuReturn` (unbind, services started). The VM has an IVSHMEM device
(`windows.vm.looking_glass_mib`, 128 holds 4K; `/dev/shm/legionctl-looking-glass`), and `windows vm view` opens
`looking-glass-client` (keyboard, mouse and clipboard through the SPICE socket) while Windows has the GPU, else the
SPICE viewer. The iGPU is refused with the reason (3c). e2e (real daemon, stand-in kernel, a stand-in QMP server):
no VM refused with nothing moved (mutation-checked), Mutter's hold refused with the way out, the hot-plug order
(mutation-checked) and port, `already`, the eject and the way back to `nvidia`, `--release-nvidia` and its refusal.

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
   at boot, display), shown and changed through the API: `legionctl config`, and the app's Windows page; each
   with its default to go back to (todo-78).

The graphics stack (todo-81): legionctl checks and keeps the stack that the GPU moves depend on: Mesa (iris, anv),
the NVIDIA driver's libraries matching its kernel module, the Vulkan ICDs and EGL vendor files of both GPUs, PRIME
offload (`gpu run --gpu nvidia -- <cmd>`), switcheroo-control, Mutter's primary and ignore tags, and that sessions and
containers with `--gpu` see both GPUs through Vulkan and OpenCL. Doctor findings with their fix; e2e on recorded
machines.

## Windows care (todo-151 a-f, built 2026-10-17; topic windows-care)

Repairs change Windows only on a copy-on-write overlay, so the real disk changes only at a commit that the user asks
for and approves.

| Command | Does | Refused when |
| --- | --- | --- |
| `windows vm overlay create` | a qcow2 file (`~/.local/share/legionctl/windows/overlay.qcow2`, the disk's size, empty) and the disk's stamp (the hash of the SYSTEM hive). While it exists, every VM start runs on it: QEMU gets an emulated NVMe controller (Windows' own `stornvme`) on the overlay with the disk as its read-only backing, named on the command line (not in the overlay's header); no VFIO, no memory lock; the daemon makes the disk's block device readable (mode 0460, owner the user) for the run and gives owner and mode back after | the VM runs; an overlay exists |
| `windows vm overlay status / drop` | its size and whether Windows ran on the disk since; drop removes it (the disk as it was) | the VM runs |
| `windows vm overlay commit` | the daemon (`WindowsOverlayCommit`, polkit `org.legionctl.windows.vm`) opens the user's own regular file (no symbolic link), stops the disk's automounts, checks that nothing uses the disk, runs `qemu-img commit` on the open file (`/proc/<pid>/fd/<n>`) with the Windows disk named as the backing, and starts the automounts again; then the overlay goes | the VM (or an image) has the disk; the stamp differs (Windows ran on the disk since: bare metal or a VM without the overlay); the disk guard of a VM start (Windows hibernated, Fast Startup on, BootNext into Windows) |
| `windows vm image <file>` | the disk (only read, through the same guarded start as the VM) into a new qcow2 file, with its BLAKE2b-512 hash; a job | the VM runs; the file exists |
| `windows vm recovery` | through the guest agent `reagentc /boottore`, then `shutdown /r /t 5`: Windows restarts into WinRE in the VM; `windows vm view` (the app's Resources page: view) shows its console | no overlay; WinRE disabled (Windows' message; nothing restarts) |
| `windows repair sfc / dism / chkdsk` | `sfc /scannow`, `DISM /Online /Cleanup-Image /RestoreHealth`, `chkdsk C: /scan` through the QEMU guest agent; the output goes to a log file in Windows, read each second and recorded as `windows.repair.output` events (a job); sfc's result comes from its text (UTF-16), DISM's and chkdsk's from the exit code | no overlay; the agent does not answer in 10 s (the step fails, nothing else runs) |
| `windows repair boot` | in WinRE (no agent there), the user opens Troubleshoot > Command Prompt; legionctl types `for %d in (C .. Z without X) do @if exist %d:\Windows\System32\config\SYSTEM bcdboot %d:\Windows /f UEFI` and Enter through QMP `send-key` (a US layout); the result shows in the console | no overlay; the guest agent answers (Windows runs, not WinRE: keys would go to its desktop) |
| `windows restore list / apply <n>` | `Get-ComputerRestorePoint`, `Restore-Computer -RestorePoint <n>` through the guest agent (Windows restarts) | apply: no overlay; no such point |

Host actions from inside Windows: the VM has a virtio-serial port `org.legionctl.host.0` (QEMU connects to the
runner's socket `host.sock`) and the guest agent's port `org.qemu.guest_agent.0` (`qga.sock`). The package ships
`/usr/share/legionctl/windows/legionctl-host.ps1`: in Windows it shows "This Windows runs in a VM on Linux", and
offers VM only (Windows' own restart and shut down) or WHOLE MACHINE: shut down, restart into Linux, restart into
Windows bare metal. A request is one line (at most 64 bytes); the runner refuses anything else (logged in the unit's
journal) and passes a known action to `internal.windows_host`: legionctl asks on the Linux desktop ("Windows in the
VM asks to ... whole machine ... Allow?"); after Allow the host's own approval applies (logind for `systemctl
poweroff` and `reboot`, polkit `org.legionctl.windows.reboot` for the restart into Windows, BootNext first). The
reply line is `ok: ...` or `refused: <why>`. The restart into Windows bare metal is refused while an overlay exists.
At a host shut down or restart the VM's unit stops Windows first (the power button), so the disk is never used by the
VM and bare-metal Windows at once.

Runnables (DESIGN-13): the runner is a runnable with host access (the disk node, `/dev/vfio`, USB nodes through the
daemon), user channel "desktop question", toolverse `internal.windows_vm_begin/end` and `internal.windows_host`;
the Windows script is a guest-side runnable whose only connection is the host port. Their manifests come with M1.

Not checked live: Windows on the emulated NVMe controller and OVMF's boot entry for it (a first start may need
the firmware's boot menu), `qemu-ga` and the virtio-serial driver in this Windows (virtio-win guest tools),
PowerShell's CreateFile on the port, WinRE's keyboard layout, `Restore-Computer` as SYSTEM. Post-v1: a scheduled
health check, more repair steps.

## Not in scope

A Windows-side agent for power and firmware is dropped (decided 2026-10-12): Lenovo Vantage manages them under
Windows. The host-action script above (user decision 2026-10-16) only asks the host.
