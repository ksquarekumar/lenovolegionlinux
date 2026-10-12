# Issues and cleanups

Found while working on this machine on 2026-10-12. "Repo" items belong to this repository; "host" items are
system state outside it, recorded here so they are not lost.

## Open issues

| # | Issue | Evidence | Scope | Proposed fix |
| --- | --- | --- | --- | --- |
| 1 | Choppy animations on the external monitor | Mutter: "GPU /dev/dri/card1 selected primary from builtin panel presence"; DP-3 (3440x1440@144) is on NVIDIA card0 | repo | Prefer NVIDIA as Mutter primary when the external display is in use (02-platform, Phase 0.3) |
| 2 | Two power-limit surfaces disagree | RAPL PL1 70 W written by legionctl; firmware `ppt_pl1_spl` 90 W; firmware mode changes rewrite MMIO RAPL | repo | Firmware attributes only; legionctl stops writing RAPL (15-layout, one path). Firmware `ppt_*` are writable only in `custom` mode (EBUSY in max-power, measured); decided: the firmware modes' built-in limits, no limit writes (custom mode runs the fans at ~6,000 rpm until a fan table is written). Measured: a `ppt_pl1_spl` write sets MMIO PL1 at once (30 W → 30,000,000 µW), and the Python reassert timer rewrites it to 85 W within 30 s; the port removes that writer — **Fixed by the switch (2026-10-13): no tool writes RAPL** |
| 3 | No re-apply after suspend/resume | no sleep hook; old `legionctl-system-onresume` disabled by cleanup | repo | system-sleep hook now, logind sensor in legionctl-system — **Fixed: system-sleep hook installed** — **Fixed by the switch: legionctl-system reacts to logind resume** |
| 4 | `reassert` does not restore power limits | code path skips `apply_rapl` | repo | Phase 0.2 — **Obsolete: power limits are the firmware's own; nothing to restore** |
| 5 | Fan RPM bogus | `thinkpad_acpi` hwmon reports `speed: 4`, `status: disabled` | repo | Fixed: firmware WMI via `acpi_call` (`\_SB_.GZFD.WMAE`, feature 0x04030001/2); validated idle vs gpu-burn — **Fixed** |
| 6 | Frequent package throttling | `package_throttle_count` 33,251 since boot at moderate temperatures | repo | MSR throttle-reason sensor; check whether firmware PROCHOT or power limits fire |
| 7 | Dual-boot clock skew | timesyncd "Clock change detected" at first sync; journal timestamps out of order | host (Windows) | `RealTimeIsUniversal=1` in Windows; Linux-side detection in `doctor` |
| 8 | Windows data partition will not mount | `ntfs3: volume is dirty`; `mnt-windows-data.mount` start-limit-hit | host (Windows) | Disable Fast Startup, run chkdsk in Windows |
| 9 | `doctor` warns "ADP0 is offline; AC performance policy should not be active" whenever on battery | unconditional check in `doctor()` | repo | Only warn if AC policy is actually applied on battery — **Fixed: the Rust doctor has no such check** |
| 10 | Throughput results of the 2026-10-12 sweeps are contaminated | load average 6–35 during steps; manual apply mid-step | repo | Re-run clean; next sweep tunes `ppt_*` |
| 11 | Fn+R firmware sends ~10 events per press | libinput capture | repo | Debounced (1.5 s) in legionctl-osd and the toggle; move to legionctl-system — **Fixed: one tap is a 2–6 event burst; legionctl-system starts a new tap after 0.4 s of quiet** |
| 12 | Mutter persistent display changes show a confirmation dialog | user report after test toggles | repo | Toggle uses TEMPORARY method (done) — **Fixed** |
| 13 | `make install` leaves untracked files | stale `/usr/lib/sysctl.d/90-legion-tuning.conf` had to be removed by hand | repo | Install via PKGBUILD/pacman — **Fixed: no leftovers** — **Fixed: installed as the Arch package** |
| 14 | MongoDB service fails on kernel ≥ 6.19 | journal | host | Disable the unit; run MongoDB in a container — **Fixed: unit disabled** |
| 15 | 8 GNOME extensions have no GNOME 51 build; 8 enabled extensions are not installed | extension metadata, enabled list | host | Wait for upstream builds; prune the enabled list |
| 16 | `gh` CLI token invalid | `gh auth status` | host | `gh auth login -p ssh -w` — **Fixed: logged in** |
| 17 | Pending sudo-side host fixes | faillock/sudoers, libvirt modular daemons, toolchain packages, `paccache` | host | Commands listed in the session; confirm which ran |
| 18 | 197 package updates pending | `checkupdates` | host | `pacman -Syu` when convenient — **Open: 14 updates pending (2026-10-12 evening)** |

| 19 | `thinkpad_acpi` force-loaded on a Legion; logs "secondary fan control detected & enabled" | `/etc/modules-load.d/acpi.conf`, dmesg | host | Drop it from autoload (08-kernel) — **Fixed: acpi.conf now loads only acpi_call, ideapad_laptop** |
| 20 | Broadcom `wl` loaded on an Intel Wi-Fi machine; DKMS rebuilt every kernel update | `lsmod`, `dkms status`, wlan0 → iwlwifi | host | `pacman -R broadcom-wl-dkms` — **Fixed: package removed** |
| 21 | Module autoload files reference missing modules (`vfio_mdev`, `virtio_serial`, `vmmon`) | systemd-modules-load "Failed to find module" each boot | host | Prune `vfio.conf`, `virtio.conf`, `vmware.conf`, `openlogi.conf`, `kvm.conf` (GVT-g) — **Fixed: vfio.conf/virtio.conf pruned, vmware.conf gone** |
| 22 | `i915 enable_gvt=1` set; GVT-g unsupported on 13th gen | `/etc/modprobe.d/intel.conf` | host | Remove the option — **Fixed: option removed** |
| 23 | Stale module trees of removed kernels | `/usr/lib/modules/{6.8.9,6.9.3,6.10.5}*`, unowned | host | Delete after confirming no package owns them — **Fixed: only current kernels remain** |
| 24 | `blacklist ecdh_generic` (old Bluetooth workaround) | `/etc/modprobe.d/blacklist.conf` | host | Verify Bluetooth, then remove — **Open** |

| 25 | Boot loader stage 3 min 33 s | rEFInd `enable_mouse`: any mouse movement cancels the 5 s countdown | host | `timeout 2`, disable `enable_mouse` — **Check: loader stage still 10.1 s in the last boot** |
| 26 | 300 MB initramfs per kernel | `MODULES=(i915 intel_agp nvidia)` + `kms` hook pull NVIDIA modules and GSP firmware into early boot | host | Drop `nvidia` and the `kms` hook; NVIDIA loads from modules-load.d before GDM — **Fixed: 27 MB per kernel** |
| 27 | 9.2 s waiting for Wi-Fi at boot | `systemd-networkd-wait-online@wlan0` enabled though networkd manages no links (NetworkManager does) | host | Disable systemd-networkd and its wait-online units — **Fixed: networkd masked/disabled** |
| 28 | Samba server running, not needed | `smb`/`nmb` pull `network-online.target` into boot | host | Disable smb, nmb — **Fixed: disabled** |
| 29 | Maintenance jobs hammer the disk right after login | `fstrim` 16 min, `plocate-updatedb` 7 min, `man-db` catch up at boot | host | Run them at idle CPU/IO priority |
| 30 | VMware leftovers (installed with VMware's own installer) | `/etc/vmware`, `/usr/lib/vmware-installer`, `/var/log/vmware*`, `~/.vmware`, autoload `vmware.conf` | host | Remove — **Fixed: removed** |
| 31 | `/etc/default/grub` is unused | the machine boots rEFInd via shim; kernel options live in `/boot/refind_linux.conf` | host | Treat `refind_linux.conf` as the source of kernel options in tooling and docs |

| 32 | Phantom I/O pressure (60–90 % "some" and "full") with idle disks | the WinBoat container (`ghcr.io/dockur/windows`, QEMU/KVM Windows VM): 77–91 % in its cgroup, all other cgroups below 1 %, its disk counters do not change (2026-10-13) | host, VM | Load classifier gates on disk activity, and `legionctl load` names the cgroup (`I/O wait without disk I/O from docker/09bd9c9baa5f`). The earlier cause (one RCU kworker) was wrong: those kworkers are short D-state sleeps. Open: which QEMU thread waits in iowait; needs root (`kernel.task_delayacct=1`, then field 42 of `/proc/<qemu>/task/*/stat`) |
| 33 | Two `docker` CLIs | `/usr/local/bin/docker` (unowned) shadows `/usr/bin/docker`, both 29.9.0 | host | Remove `/usr/local/bin/docker` |
| 34 | Docker uses the cgroupfs driver on a systemd, cgroup v2 host | `/etc/docker/daemon.json` `native.cgroupdriver=cgroupfs` | host | Switch to the systemd driver (recreates containers) |
| 35 | WinBoat holds 16 GiB of 31 GiB RAM | `docker stats` | host | Pause or stop when not in use; memory alert (17-virtualization) |
| 36 | Docker disk use | 32 GB reclaimable images, 50 GB build cache on `/data` | host | `docker image prune`, `docker builder prune` after review |
| 37 | Machine wakes from suspend early | S3 on 2026-10-12 woke after ~7 s, before a 40 s RTC alarm; no wake IRQ recorded; USB wake enabled on ITE 8258 keyboard, Logitech receiver, USB LAN | host | Find the source (`/sys/kernel/debug/wakeup_sources`); disable wake on the receiver and LAN if it is them |
| 38 | `spd5118` resume errors | `spd5118_resume returns -6` for both DIMM sensors at every resume; sensors read correctly afterwards | host | Kernel driver noise; report upstream if it persists |
| 39 | Plugging or unplugging never applied the power policy | `legion-apply-ac/battery.service` disabled, no `.wants` links: the udev rule started empty targets; measured on battery in `max-power` with PPD `performance` (2026-10-12) | repo | Fixed by the switch: legionctl-system reacts to power-source events itself — **Fixed by the switch** |
| 40 | One i915 FIFO underrun when the built-in panel turned on | `CPU pipe A FIFO underrun` at panel enable; no glitch after that | host | Closed unless it repeats |
| 41 | On USB-C power the Python reassert timer forced the AC policy on battery | the USB-C port (`ucsi-source-psy-USBC000:001`, the monitor) reports `online=1`, so `ConditionACPower` is true while `ADP0` is offline; the battery ran in `max-power` (2026-10-12) | repo | Fixed by the switch (no timer; legionctl-system reads `ADP0`). Decided 2026-10-13: USB-C power uses the battery policy (USB-C PD gives less than the barrel adapter); `status` shows the source as `usb-c` |

## Cleanups in this repo

- `tuning/` holds one file (`scx_loader.toml`); fold into configsets when the config layers exist (14-sessions).
- `packaging/arch/PKGBUILD` keeps `# shellcheck disable=SC2034,SC2154`: makepkg reads the variables and provides
  `$pkgdir`, which shellcheck cannot see. Kept for review (rule: no suppressed lints).
