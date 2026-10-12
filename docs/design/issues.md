# Issues and cleanups

Found while working on this machine on 2026-10-12. "Repo" items belong to this repository; "host" items are
system state outside it, recorded here so they are not lost.

## Open issues

| # | Issue | Evidence | Scope | Proposed fix |
| --- | --- | --- | --- | --- |
| 1 | Choppy animations on the external monitor | Mutter: "GPU /dev/dri/card1 selected primary from builtin panel presence"; DP-3 (3440x1440@144) is on NVIDIA card0 | repo | Prefer NVIDIA as Mutter primary when the external display is in use (02-platform, Phase 0.3) |
| 2 | Two power-limit surfaces disagree | RAPL PL1 70 W written by legionctl; firmware `ppt_pl1_spl` 90 W; firmware mode changes rewrite MMIO RAPL | repo | Firmware attributes primary, RAPL fallback (Phase 0.4) |
| 3 | No re-apply after suspend/resume | no sleep hook; old `legiond-onresume` disabled by cleanup | repo | system-sleep hook now, logind sensor in legiond |
| 4 | `reassert` does not restore power limits | code path skips `apply_rapl` | repo | Phase 0.2 |
| 5 | Fan RPM bogus | `thinkpad_acpi` hwmon reports `speed: 4`, `status: disabled` | repo | Fixed: firmware WMI via `acpi_call` (`\_SB_.GZFD.WMAE`, feature 0x04030001/2); validated idle vs gpu-burn |
| 6 | Frequent package throttling | `package_throttle_count` 33,251 since boot at moderate temperatures | repo | MSR throttle-reason sensor; check whether firmware PROCHOT or power limits fire |
| 7 | Dual-boot clock skew | timesyncd "Clock change detected" at first sync; journal timestamps out of order | host (Windows) | `RealTimeIsUniversal=1` in Windows; Linux-side detection in `doctor` |
| 8 | Windows data partition will not mount | `ntfs3: volume is dirty`; `mnt-windows-data.mount` start-limit-hit | host (Windows) | Disable Fast Startup, run chkdsk in Windows |
| 9 | `doctor` warns "ADP0 is offline; AC performance policy should not be active" whenever on battery | unconditional check in `doctor()` | repo | Only warn if AC policy is actually applied on battery |
| 10 | Throughput results of the 2026-10-12 sweeps are contaminated | load average 6–35 during steps; manual apply mid-step | repo | Re-run clean; next sweep tunes `ppt_*` |
| 11 | Fn+R firmware sends ~10 events per press | libinput capture | repo | Debounced (1.5 s) in legion-osd and the toggle; move to legiond |
| 12 | Mutter persistent display changes show a confirmation dialog | user report after test toggles | repo | Toggle uses TEMPORARY method (done) |
| 13 | `make install` leaves untracked files | stale `/usr/lib/sysctl.d/90-legion-tuning.conf` had to be removed by hand | repo | Install via PKGBUILD/pacman |
| 14 | MongoDB service fails on kernel ≥ 6.19 | journal | host | Disable the unit; run MongoDB in a container |
| 15 | 8 GNOME extensions have no GNOME 51 build; 8 enabled extensions are not installed | extension metadata, enabled list | host | Wait for upstream builds; prune the enabled list |
| 16 | `gh` CLI token invalid | `gh auth status` | host | `gh auth login -p ssh -w` |
| 17 | Pending sudo-side host fixes | faillock/sudoers, libvirt modular daemons, toolchain packages, `paccache` | host | Commands listed in the session; confirm which ran |
| 18 | 197 package updates pending | `checkupdates` | host | `pacman -Syu` when convenient |

| 19 | `thinkpad_acpi` force-loaded on a Legion; logs "secondary fan control detected & enabled" | `/etc/modules-load.d/acpi.conf`, dmesg | host | Drop it from autoload (08-kernel) |
| 20 | Broadcom `wl` loaded on an Intel Wi-Fi machine; DKMS rebuilt every kernel update | `lsmod`, `dkms status`, wlan0 → iwlwifi | host | `pacman -R broadcom-wl-dkms` |
| 21 | Module autoload files reference missing modules (`vfio_mdev`, `virtio_serial`, `vmmon`) | systemd-modules-load "Failed to find module" each boot | host | Prune `vfio.conf`, `virtio.conf`, `vmware.conf`, `openlogi.conf`, `kvm.conf` (GVT-g) |
| 22 | `i915 enable_gvt=1` set; GVT-g unsupported on 13th gen | `/etc/modprobe.d/intel.conf` | host | Remove the option |
| 23 | Stale module trees of removed kernels | `/usr/lib/modules/{6.8.9,6.9.3,6.10.5}*`, unowned | host | Delete after confirming no package owns them |
| 24 | `blacklist ecdh_generic` (old Bluetooth workaround) | `/etc/modprobe.d/blacklist.conf` | host | Verify Bluetooth, then remove |

| 25 | Boot loader stage 3 min 33 s | rEFInd `enable_mouse`: any mouse movement cancels the 5 s countdown | host | `timeout 2`, disable `enable_mouse` |
| 26 | 300 MB initramfs per kernel | `MODULES=(i915 intel_agp nvidia)` + `kms` hook pull NVIDIA modules and GSP firmware into early boot | host | Drop `nvidia` and the `kms` hook; NVIDIA loads from modules-load.d before GDM |
| 27 | 9.2 s waiting for Wi-Fi at boot | `systemd-networkd-wait-online@wlan0` enabled though networkd manages no links (NetworkManager does) | host | Disable systemd-networkd and its wait-online units |
| 28 | Samba server running, not needed | `smb`/`nmb` pull `network-online.target` into boot | host | Disable smb, nmb |
| 29 | Maintenance jobs hammer the disk right after login | `fstrim` 16 min, `plocate-updatedb` 7 min, `man-db` catch up at boot | host | Run them at idle CPU/IO priority |
| 30 | VMware leftovers (installed with VMware's own installer) | `/etc/vmware`, `/usr/lib/vmware-installer`, `/var/log/vmware*`, `~/.vmware`, autoload `vmware.conf` | host | Remove |
| 31 | `/etc/default/grub` is unused | the machine boots rEFInd via shim; kernel options live in `/boot/refind_linux.conf` | host | Treat `refind_linux.conf` as the source of kernel options in tooling and docs |

## Cleanups in this repo

- Remove or wire up the unused `ac.cpu_power` profile block (duplicates `rapl`).
- Rename `legion-reassert-ac.{service,timer}`: it now also handles PPD and governor, and goes away with legiond.
- `legion-osd` starts `legion-refresh-toggle` via `PATH`; use the absolute install path.
- README "Commands" section predates `follow`, `validate`, Fn keys, OSD and the Makefile tooling; refresh it.
- `docs/DKMS_PLAN.md` and `docs/UPSTREAMS.md` predate the firmware-attribute findings; reconcile.
- PKGBUILD: `url=file:///...`, static `pkgver`; derive from git (`pkgver()`), point `url` at the GitHub repo.
- `scripts/install-apply-check.sh` overlaps `make check`; fold in or delete.
- Tests load extension-less scripts through `SourceFileLoader`; fine until the Rust port, then replaced.
- `tuning/` holds one file (`scx_loader.toml`); fold into configsets in Phase 1.
- Commit the staged work (35+ files) before starting Phase 0 so the redesign starts from a clean tree.
