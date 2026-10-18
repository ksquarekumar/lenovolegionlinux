//! Dual boot (note 05): the Windows boot entry, a pending one-time boot, NTFS volumes Windows left dirty, what the
//! Windows registry says about the hardware clock and Fast Startup, and the plan that boots Windows once (`BootNext`,
//! then reboot). The firmware's boot entries come from `efibootmgr`; the registry comes from the SYSTEM hive on a
//! mounted NTFS volume (decision D3: the user's read-only ntfs3 mount, no raw reader, no root).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hive::Hive;
use crate::policy::Step;
use crate::sysroot::SysRoot;

/// Windows' own boot loader, whatever the entry's label says.
const BOOTMGR: &str = "\\EFI\\Microsoft\\Boot\\bootmgfw.efi";
/// The SYSTEM hive, from the root of the Windows volume.
const SYSTEM_HIVE: &str = "Windows/System32/config/SYSTEM";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WindowsState {
    /// The firmware entry that starts Windows (`0004`), if there is one.
    pub boot_entry: Option<String>,
    pub boot_entry_label: Option<String>,
    /// A one-time boot the next start uses, if one is set.
    pub boot_next: Option<String>,
    /// NTFS volumes the kernel reported dirty since boot: Windows was not shut down fully (Fast Startup or
    /// hibernation), so Linux must not write them.
    pub dirty_ntfs: Vec<String>,
    /// The SYSTEM hive the registry values come from; None when no mounted NTFS volume holds one.
    pub system_hive: Option<String>,
    /// Windows keeps the hardware clock in UTC (`RealTimeIsUniversal` is 1), as Linux does.
    pub rtc_utc: Option<bool>,
    /// Fast Startup is on (`HiberbootEnabled` is 1): Windows hibernates at shut down and leaves its volumes dirty.
    pub fast_startup: Option<bool>,
    /// What needs attention before booting Windows or mounting its volumes.
    pub hazards: Vec<String>,
}

/// `efibootmgr` output: the Windows entry (id, label) and `BootNext`.
#[must_use]
pub fn parse_efibootmgr(text: &str) -> (Option<(String, String)>, Option<String>) {
    let entry = text.lines().find_map(|line| {
        let rest = line.strip_prefix("Boot")?;
        let (id, rest) = rest.split_at_checked(4)?;
        if !id.bytes().all(|b| b.is_ascii_hexdigit()) || !rest.starts_with('*') {
            return None;
        }
        let (label, path) = rest[1..].split_once('\t')?;
        path.contains(BOOTMGR)
            .then(|| (id.to_string(), label.trim().to_string()))
    });
    let next = text
        .lines()
        .find_map(|l| l.strip_prefix("BootNext:"))
        .map(|v| v.trim().to_string());
    (entry, next)
}

/// What a dirty NTFS volume means and what to do, in one text for doctor and `windows status`.
#[must_use]
pub fn dirty_hazard(volume: &str) -> String {
    format!(
        "NTFS volume {volume} is marked dirty: Windows did not shut down fully. Start Windows and shut it down with \
         Fast Startup off"
    )
}

/// NTFS volumes the kernel reported as dirty since boot.
///
/// # Errors
/// The kernel log cannot be read (`journalctl` is missing or fails).
pub fn dirty_ntfs() -> Result<Vec<String>> {
    let out = Command::new("journalctl")
        .args(["-k", "-b", "-q", "--no-pager", "-o", "cat"])
        .output()
        .context("reading the kernel log")?;
    anyhow::ensure!(
        out.status.success(),
        "the kernel log cannot be read: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let mut volumes: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (volume, rest) = l.split_once("ntfs3(")?.1.split_once(')')?;
            rest.contains("dirty").then(|| volume.to_string())
        })
        .collect();
    volumes.sort();
    volumes.dedup();
    Ok(volumes)
}

/// Whether the NTFS volume on the block device `dev` is marked dirty: bit 0x0001 of the flags of its `$Volume`
/// (`$VOLUME_INFORMATION` of MFT record 3), as Windows leaves them until it shuts down fully or checks the volume.
///
/// # Errors
/// The device cannot be read, or it holds no NTFS volume that this reader knows.
pub fn ntfs_dirty(dev: &Path) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let bad = || {
        anyhow::anyhow!(
            "{} holds no NTFS volume that legionctl can read",
            dev.display()
        )
    };
    let le = |b: &[u8], at: usize, n: usize| -> Result<u64> {
        let bytes = b.get(at..at + n).ok_or_else(bad)?;
        Ok(bytes.iter().rev().fold(0, |v, x| (v << 8) | u64::from(*x)))
    };
    let mut file =
        std::fs::File::open(dev).with_context(|| format!("opening {}", dev.display()))?;
    let mut boot = [0u8; 512];
    file.read_exact(&mut boot)
        .with_context(|| format!("reading {}", dev.display()))?;
    anyhow::ensure!(&boot[3..11] == b"NTFS    ", bad());
    let sector = le(&boot, 0x0B, 2)?;
    // the values come from the disk: no arithmetic on them may overflow
    let per_cluster = match boot[0x0D] {
        0 => return Err(bad()),
        n @ 1..=0x80 => u64::from(n),
        // a large cluster: 2^(256 - n) sectors
        n => 1u64.checked_shl(256 - u32::from(n)).ok_or_else(bad)?,
    };
    let cluster = sector.checked_mul(per_cluster).ok_or_else(bad)?;
    let record_size = match i8::from_ne_bytes([boot[0x40]]) {
        n if n > 0 => u64::from(n.unsigned_abs())
            .checked_mul(cluster)
            .ok_or_else(bad)?,
        n => 1u64
            .checked_shl(u32::from(n.unsigned_abs()))
            .ok_or_else(bad)?,
    };
    anyhow::ensure!((1024..=65536).contains(&record_size), bad());
    let mut record = vec![0u8; usize::try_from(record_size)?];
    let start = le(&boot, 0x30, 8)?
        .checked_mul(cluster)
        .and_then(|mft| mft.checked_add(3 * record_size))
        .ok_or_else(bad)?;
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut record)
        .with_context(|| format!("reading {}", dev.display()))?;
    anyhow::ensure!(&record[..4] == b"FILE", bad());
    // the update sequence: the last two bytes of each 512 bytes hold its number, the array holds what was there
    let (usa, count) = (
        usize::try_from(le(&record, 4, 2)?)?,
        usize::try_from(le(&record, 6, 2)?)?,
    );
    let number = [
        record.get(usa).copied().ok_or_else(bad)?,
        record.get(usa + 1).copied().ok_or_else(bad)?,
    ];
    for i in 1..count {
        let end = i * 512 - 2;
        anyhow::ensure!(record.get(end..end + 2) == Some(&number[..]), bad());
        let kept = record
            .get(usa + 2 * i..usa + 2 * i + 2)
            .ok_or_else(bad)?
            .to_vec();
        record[end..end + 2].copy_from_slice(&kept);
    }
    let mut at = usize::try_from(le(&record, 0x14, 2)?)?;
    loop {
        let kind = le(&record, at, 4)?;
        anyhow::ensure!(kind != 0xFFFF_FFFF, bad());
        let length = usize::try_from(le(&record, at + 4, 4)?)?;
        anyhow::ensure!(length > 0, bad());
        if kind == 0x70 {
            // resident: the value starts at its offset; the flags are at 0x0A of the value
            let value = at + usize::try_from(le(&record, at + 0x14, 2)?)?;
            anyhow::ensure!(record.get(at + 8) == Some(&0), bad());
            return Ok(le(&record, value + 0x0A, 2)? & 1 == 1);
        }
        at += length;
    }
}

/// The Windows volume: an NTFS volume of the mount table or of `/etc/fstab` (an automount mounts it on access) that
/// holds the SYSTEM hive; its source (`/dev/...` or `UUID=...`) and mount point. None when no volume holds it.
///
/// # Errors
/// The mount table or `/etc/fstab` is there but cannot be read.
pub fn volume(root: &SysRoot) -> Result<Option<(String, PathBuf)>> {
    for table in [crate::mounts::HOST_MOUNTS, "/etc/fstab"] {
        let Some(text) = root.read(table)? else {
            continue;
        };
        let found = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| {
                let mut fields = l.split_whitespace();
                let source = fields.next()?.to_string();
                let target = fields.next()?;
                matches!(fields.next()?, "ntfs3" | "ntfs" | "ntfs-3g" | "fuseblk")
                    .then(|| (source, PathBuf::from(target.replace("\\040", " "))))
            })
            .find(|(_, target)| root.path(target).join(SYSTEM_HIVE).is_file());
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

/// The SYSTEM hive on the Windows volume, when there is one.
///
/// # Errors
/// See [`volume`].
pub fn system_hive(root: &SysRoot) -> Result<Option<PathBuf>> {
    Ok(volume(root)?.map(|(_, target)| root.path(target).join(SYSTEM_HIVE)))
}

/// `RealTimeIsUniversal` and `HiberbootEnabled` of the control set Windows starts with, from the SYSTEM hive `path`.
///
/// # Errors
/// The hive cannot be read or parsed.
pub fn registry(path: &Path) -> Result<(bool, bool)> {
    let hive = Hive::open(path)?;
    let set = hive.current_control_set()?;
    let rtc = hive.dword(
        &format!("{set}\\Control\\TimeZoneInformation"),
        "RealTimeIsUniversal",
    )?;
    let hiberboot = hive.dword(
        &format!("{set}\\Control\\Session Manager\\Power"),
        "HiberbootEnabled",
    )?;
    // Windows' defaults when a value is missing: local time, Fast Startup on
    Ok((rtc == Some(1), hiberboot.unwrap_or(1) == 1))
}

/// The current state.
///
/// # Errors
/// `efibootmgr` is missing or fails (no EFI variables), or the kernel log cannot be read.
pub fn state(root: &SysRoot) -> Result<WindowsState> {
    let out = Command::new("efibootmgr")
        .output()
        .context("running efibootmgr")?;
    anyhow::ensure!(
        out.status.success(),
        "efibootmgr failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let (entry, boot_next) = parse_efibootmgr(&String::from_utf8_lossy(&out.stdout));
    let dirty_ntfs = dirty_ntfs()?;
    let mut hazards: Vec<String> = dirty_ntfs.iter().map(|v| dirty_hazard(v)).collect();
    if entry.is_none() {
        hazards.push("no firmware boot entry starts Windows Boot Manager".into());
    }
    if let (Some(next), Some((id, _))) = (&boot_next, &entry)
        && next == id
    {
        hazards.push(format!(
            "the next start boots Windows (BootNext {next}) already"
        ));
    }
    let hive = system_hive(root)?;
    let (mut rtc_utc, mut fast_startup) = (None, None);
    if let Some(path) = &hive {
        match registry(path) {
            Ok((rtc, fast)) => {
                if !rtc {
                    hazards.push("Windows keeps the hardware clock in local time, so the clock jumps at each boot: in Windows, set the registry value RealTimeIsUniversal (TimeZoneInformation) to 1".into());
                }
                if fast {
                    hazards.push("Fast Startup is on, so Windows leaves its volumes dirty: in Windows, run powercfg /h off".into());
                }
                (rtc_utc, fast_startup) = (Some(rtc), Some(fast));
            }
            Err(e) => hazards.push(format!("the Windows registry cannot be read: {e:#}")),
        }
    }
    Ok(WindowsState {
        boot_entry: entry.as_ref().map(|(id, _)| id.clone()),
        boot_entry_label: entry.map(|(_, label)| label),
        boot_next,
        dirty_ntfs,
        system_hive: hive.map(|p| p.display().to_string()),
        rtc_utc,
        fast_startup,
        hazards,
    })
}

/// Boot Windows once: set `BootNext` to its entry, then reboot. The start after that returns to the default order.
/// The reboot is the last step; a first step says which waiting start this one replaces.
///
/// # Errors
/// No entry starts Windows.
pub fn reboot_plan(state: &WindowsState, boot: &crate::boot::BootState) -> Result<Vec<Step>> {
    let id = state
        .boot_entry
        .clone()
        .context("no firmware boot entry starts Windows Boot Manager")?;
    Ok(crate::boot::replaced_by(boot, &id)
        .into_iter()
        .chain([
            Step::Unit(vec!["efibootmgr".into(), "--bootnext".into(), id]),
            Step::Run(vec!["systemctl".into(), "reboot".into()]),
        ])
        .collect())
}
