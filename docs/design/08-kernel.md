# 08 — Kernel modules, drivers and DKMS

legionctl-system owns an explicit inventory of the kernel pieces this machine relies on, validates it on every boot and
after every kernel update, and repairs what it can (load, rebuild through DKMS, fix autoload config).

## Inventory (measured 2026-10-12, kernel 7.2.9-2-cachyos)

| Module | Source | State | Needed for | Status |
| --- | --- | --- | --- | --- |
| `ideapad_laptop` | in-tree | loaded | Fn keys (`Ideapad extra buttons`), `fn_lock`, `conservation_mode`, `camera_power` | required |
| `lenovo_wmi_gamezone`, `lenovo_wmi_other`, `lenovo_wmi_capdata`, `lenovo_wmi_events` | in-tree | loaded | `platform_profile`, firmware power limits (`ppt_*`), Fn+Q events | required |
| `acpi_call` | DKMS 1.2.2 (both kernels built) | loaded, autoloaded | firmware fan RPM through `\_SB_.GZFD.WMAE` | required (fan) |
| `msr` | built-in | n/a | throttle reasons (`IA32_*THERM_STATUS`), turbostat | required |
| `intel_rapl_msr`, `processor_thermal_rapl` | in-tree | loaded | RAPL energy counters (watts) | required |
| `nvidia`, `nvidia_modeset`, `nvidia_drm`, `nvidia_uvm` | DKMS 580.178.04 (both kernels built) | loaded | dGPU, external outputs (DP-3 is wired to NVIDIA) | required |
| `i915` | in-tree | loaded, bound to 00:02.0 | iGPU, built-in panel | required |
| `kvm_intel`, `vfio_pci`, `vfio_iommu_type1` | in-tree | loaded | WinBoat VM, installed-Windows VM | optional |
| `uinput` | in-tree | loaded | logiops virtual input | optional |
| `tcp_bbr`, `sch_fq` | in-tree | loaded | `sysctl` layer (`bbr`, `fq`) | required by configset |
| `ip_tables`, `iptable_nat` | in-tree | loaded | WinBoat networking | optional |
| `thinkpad_acpi` | in-tree | **force-loaded** by `/etc/modules-load.d/acpi.conf` | nothing; reports a bogus fan and logs "secondary fan control detected & enabled" on a Legion EC | **remove from autoload** (not a blanket blacklist; it stays loadable) |
| `wl` | DKMS `broadcom-wl-dkms` | loaded | nothing; Wi-Fi is Intel `iwlwifi` | **remove package** |
| `xe` | in-tree | loaded, unbound | nothing (iGPU is on `i915`) | harmless; leave |

## Autoload and option drift found

| File | Problem |
| --- | --- |
| `/etc/modules-load.d/acpi.conf` | forces `thinkpad_acpi` |
| `/etc/modules-load.d/kvm.conf` | `kvmgt`, `mdev`: Intel GVT-g, not supported on 13th-gen iGPUs |
| `/etc/modules-load.d/vfio.conf` | `vfio_mdev` no longer exists → "Failed to find module" every boot |
| `/etc/modules-load.d/virtio.conf` | guest-side virtio drivers on a host; `virtio_serial` missing → boot error |
| `/etc/modules-load.d/vmware.conf` | `vmmon` not installed → boot error |
| `/etc/modules-load.d/openlogi.conf` | leftover from removed OpenLogi (duplicate `uinput`) |
| `/etc/modprobe.d/intel.conf` | `i915 enable_gvt=1` (GVT-g unsupported here) |
| `/etc/modprobe.d/blacklist.conf` | `blacklist ecdh_generic`, an old Bluetooth workaround; verify Bluetooth (MX Keys S) is unaffected before removing |
| `/usr/lib/modules/{6.8.9,6.9.3,6.10.5}*` | module trees of removed kernels, unowned (6.9.3 holds 6.5 MB) |

## Design

- The profile (later a configset) declares modules as `required`, `optional` or `unwanted`, each with the
  feature that needs it, plus the DKMS packages that provide out-of-tree ones.
- `doctor` / legionctl-system health check, per installed kernel (`/usr/lib/modules/*` owned by a package):
  - required module available for that kernel (in-tree, built-in, or DKMS built — `dkms status`);
  - loaded on the running kernel when its feature is enabled;
  - unwanted modules not autoloaded; autoload files reference only modules that exist;
  - module options that refer to features this hardware lacks.
- Repair (root, explicit, previewed like any other plan):
  - `dkms autoinstall -k <kernel>` for a missing build; surface the build log path on failure;
  - `modprobe` for a required module that is available but not loaded;
  - a single owned autoload file `/usr/lib/modules-load.d/legion.conf` for what the configset needs; foreign
    autoload files are reported, never edited silently.
- A pacman hook (`/usr/share/libalpm/hooks/90-legion-modules.hook`, `PostTransaction` on `usr/lib/modules/*`)
  runs the check after kernel or DKMS updates, so a kernel upgrade that breaks `acpi_call` or `nvidia` is
  reported before the next reboot instead of after it. DKMS's own hooks still do the building.
- Secure Boot is on (user mode): DKMS modules must be signed with an enrolled key. The check verifies
  `modinfo -F signer` for out-of-tree modules and reports unsigned ones.

## Cleanup decided 2026-10-12

| Item | Action |
| --- | --- |
| `thinkpad_acpi` autoload | removed from `acpi.conf` (module stays loadable) |
| `broadcom-wl-dkms` | package removed |
| `kvm.conf` | `kvmgt`, `mdev` removed (GVT-g unsupported on 13th gen) |
| `vfio.conf` | `vfio_mdev` removed (merged into the vfio core long ago) |
| `virtio.conf` | guest drivers replaced by the host-side accelerators `vhost_net`, `vhost_vsock` |
| `vmware.conf` | removed: VMware is not installed, so `vmmon` cannot exist |
| `openlogi.conf` | removed with OpenLogi |
| `intel.conf` | `enable_gvt=1` dropped; initramfs rebuilt because `i915` is in `MODULES` |
| `/usr/lib/modules/{6.8.9,6.9.3,6.10.5}*` | deleted (index files and a build cache only; no kernel images in `/boot`) |

## Initramfs ownership (decided 2026-10-12)

legionctl/legionctl-system manages the initramfs module set as part of the GPU mode, without editing files it does not own:

- Owned drop-in: `/etc/mkinitcpio.conf.d/90-legion.conf`. It sorts after CachyOS's auto-generated `10-chwd.conf`
  (`MODULES+=(nvidia nvidia_modeset nvidia_uvm nvidia_drm)`), so its plain `MODULES=(...)` assignment wins without
  touching chwd's file.
- Hybrid mode (panel on i915): `MODULES=(i915)`. NVIDIA loads from `modules-load.d` before GDM. Measured: 300 MB →
  191 MB with only the main-file change (chwd still added NVIDIA), smaller once the drop-in overrides chwd.
- Discrete-only mode (panel on NVIDIA): `MODULES=(nvidia nvidia_modeset nvidia_uvm nvidia_drm)` so the panel and
  splash have early KMS. Switching GPU mode rewrites the drop-in and rebuilds before asking for the restart.
- Rebuild one kernel at a time (`mkinitcpio -p <preset>`), verify the image exists, its size, that the kernel was
  re-signed by the `kernel-sbsign` post hook (Secure Boot via shim + rEFInd key), then rebuild the next. The other
  kernel is the fallback until the first boots successfully.
- The machine boots rEFInd via shim; kernel options live in `/boot/refind_linux.conf`, not `/etc/default/grub`.

## Boot takeover (built 2026-10-14, todo-80; decided by the user: systemd-boot + UKI)

The chain here: the firmware entry "Linpus lite" starts shim (`/boot/efi/EFI/BOOT/bootx64.efi`), shim starts rEFInd
(`grubx64.efi`, signed with the "Locally-generated rEFInd key" that MOK trusts), rEFInd starts the CachyOS kernels.
`legionctl boot install` adds, beside it and without changing it: `EFI/legionctl/` with a copy of shim and MokManager
and systemd-boot as shim's second stage (`grubx64.efi`, signed by `sbsign` with `/etc/refind.d/keys/refind_local.key`),
one UKI per kernel in `/boot` (`EFI/Linux/legionctl-<kernel>.efi` by `ukify`, signed with the same key, the microcode
and the initramfs, and the running system's kernel line from `/proc/cmdline` without `initrd=`), `loader/loader.conf`
(the running kernel, from rEFInd's `initrd=`, as default; no menu: hold a key at start for it), and a firmware entry
`legionctl` made with `--create-only` (not first in the order); then BootNext tries it once, so a failed start falls
back to the old chain. `boot confirm` puts the entry first only when this start came through it; `boot rebuild` (also
the pacman hook `95-legionctl-uki.hook`, after mkinitcpio's) makes the UKIs again and removes those of kernels that
are gone; `boot remove` takes the entry, the folder, the UKIs and our `loader.conf` away. The daemon (D-Bus `Boot`,
polkit `org.legionctl.boot`, no kept authorization) carries a plan out step by step and stops at the first failure,
so no entry follows an unsigned loader. Windows stays reachable through `windows reboot` (BootNext into its entry) and
the firmware menu: systemd-boot cannot start the boot manager on the other disk's ESP. e2e (real daemon, a fake ESP,
a stand-in efibootmgr with its own state, stand-in ukify/sbsign): the dry run changes nothing; install signs the loader
and each kernel's image with the kernel line, adds the entry not first and sets BootNext (mutation-checked); confirm
is refused unless this start came through it (mutation-checked); rebuild drops a removed kernel's image; remove leaves
the old chain as it was. Live: the first install and the test start are the user's.
