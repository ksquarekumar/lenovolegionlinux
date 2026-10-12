# 11 — Disks: health, mounts and NTFS repair

## What is used today

- **Mounting NTFS**: the in-kernel `ntfs3` driver (read-write), configured in `/etc/fstab` as on-demand
  automounts (`noauto,x-systemd.automount,x-systemd.idle-timeout=60,nofail,uid=1000,gid=1000,umask=022,windows_names`).
  `/mnt/windows-system` is read-only; `/mnt/windows-data` is read-write.
- **Repairing NTFS**: nothing on Linux. `ntfs3` has no fsck, and `ntfs-3g` (which ships `ntfsfix`, `ntfsinfo`) is not
  installed. `ntfsfix` only clears the dirty flag and resets the journal; it is not a replacement for `chkdsk`.
  The only full repair is `chkdsk /f` in Windows, bare-metal or inside the installed-Windows VM (05-windows).
- **Health**: `smartctl` is installed (covers NVMe); `nvme-cli` is not. `fstrim.timer` and `udisks2` are active.
  `findmnt --verify` passes. NVMe temperatures 44 °C (nvme0) and 40 °C (nvme1) at idle.

## Disks feature

| Capability | How |
| --- | --- |
| Inventory | udisks2 D-Bus (drives, partitions, filesystems, mount state), `lsblk -J` fallback |
| Health | `smartctl -j -a` per NVMe: percentage used, available spare, media errors, unsafe shutdowns, temperature; trend stored over time |
| TRIM | `fstrim.timer` state and last run; warn if disabled |
| fstab manager | parse, validate (`findmnt --verify --verbose`), show each entry with its systemd mount/automount state and last failure; edits are previewed as a diff, backed up (`/etc/fstab.legion-<timestamp>`), validated, then applied with `systemctl daemon-reload` |
| NTFS entries | generate entries by UUID with the options above; `ro` for the Windows system volume; never `force` |
| Mount failures | read the unit's journal; map `ntfs3: volume is dirty` / hibernation / `start-limit-hit` to a cause and a fix; `systemctl reset-failed` after the fix |
| ext4 checks | `tune2fs -l`: error count, last check, mount count; schedule an fsck at next boot if errors are recorded |
| Swap / resume | `resume=UUID` matches the swap partition used for hibernation; zram size and priority |

## NTFS repair flow

1. **Explain**: dirty flag set (unclean shutdown) or hibernated (Fast Startup or Windows hibernation). Read the
   state without mounting: NTFS boot sector and `$Volume` flags (`ntfsinfo`), `hiberfil.sys` header.
2. **Safe access now**: offer a read-only mount (`ntfs3` mounts dirty volumes read-only).
3. **Quick fix (with warning)**: `ntfsfix -d` clears the dirty flag so `ntfs3` mounts read-write. Refused if the
   volume is hibernated, because writing to it can lose data Windows has not flushed.
4. **Full repair**: `chkdsk /f` in Windows. With the VM in place, legiond can run it in the installed Windows
   without a reboot.
5. **Recover the mount**: `systemctl reset-failed mnt-windows\x2ddata.mount`, then access the path to trigger the
   automount.

Never mount an NTFS volume read-write while Windows runs in the VM or is hibernated (05-windows guard rails).

## Packages

`ntfs-3g` for its tools only (mounting stays on `ntfs3`), `nvme-cli`; `smartmontools` and `udisks2` are present.
