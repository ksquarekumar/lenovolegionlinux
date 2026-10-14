# 08 — Kernel modules, drivers and DKMS

legionctl-system owns an explicit inventory of the kernel pieces this machine relies on, validates it on every boot and
after every kernel update, and repairs what it can (load, rebuild through DKMS, fix autoload config).

## Inventory (measured 2026-10-12, kernel 7.2.9-2-cachyos)

| Module | Source | State | Needed for | Status |
| --- | --- | --- | --- | --- |
| `ideapad_laptop` | in-tree | loaded | Fn keys (`Ideapad extra buttons`), `fn_lock`, `conservation_mode`, `camera_power` | required |
| `lenovo_wmi_gamezone`, `lenovo_wmi_other`, `lenovo_wmi_capdata`, `lenovo_wmi_events`, `lenovo_wmi_hotkey_utilities` | in-tree | loaded | `platform_profile`, firmware power limits (`ppt_*`), Fn+Q events, mute LEDs | **replaced** by `legionctl_wmi` (below); the package blacklists them |
| `legionctl_wmi` | DKMS, this repository (`kernel/legionctl-wmi`) | loaded after the first live load (the user's) | the same ABI as the five drivers above, and the fan table, fan max mode and full speed | required |
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
and the initramfs, and the kernel's own command line: see "Kernels" below), `loader/loader.conf`
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

## Kernels (built 2026-10-14, todo-84 parts 1 and 2; decided by the user)

Two kernels are installed: `linux-cachyos` (EEVDF scheduler, thin LTO) and `linux-cachyos-bore-lto` (BORE, thin LTO).
rEFInd starts `linux-cachyos-bore-lto`. legionctl starts a kernel only through the boot takeover's systemd-boot
entries. There is no rEFInd path and no kexec.

| Command | What it does |
| --- | --- |
| `legionctl kernel list` | Each installed kernel (`/usr/lib/modules/<release>/` with `vmlinuz`): the package (`pkgbase`), the release, the scheduler (`bore` when `CONFIG_SCHED_BORE=y`, else `eevdf`) and `lto` (`CONFIG_LTO_CLANG_*`), from `build/.config` of the headers (the name is not enough: `linux-cachyos` is LTO too), legionctl's image, the command line, the DKMS state of `nvidia` and `legionctl-wmi` for that release (`dkms status`), and the marks `running` and `default`. |
| `legionctl kernel default <name>` | legionctl's `loader.conf` starts `<name>` from the next start on. |
| `legionctl kernel try <name>` | The next start only: the EFI variable `LoaderEntryOneShot` (systemd-boot's one-time entry) and `BootNext` to legionctl's firmware entry, then the restart. The default stays. |
| `legionctl kernel cmdline set <name> "<line>"` | The command line of `<name>` only (see below). |
| `legionctl kernel cmdline undo <name>` | The line of `<name>` before its last change. A second undo goes forward again. |

`default`, `try` and `cmdline` refuse, with nothing changed, when the boot takeover is not installed ("install the
boot takeover first (legionctl boot install)"), and when `<name>` is not a kernel in `/boot` or has no legionctl
image. Each change goes through the daemon (D-Bus `Kernel(action, name, line)`, polkit `org.legionctl.boot`, no
kept authorization). The operator makes the plan first, so a refusal comes before the polkit dialog; `--dry-run`
shows the plan. The daemon carries the plan out step by step and stops at the first failure. For `try`, the restart
comes only after each step before it worked.

`try` writes the variable to efivarfs itself (attributes non-volatile, boot and run time, the value as UTF-16 with
its end mark). `bootctl set-oneshot` refuses when this start did not come through systemd-boot (no `LoaderInfo`),
and the first try after `boot install` comes through rEFInd. An earlier one-time entry that systemd-boot did not use
is immutable in efivarfs: the daemon removes the immutable flag (`chattr -i`) first.

### The command line of each kernel

Each kernel has its own command line in `/etc/legionctl/kernel/<name>.cmdline`. That file is the one source of the
line in the kernel's image: `boot install`, `boot rebuild` (and the pacman hook) and `kernel cmdline set` read it.
When a kernel has no file yet (`boot install`, or a new kernel package), the plan writes the running system's line
(`/proc/cmdline` without `initrd=` and `BOOT_IMAGE=`) into it. rEFInd's `/boot/refind_linux.conf` stays as it is: it
is the old chain's line.

`kernel cmdline set` checks the line before it writes anything:

- The line must have `root=`.
- Each `<module>.<parameter>` must name a module that this kernel has: built in (`modules.builtin`,
  `modules.builtin.modinfo`) or loadable (`modules.dep`, with the DKMS modules). The kernel ignores a parameter of an
  unknown module without a word, so `zwsap.enabled=0` is refused. Names that the kernel gives to user space (`rd.`,
  `systemd.`, `udev.`, `luks.`, `plymouth.`, `vconsole.`, `locale.`) are not modules.
- The parameter names are not checked: `modules.builtin.modinfo` does not list each parameter. On 7.2.9 it lacks 15
  of 256 parameters of built-in modules that `/sys/module` shows, `zswap.enabled` of this machine's line among them.
  Parameters without a dot are not checked either: the kernel gives the ones it does not know to `init`.

Then the daemon makes that kernel's image only: `ukify` writes `legionctl-<name>.efi.new`, `sbverify --cert` with
the MOK certificate checks its signature, and `mv` puts it in place of the old image. Only after that, the old line
goes to `<name>.cmdline.old` and the new line to `<name>.cmdline`. A failed step leaves the old image and the old
line. `boot rebuild` uses the same checks and steps for each kernel.

### The `kernel` condition of the configsets

A configset layer can apply to one kernel: `{ "when": { "kernel": "linux-cachyos" }, "set": { ... } }`, or
`legionctl rules edit 50-plain --when kernel=linux-cachyos sysctl.vm.swappiness=70`. The value is the package of the
running kernel (01-core.md, Configsets). A layer for a kernel that is not installed is not used, and `doctor` names
it.

### Tests (e2e, `crates/legionctl/tests/machine.rs`)

On the recorded machine (its `pkgbase` files and the scheduler and LTO lines of each `build/.config` are recorded),
with the real daemon, a stand-in ESP, a stand-in `efibootmgr` with its own state, and stand-in `ukify`, `sbsign`,
`sbverify`, `dkms` and `systemctl`:

- `kernels_are_listed_and_start_through_the_boot_takeover_once_or_by_default`: the list (both kernels, flavour,
  DKMS, running, default); `default` and `try` refused with nothing changed before `boot install`; `default` changes
  `loader.conf` only; `try` sets `LoaderEntryOneShot` and `BootNext` and restarts, and the default stays.
- `a_kernels_command_line_goes_into_its_image_only_and_undo_puts_the_old_one_back`: the line goes into that
  kernel's image only; a line without `root=`, a parameter of an unknown module and an unsigned image are refused
  with the old image and line left; `boot rebuild` keeps the line; undo puts the old line back.
- `a_kernel_layer_applies_only_on_its_kernel`: a `kernel` layer applies only while its kernel runs; a layer for a
  kernel that is not installed is a `doctor` finding.

Live: the boot takeover is not installed on this machine yet. The first `boot install`, `kernel try` and
`kernel default` are the user's.

## legionctl-wmi: our Legion WMI module (todo-79 part 1, decided by the user 2026-10-14)

`kernel/legionctl-wmi/` is a DKMS kernel module (GPL-2.0, kernel coding style, `checkpatch --strict` clean). It owns
the whole Legion WMI of this laptop (82WQ, BIOS KWCN54WW). It replaces `lenovo_wmi_gamezone`, `lenovo_wmi_other`,
`lenovo_wmi_capdata`, `lenovo_wmi_events` and `lenovo_wmi_hotkey_utilities`. After the daemon switch (a later part),
it also replaces the fan calls through `acpi_call`, and the daemon then uses sysfs only. Until that switch, the daemon
continues to use `acpi_call` for the fans.

### Why our own module

- The upstream drivers give no fan table, no fan max mode and no full speed. Today these go through `acpi_call`,
  which lets root call any ACPI method. The module makes only the calls below, checks each value in the kernel and
  sets again what the firmware forgets (after resume, and on each entry to custom mode).
- One owner: after the switch, the module is the only writer of the GameZone, Other Mode and fan methods. It makes
  one call at a time.
- The ABI that the daemon reads today does not change: the same `platform_profile` and the same `lenovo-wmi-other-0`
  firmware attributes. Thus the blacklist is safe.

### What it binds (the code cites the source of each id)

| GUID | Object | Use | Source |
| --- | --- | --- | --- |
| `887B54E3-DDDC-4B2C-8B88-68A26A8835D0` | WMAA | GameZone: thermal mode 43/44/45, fan max mode 0x0D | upstream wmi-gamezone.c; 0x0D measured |
| `DC2A8805-3A8C-41BA-A6F7-092E0089CD3B` | WMAE | Other Mode: GetFeatureValue 17, SetFeatureValue 18 | upstream wmi-other.c |
| `7A8F5407-CB67-4D6E-B547-39B3BE018154` | WQA5 | Capability Data 01: supported, default, step, min, max | upstream wmi-capdata.c |
| `92549549-4BDE-4F06-AC04-CE8BF898DBAA` | WMAB | Fan_Get_Table 5, Fan_Set_Table 6 | LLL `model_kwcn` |
| `D320289E-8FEA-41E0-86F9-911D83151B5F` | event 0xE7 | thermal mode change (Fn+Q) | upstream wmi-events.c |
| `CE6C0974-0407-4F50-88BA-4FC3B6559AD8` | WMSK | micmute and mute LEDs | upstream wmi-hotkey-utilities.c |

The GameZone device is the hub. It binds after the Other Mode, Capability Data 01 and fan method devices (probe
deferral), and device links unbind it before them. When it binds, the module reads the thermal mode, the capability
data and the current values. It writes nothing.

The Fn keys stay with `ideapad_laptop` (`VPC2004`). No upstream driver makes an input device from the Legion GUIDs:
the events driver gives the thermal mode change only, and `lenovo_wmi_hotkey_utilities` gives the mute LEDs only.
The module does the same. The GameZone key event `10AFC6D9-...` stays unbound because its data is not known (open
item).

### sysfs ABI

| Path | Read | Write |
| --- | --- | --- |
| `/sys/class/platform-profile/platform-profile-<n>/profile` (`name`: `lenovo-wmi-gamezone`; the profile's `paths.platform_profile`) and `/sys/firmware/acpi/platform_profile` | `low-power` (quiet), `balanced`, `performance`, `max-power` (0xE0), `custom` (0xFF) | the same; only the class device takes `custom` (the kernel's rule for the legacy file); a change notice (poll) for each Fn+Q |
| `/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes/<name>/` | `current_value` (of the current mode), `min_value`, `max_value`, `default_value`, `scalar_increment`, `display_name`, `type` | `current_value`: within min-max (else `EINVAL`), custom mode only (else `EBUSY`) |
| hwmon `legionctl_wmi`: `fan1_input`, `fan2_input` | RPM (Other Mode 0x04030001/2) | - |
| hwmon `pwm1_auto_point1_pwm` .. `pwm1_auto_point10_pwm` | the firmware's table (Fan_Get_Table) | one level 1-10 per point; custom mode only (`EBUSY`); a point below its minimum {1,1,1,1,1,1,1,1,3,5}, above 10, not a number, or a table that decreases gives `EINVAL`; the module reads the table back after each write (`EIO` on a difference) |
| hwmon `pwm1_enable` | `2` (the table), `0` (full speed) | `0` or `2` (else `EINVAL`); custom mode only (`EBUSY`) |
| hwmon `fan_max_mode` | the state that the module set (the firmware has no read) | `0` or `1`, in all modes |
| `/sys/class/leds/platform::micmute`, `platform::mute` | brightness | 0 or 1 |

`<name>` is one of `cpu_temp`, `gpu_nv_ac_offset`, `gpu_nv_ctgp`, `gpu_nv_ppab`, `gpu_temp`, `ppt_cpu_cl`,
`ppt_pl1_spl`, `ppt_pl1_tau`, `ppt_pl2_sppt`. The module shows a name only when Capability Data 01 marks it
supported. Each `EBUSY` writes a kernel warning that names the mode, for example `the fan table works in custom mode
only; the mode is balanced (0x2)`.

Fan table points: the pwm values are levels 1-10, not 0-255 (one table for both fans, 02-platform.md). A write
changes one point, and the table must stay valid after each write. To set a new table, write in two passes: first
from point 10 down to point 1 with the higher of the old and the new level, then from point 1 up to point 10 with the
new level. Each table between the two is then valid.

What the module sets again: after resume, fan max mode (when it is on). After resume, on each entry to custom mode
through `platform_profile` and on each Fn+Q into custom mode: the last table written through sysfs and full speed
(when it is on). When the mode leaves custom, full speed is off (the firmware stops it).

### Build, package and test

- `make kmod` runs checkpatch and builds the module against `KDIR` (default: the headers of the running kernel). It
  loads nothing. `make kmod-checkpatch` runs checkpatch only (CI, with checkpatch of v7.2).
- The module uses the WMI buffer API (`wmidev_invoke_method`, `notify_new`) of Linux 7.0 and later. DKMS does not
  build it for older kernels (`BUILD_EXCLUSIVE_KERNEL_MIN`).
- The package installs the source to `/usr/src/legionctl-wmi-<version>/` (DKMS builds it for each installed kernel)
  and the blacklist of the five upstream drivers to `/usr/share/legionctl/legionctl-wmi.conf` only (review on
  merge, 2026-10-14): a package install must not make an untested module replace the upstream drivers at the next
  start. The blacklist goes to `/etc/modprobe.d` in step 6 below, after the user's first live load; until then the
  profile keeps requiring the upstream drivers.
- Secure Boot is on: DKMS must sign the module with the enrolled key, as for `acpi_call` and `nvidia`.
- Test (`crates/legionctl/tests/e2e.rs`, `the_legionctl_wmi_module_gives_the_legion_wmi_through_sysfs`, also in the
  CI VM step): a Debian VM with the generic 7.x kernel builds the module with DKMS. A stand-in of the firmware
  (`tests/fixtures/legionctl-wmi/ssdt.asl`: the same GUIDs, object ids and buffer layouts) is an ACPI table that
  QEMU gives (`-acpitable`). The guest installs the module and the blacklist, then restarts: udev loads the drivers
  as on the real machine. The stand-in prints each call that it gets as an AML Debug line, and a test helper
  (`aml-call`) calls its Fn+Q method as the EC does. The guest script (`run.sh`) makes 46 checks through sysfs: the
  blacklist, the profile and its Fn+Q notice, the attribute bounds and modes, the fan table byte for byte as the
  firmware gets it, the refusals, the table again after a mode change, after Fn+Q and after resume (`pm_test`), full
  speed, fan max mode, the LEDs, and a clean unload.

### First live load (the user's, todo-17 gate)

The daemon continues to use `acpi_call` for the fans until the switch part. The first load on this machine is the
user's:

1. Build and install the package (`make package install-package`). Do not restart yet. Make sure that DKMS built
   and signed the module for each kernel: `dkms status legionctl-wmi`, `modinfo -F signer legionctl_wmi`.
2. Try it without a restart:
   `sudo modprobe -r lenovo_wmi_other lenovo_wmi_gamezone lenovo_wmi_events lenovo_wmi_capdata lenovo_wmi_hotkey_utilities`,
   then `sudo modprobe legionctl_wmi`. Read `sudo dmesg | tail`.
3. Read only first: `cat /sys/firmware/acpi/platform_profile`,
   `grep . /sys/class/firmware-attributes/lenovo-wmi-other-0/attributes/*/*`, and the hwmon `legionctl_wmi`
   (`fan1_input`, `fan2_input`, `pwm1_auto_point*_pwm`). Compare with the values in 02-platform.md.
4. Set custom mode (`echo custom | sudo tee /sys/class/platform-profile/platform-profile-0/profile`). With
   `legionctl monitor` open, write `pwm1_auto_point1_pwm` .. `pwm1_auto_point10_pwm` (the levels of the profile's
   default curve), read them back, and make sure that the RPM follows the ladder. Write `pwm1_enable` 0, then 2.
   Write `fan_max_mode` 1, then 0. Push Fn+Q and make sure that `platform_profile` follows.
5. To go back: `sudo modprobe -r legionctl_wmi`, then
   `sudo modprobe lenovo_wmi_gamezone lenovo_wmi_other lenovo_wmi_events lenovo_wmi_hotkey_utilities`.
6. Only when steps 3 and 4 are right: `sudo install -m644 /usr/share/legionctl/legionctl-wmi.conf /etc/modprobe.d/`,
   restart, and `lsmod | grep -E 'legionctl_wmi|lenovo_wmi'` shows `legionctl_wmi` only. The switch part then moves
   the profile and the daemon to the module (no acpi_call). To go back for good: remove that file and restart.
