# 08 — Kernel modules, drivers and DKMS

legionctl-system owns an explicit inventory of the kernel pieces this machine relies on, validates it on every boot and
after every kernel update, and repairs what it can (load, rebuild through DKMS, fix autoload config).

## Inventory (measured 2026-10-12, kernel 7.2.9-2-cachyos)

| Module | Source | State | Needed for | Status |
| --- | --- | --- | --- | --- |
| `ideapad_laptop` | in-tree | loaded | Fn keys (`Ideapad extra buttons`), `fn_lock`, `conservation_mode`, `camera_power` | required |
| `lenovo_wmi_gamezone`, `lenovo_wmi_other`, `lenovo_wmi_capdata`, `lenovo_wmi_events`, `lenovo_wmi_hotkey_utilities` | in-tree | not loaded (blacklisted by the opt-in, 2026-10-16) | `platform_profile`, firmware power limits (`ppt_*`), Fn+Q events, mute LEDs | **replaced** by `legionctl_wmi` (below) |
| `legionctl_wmi` | DKMS, this repository (`kernel/legionctl-wmi`) | loaded at each start (the user's opt-in `/etc/modprobe.d/legionctl-wmi.conf`, 2026-10-16) | the same ABI as the five drivers above, the fan RPM, fan table, fan max mode and full speed | required: `doctor` reports it not loaded, a missing opt-in and a Legion WMI device it does not have |
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
  runs the check after kernel or DKMS updates, so a kernel upgrade that breaks `legionctl_wmi` or `nvidia` is
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
back to the old chain. The start through the entry makes it the first entry by itself when it reaches
`graphical.target` (kernel, initramfs, root file system and the login screen came up): the package links
`legionctl-boot-confirm.service` (`legionctl-system --boot-confirm`, after `graphical.target`) into
`graphical.target.wants`, so it runs at each start and does nothing without the boot takeover, when the entry leads
already, or when this start came through another entry. It writes this start's boot id to
`/run/legionctl/boot-confirmed`, and the operator's session module shows one desktop notice per start ("legionctl's
boot entry worked and is now the default"; the user's event `machine.boot-default` keeps it to one). The user approved
this with `boot install`, so polkit is not asked; there is no `boot confirm` (todo-180 G). When the last start came
through another entry (the firmware went back to rEFInd), `doctor` says so and what to do; `boot rebuild` (also
the pacman hook `95-legionctl-uki.hook`, after mkinitcpio's) makes the UKIs again, each kernel on its own (a kernel
whose line fails its check keeps its old image, and the hook writes a warning to its output and to the journal, tag
`legionctl-system`; `doctor` shows it until the line is correct), and removes those of kernels that are gone; when the
default kernel is gone, `loader.conf` starts the running kernel, else the first kernel with an image; `boot remove` takes the entry, the folder, the UKIs and our `loader.conf` away. The daemon (D-Bus `Boot`,
polkit `org.legionctl.boot`, no kept authorization) carries a plan out step by step and stops at the first failure,
so no entry follows an unsigned loader. Each boot and kernel plan (and its dry run) first checks each tool that it
runs (ukify: systemd-ukify, sbsign and sbverify: sbsigntools, efibootmgr) and refuses with the packages to install
before any step (todo-149; the package has them as optional dependencies, as other feature tools). Each install step can
run again: an install that failed on the way is completed by the next one, which makes the firmware entry only when
it is not there. Windows stays reachable through `windows reboot` (BootNext into its entry) and
the firmware menu: systemd-boot cannot start the boot manager on the other disk's ESP. e2e (real daemon, a fake ESP,
a stand-in efibootmgr with its own state, stand-in ukify/sbsign): the dry run changes nothing; install signs the loader
and each kernel's image with the kernel line, adds the entry not first and sets BootNext (mutation-checked); the
unit's command after a start through the entry makes it first and the desktop gets one notice, after a start through
another entry it changes nothing and doctor says so, and a default entry is left alone (mutation-checked); rebuild drops a removed kernel's image; remove leaves
the old chain as it was. Live: the first install and the test start are the user's.

## Kernels (built 2026-10-14 and 2026-10-15, todo-84 parts 1 to 4; decided by the user)

Two kernels are installed: `linux-cachyos` (EEVDF scheduler, thin LTO) and `linux-cachyos-bore-lto` (BORE, thin LTO).
rEFInd starts `linux-cachyos-bore-lto`. legionctl starts a kernel only through the boot takeover's systemd-boot
entries. There is no rEFInd path and no kexec.

| Command | What it does |
| --- | --- |
| `legionctl kernel list` | Each installed kernel (`/usr/lib/modules/<release>/` with `vmlinuz`): the package (`pkgbase`), the release, the scheduler (`bore` when `CONFIG_SCHED_BORE=y`, else `eevdf`) and `lto` (`CONFIG_LTO_CLANG_*`), from `build/.config` of the headers (the name is not enough: `linux-cachyos` is LTO too), legionctl's image, the command line, the DKMS state of `nvidia` and `legionctl-wmi` for that release (`dkms status`), and the marks `running` and `default`. |
| `legionctl kernel default <name>` | legionctl's `loader.conf` starts `<name>` from the next start on. |
| `legionctl kernel try <name>` | The next start only: the EFI variable `LoaderEntryOneShot` (systemd-boot's one-time entry) and `BootNext` to legionctl's firmware entry, then the restart. The default stays. |
| `legionctl kernel cmdline set <name> "<line>"` | The command line of `<name>` only, after one start through a trial image (see below). |
| `legionctl kernel cmdline undo <name>` | The line of `<name>` before its last change, the same way. A second undo goes forward again. |
| `legionctl kernel bench <suite> [--all [--seconds <s>] [--dry-run]] [--cancel]` | The newest run of a bench suite on each kernel, side by side; `--all` starts a series, `--cancel` stops it (see below). |
| `legionctl kernel build <base> [--config-fragment <file>] [--name <suffix>] [--dry-run]` | A kernel package `linux-cachyos-<suffix>` built from the installed `<base>`, then installed (see below). |

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

`kernel cmdline set`, `boot install` and `boot rebuild` check the line before they write anything:

- The line must have `root=`.
- Each `<module>.<parameter>` must name a module that this kernel has: built in (`modules.builtin`,
  `modules.builtin.modinfo`) or loadable (`modules.dep`, with the DKMS modules). The kernel ignores a parameter of an
  unknown module without a word, so `zwsap.enabled=0` is refused. Names that the kernel gives to user space (`rd.`,
  `systemd.`, `udev.`, `luks.`, `plymouth.`, `vconsole.`, `locale.`) are not modules.
- Some built-in modules are in none of these files (`rcupdate`, `rcutree`, `pcie_aspm`, `module`). A module that the
  running kernel has in `/sys/module` is accepted. For the running kernel, this is the full rule. For another
  kernel, the line is accepted with a warning: that kernel can be built in a different way, and only its files can
  tell, which do not list the module.
- The parameter names are not checked: `modules.builtin.modinfo` does not list each parameter. On 7.2.9 it lacks 15
  of 256 parameters of built-in modules that `/sys/module` shows, `zswap.enabled` of this machine's line among them.
  Parameters without a dot are not checked either: the kernel gives the ones it does not know to `init`.

Then the daemon makes a trial image of that kernel only, `legionctl-<name>.trial.efi`: `ukify` writes it with `.new`
at the end, `sbverify --cert` with the MOK certificate checks its signature, and `mv` gives it its name. The new line
goes to `<name>.cmdline.trial`, and the next start only goes to the trial image (`LoaderEntryOneShot` and
`BootNext`, as `kernel try`, but without the restart). The kernel's image and line stay. A bad line thus costs one
start, and the start after it is the old image again.

After a good start through the trial image, give the same command again. The daemon then sees that this start came
through the trial image (`LoaderEntrySelected`) and that the one-time entry is used (systemd-boot removes
`LoaderEntryOneShot` at that start). Only then `mv` puts the trial image in place of the kernel's image, the old line
goes to `<name>.cmdline.old` and the new line to `<name>.cmdline`. Before that start, the same command makes the
trial image again. A failed step leaves the old image and the old line. `boot rebuild` uses the same checks and the
same image steps for each kernel, and removes a trial image (its line stays in `<name>.cmdline.trial` until the next
`kernel cmdline set`).

### Boot entries for saved sets (todo-85)

A saved set with `--boot` (note 22) is one profile of each kernel's image (a multi-profile UKI): the kernel's line
and `legionctl.set=<name>`. The build makes one profile file for each such set with `ukify build --profile
--cmdline`, then the image with `--join-profile` for each, signed and checked as before, and removes the profile
files. Profile 0 is the kernel's own line, the default entry. `timeout 0` keeps the menu hidden until a key is held.

### The `kernel` condition of the configsets

A configset layer can apply to one kernel: `{ "when": { "kernel": "linux-cachyos" }, "set": { ... } }`, or
`legionctl rules edit 50-plain --when kernel=linux-cachyos sysctl.vm.swappiness=70`. The value is the package of the
running kernel (01-core.md, Configsets). A layer for a kernel that is not installed is not used, and `doctor` names
it.

### Bench and tune per kernel (part 3)

Each bench run records the kernel that it ran on: the package and the release (`bench list`, `bench compare` show
it in the title of each run). A run made before this change has no kernel.

`kernel bench <suite>` shows the newest run of the suite (`cpu`, `gpu` or `mixed`) on each kernel, one column per
kernel, with the change of each against the first kernel. While a series runs, it also shows how far it is.

`kernel bench <suite> --all` starts a series:

1. The operator makes the plan first. It refuses, with nothing changed, when the boot takeover is not installed,
   when a kernel in `/boot` has no legionctl image, and when a series runs already. `--dry-run` shows the plan.
2. The series is a job (`kernel.bench` in the jobs table). Each installed kernel, in the order of the names, starts
   once through `kernel try`. That is a restart, and polkit asks each time.
3. The operator starts at login (`graphical-session.target`) and resumes the job. It compares the running kernel
   with the kernel that it asked for. When they are not the same, the start failed: the job stops with the reason
   ("linux-cachyos did not start (linux-cachyos-bore-lto runs)"), and the next start is the default kernel.
4. When they are the same, the operator waits for an idle machine (at most 10 minutes), runs the suite
   (`--seconds`, default 30), and starts the next kernel.
5. After the last kernel, the default kernel starts once more, and the job ends with the list of runs.

A step resets the job's count of restarts, so the limit of 3 cut-offs applies to one step only, not to the series.
`kernel bench <suite> --cancel` ends the job. The next start is the default kernel (a one-time start does not
change the default), and the operator does nothing at that login.

`tune <attribute> <scenario> <values>... --per-kernel` puts the recommendation into a generated layer for the running
kernel only: `/var/lib/legionctl/configsets/50-tuned-<package>.jsonc` with `"when": {"power": "ac", "kernel":
"<package>"}`, name `tuned-<package>` (06-tuning.md). It is off until `legionctl rules enable tuned-<package>`. As
each power layer, it is used only when a layer also gives the value for battery.

### Kernel builds (part 4)

`kernel build <base> [--config-fragment <file>] [--name <suffix>]` builds a CachyOS kernel package from an installed
one. The default name is `legionctl`. The plan refuses, with nothing changed:

- a name that is not 1-32 of `a-z`, `0-9` and `-`, and a name whose package (`linux-cachyos-<suffix>`) is an
  installed kernel;
- a base that is not an installed `linux-cachyos` kernel;
- a fragment line that is not a kernel option (`CONFIG_<option>=<value>`, or a comment such as
  `# CONFIG_<option> is not set`).

The build is a job (`kernel.build`). Each step is a `kernel.build.progress` event (`legionctl events`). The work
folder is `~/.cache/legionctl/kernel-build/linux-cachyos-<suffix>/`.

1. Clone the PKGBUILD repository `https://github.com/CachyOS/linux-cachyos` (once). The base's folder is its name
   without `-lto` (`linux-cachyos-bore` for `linux-cachyos-bore-lto`).
2. Pin: check out the newest commit whose `<folder>/.SRCINFO` has the installed version (`pkgver-pkgrel`, from the
   release: `7.2.9-2`). The repository has no tags for each version.
3. Name: the PKGBUILD gets `_pkgsuffix=cachyos-<suffix>` before its line `pkgbase="linux-$_pkgsuffix"`. The
   package is `linux-cachyos-<suffix>`, and the release is `<version>-cachyos-<suffix>`. A PKGBUILD without that
   line stops the build.
4. Merge: with a fragment, the PKGBUILD's own `prepare()` runs first, then `scripts/kconfig/merge_config.sh -m
   .config legionctl.config` and `make olddefconfig` complete the configuration.
5. Build: `makepkg -s -C` in a container (image `legionctl-kernel-build` from `archlinux:base-devel`, a user with
   your uid), with the base's LTO mode (`_use_llvm_lto`: thin, full or none, from the base's `build/.config`). The
   log is `build.log` in the work folder.
6. Install: the daemon (D-Bus `Kernel("install", <package>, <folder>)`, polkit `org.legionctl.boot`) takes only a
   package `linux-cachyos-<name>` and refuses an installed name again. As root, it copies the kernel and its headers
   into its own folder `/var/lib/legionctl/kernel-install` (0700), so the caller cannot change a file after the
   check. There, each copy's `.PKGINFO` (`bsdtar`) must name the kernel or its headers (`pkgname`) and the kernel
   (`pkgbase`), with one kernel package and one version. Then it runs `pacman -U` with the copies and removes them.
   pacman's hooks make the initramfs (mkinitcpio), legionctl's signed image `legionctl-linux-cachyos-<suffix>.efi`
   with the running command line (`95-legionctl-uki.hook`), and the DKMS modules.

A restart of the operator cuts off the build. The operator resumes the job: it uses the clone, pins and names again,
and skips makepkg only when the package of the expected version is there and `legionctl.built` (written after makepkg
passed) records this version and this fragment. Else it removes each package in the folder before makepkg, so a
package of an earlier build (another version or fragment) never goes to pacman. The new kernel is then in `kernel
list`, and `kernel try` starts it.

### Tests (e2e, `crates/legionctl/tests/machine.rs`)

On the recorded machine (its `pkgbase` files and the scheduler and LTO lines of each `build/.config` are recorded),
with the real daemon, a stand-in ESP, a stand-in `efibootmgr` with its own state, and stand-in `ukify`, `sbsign`,
`sbverify`, `dkms` and `systemctl`:

- `kernels_are_listed_and_start_through_the_boot_takeover_once_or_by_default`: the list (both kernels, flavour,
  DKMS, running, default); `default` and `try` refused with nothing changed before `boot install`; `default` changes
  `loader.conf` only; `try` sets `LoaderEntryOneShot` and `BootNext` and restarts, and the default stays.
- `a_kernels_command_line_goes_into_its_image_only_and_undo_puts_the_old_one_back`: the line goes into a trial image
  and the next start only; the same command before a start through it changes nothing of the kernel, after it the
  trial image becomes that kernel's image only; a line without `root=`, a parameter of an unknown module and an
  unsigned image are refused with the old image and line left; `boot rebuild` keeps the line; undo puts the old line
  back the same way.
- `a_built_in_module_that_the_running_kernel_has_is_accepted_on_each_kernels_line`: `rcupdate.` (in `/sys/module`,
  not in the module files) is accepted on the running kernel's line, with a warning on the other kernel's line; a
  module that `/sys/module` does not have either is refused.
- `the_pacman_hook_makes_each_kernel_image_on_its_own_and_reports_the_one_it_cannot_make`: `legionctl-system
  --boot-rebuild` with one kernel's line bad: the default kernel's image is made again, the other keeps its old image,
  and the warning is in the hook's output, in the journal (stand-in `logger`) and in `doctor`.
- `legionctl_boot_comes_beside_the_old_chain_and_is_tried_once_before_it_leads` (also): when the default kernel is
  removed, rebuild makes the running kernel the default in `loader.conf`.
- `kernel_install_takes_only_a_built_kernel_and_installs_roots_checked_copies`: through the daemon's `Kernel`
  method: `glibc`, a file named as the kernel whose `.PKGINFO` names `glibc`, two kernel packages and two versions
  are refused with nothing given to pacman; then pacman gets root's copies only, and the copies go.
- `a_kernel_layer_applies_only_on_its_kernel`: a `kernel` layer applies only while its kernel runs; a layer for a
  kernel that is not installed is a `doctor` finding.
- `a_bench_series_starts_each_kernel_once_and_goes_on_after_each_start`: the plan (one start for each kernel, then
  the default); the operator, started again after each start, benches the kernel that it asked for (the stand-in
  workload scores by the running release), records the kernel, starts the next one and ends after the default; the
  view and `bench compare` show the change between the kernels.
- `a_bench_series_stops_at_a_wrong_start_and_when_cancelled`: a start of the wrong kernel stops the series with no
  bench and no other start; a cancelled series does nothing at the next start; the view shows the newest run of
  each kernel.
- `a_tuned_layer_for_one_kernel_applies_only_on_that_kernel`: `tune --per-kernel` writes `tuned-<package>` with the
  `kernel` condition; enabled, it sets the limit on its kernel only.
- `a_kernel_build_is_refused_or_planned_before_anything_runs`: an installed name, a bad fragment, a bad name and an
  unknown base are refused, and the dry run shows the plan; nothing runs.
- `a_kernel_is_built_from_its_pinned_pkgbuild_and_installed_through_the_daemon`: a build cut off by an operator
  restart goes on as the job with the same clone, and removes the packages of an earlier build (an older version, the
  same version with another fragment) that are in the folder; the PKGBUILD of 7.2.9-2 (not the newer 7.2.10-1), the name, the
  fragment merged and completed, the base's LTO, `pacman -U` of the kernel and its headers through the daemon
  (polkit); after pacman's hook, the new kernel's signed image, and its name refused as installed. Stand-ins: git (a
  local repository), docker (it runs the PKGBUILD's `prepare()` and writes the packages), `merge_config.sh`, make
  and pacman.

Live: the boot takeover is not installed on this machine yet. The first `boot install`, `kernel try`,
`kernel default`, `kernel bench --all` and `kernel build` are the user's. No real kernel was built: the network
clone, the pin on the real repository, the container build (about an hour) and the real pacman hooks are not
verified.

## legionctl-wmi: our Legion WMI module (todo-79 part 1, decided by the user 2026-10-14)

`kernel/legionctl-wmi/` is a DKMS kernel module (GPL-2.0, kernel coding style, `checkpatch --strict` clean). It owns
the whole Legion WMI of this laptop (82WQ, BIOS KWCN54WW). It replaces `lenovo_wmi_gamezone`, `lenovo_wmi_other`,
`lenovo_wmi_capdata`, `lenovo_wmi_events` and `lenovo_wmi_hotkey_utilities`, and the fan calls that went through
`acpi_call` before the switch part (below). The daemon uses its sysfs only.

### Why our own module

- The upstream drivers give no fan table, no fan max mode and no full speed. Before the switch these went through
  `acpi_call`, which lets root call any ACPI method. The module makes only the calls below, checks each value in the kernel and
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
- The package installs the source to `/usr/src/legionctl-wmi-<version>/` (DKMS builds it for each installed kernel),
  `/usr/lib/modprobe.d/legionctl-wmi.conf` with `blacklist legionctl_wmi` (`kernel/legionctl-wmi/no-autoload.conf`),
  and the opt-in blacklist of the five upstream drivers to `/usr/share/legionctl/legionctl-wmi.conf` only (review on
  merge, 2026-10-14): a package install must not make an untested module replace the upstream drivers at the next
  start. A blacklist stops only the load by alias (udev); `modprobe legionctl_wmi` still loads the module. In step 6
  below, after the user's first live load, the opt-in goes to `/etc/modprobe.d/legionctl-wmi.conf`: a file in `/etc`
  with the same name replaces the one in `/usr/lib` (modprobe.d(5)), so udev then loads `legionctl_wmi` and not the
  upstream drivers. Until then the profile keeps requiring the upstream drivers.
- Incident (2026-10-16, todo-143): the first package had no `/usr/lib` file. The module has the modalias of each
  Legion WMI GUID, so at the first start udev loaded it beside the upstream drivers. `legionctl_wmi` bound the fan and
  other-method GUIDs, `lenovo_wmi_gamezone` bound the GameZone GUID, `lenovo_wmi_other` got nothing: no
  `platform_profile`, and the daemon could not set the power mode. `legionctl doctor` now reports a mixed bind (the
  bound devices under `/sys/bus/wmi/drivers` of both driver families) and a missing `platform_profile` while a Legion
  WMI driver is loaded.
- Secure Boot is on: DKMS must sign the module with the enrolled key, as for `nvidia`.
- Test (`crates/legionctl/tests/e2e.rs`, `the_legionctl_wmi_module_gives_the_legion_wmi_through_sysfs`, also in the
  CI VM step): a Debian VM with the generic 7.x kernel builds the module with DKMS. A stand-in of the firmware
  (`tests/fixtures/legionctl-wmi/ssdt.asl`: the same GUIDs, object ids and buffer layouts) is an ACPI table that
  QEMU gives (`-acpitable`). The guest installs the module and the `/usr/lib` file, checks that `modprobe -b` of a
  Legion WMI alias does not plan `legionctl_wmi` and that `modprobe legionctl_wmi` does, installs the opt-in in
  `/etc`, then restarts: udev loads the drivers as on the real machine. The stand-in prints each call that it gets as
  an AML Debug line, and a test helper (`aml-call`) calls its Fn+Q method as the EC does. The guest script (`run.sh`)
  makes 48 checks: the no-autoload file, the opt-in blacklist, the profile and its Fn+Q notice, the attribute bounds
  and modes, the fan table byte for byte as the firmware gets it, the refusals, the table again after a mode change,
  after Fn+Q and after resume (`pm_test`), full speed, fan max mode, the LEDs, and a clean unload.

### First live load (the user's, todo-17 gate)

Steps 1-3 and 6 are done (2026-10-16, EVIDENCE/validation/29; the opt-in is installed). Step 4 is open: the first
live writes are the user's, and no agent or test writes the real firmware. The first load on this machine is the
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
6. Only when steps 3 and 4 are right: `sudo install -m644 /usr/share/legionctl/legionctl-wmi.conf /etc/modprobe.d/`.
   This file has the same name as the package's `/usr/lib/modprobe.d/legionctl-wmi.conf` and replaces it: the
   blacklist moves from `legionctl_wmi` to the upstream drivers. Restart, and
   `lsmod | grep -E 'legionctl_wmi|lenovo_wmi'` shows `legionctl_wmi` only; `legionctl doctor` reports no mixed bind.
   The switch part (below) moved the daemon to the module. To go back for good: remove the file in `/etc` and restart
   (the `/usr/lib` file then stops the autoload of `legionctl_wmi` again); the fan commands then fail with
   `legionctl_wmi is not loaded: see docs/design/08-kernel.md`.

### The switch part (todo-79 part 2, 2026-10-16)

- The daemon and the operator read and write the firmware through the module's sysfs only. The hwmon device is found
  by its name `legionctl_wmi`, never by a fixed `hwmonN`. The platform profile and the firmware attributes keep their
  paths (the module gives the upstream ABI).
- Fan RPM (status, monitor, sensors, `/v1/metrics`, bench), the fan table, fan max mode and full speed come from the
  hwmon files; the fan writes go through the same files (02-platform, Fan control, says the order and the rules).
- `acpi_call` is gone: no code path, no package dependency, not in the profile's required modules. Without the module
  each fan command fails with `legionctl_wmi is not loaded: see docs/design/08-kernel.md`; there is no other path.
- `doctor`: `legionctl_wmi` not loaded (the required module check), the opt-in file missing, a Legion WMI device of
  the six that `legionctl_wmi` does not have, a mixed bind, a missing `platform_profile`.
- More readings: full speed in `fan status`, `sensors` and the `FanModeChanged(max, full_speed)` signal and
  `fan-mode` journal entry; fan max mode as the module reports it. Not yet: the firmware temperatures (todo-93: the
  sensor ids need the read-only WMI probe).
- Tests: the recorded tree of this machine (`tests/fixtures/sysfs-legionctl-wmi-82wq-2026-10-16`, read-only: hwmon9,
  `lenovo-wmi-other-0`, `platform-profile-0`, the WMI driver binds, `/proc/modules`, the opt-in) is the firmware of
  the e2e tests (`crates/legionctl/tests/machine.rs`).
- The `acpi_call` package is still installed on this machine (`/usr/lib/modules-load.d/acpi_call.conf` loads it).
  legionctl does not use it; the user can remove it (`sudo pacman -Rns acpi_call-dkms`).
