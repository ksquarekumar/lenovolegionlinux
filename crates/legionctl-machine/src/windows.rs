//! Dual boot (note 05): the Windows boot entry, a pending one-time boot, NTFS volumes Windows left dirty, what the
//! Windows registry says about the hardware clock and Fast Startup, and the plan that boots Windows once (`BootNext`,
//! then reboot). The firmware's boot entries come from `efibootmgr`; the registry comes from the SYSTEM hive on a
//! mounted NTFS volume (decision D3: the user's read-only ntfs3 mount, no raw reader, no root).

use std::path::PathBuf;
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

/// NTFS volumes the kernel reported as dirty since boot.
#[must_use]
pub fn dirty_ntfs() -> Vec<String> {
    let Ok(out) = Command::new("journalctl")
        .args(["-k", "-b", "-q", "--no-pager", "-g", "ntfs3.*dirty"])
        .output()
    else {
        return vec![];
    };
    let mut volumes: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| Some(l.split_once("ntfs3(")?.1.split_once(')')?.0.to_string()))
        .collect();
    volumes.sort();
    volumes.dedup();
    volumes
}

/// The Windows volume: an NTFS volume of the mount table or of `/etc/fstab` (an automount mounts it on access) that
/// holds the SYSTEM hive; its source (`/dev/...` or `UUID=...`) and mount point. None when no volume holds it.
///
/// # Errors
/// The mount table or `/etc/fstab` is there but cannot be read.
pub fn volume(root: &SysRoot) -> Result<Option<(String, PathBuf)>> {
    for table in ["/proc/self/mounts", "/etc/fstab"] {
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

fn system_hive(root: &SysRoot) -> Result<Option<PathBuf>> {
    Ok(volume(root)?.map(|(_, target)| root.path(target).join(SYSTEM_HIVE)))
}

/// `RealTimeIsUniversal` and `HiberbootEnabled` of the control set Windows starts with.
fn registry(hive: &Hive) -> Result<(bool, bool)> {
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
/// `efibootmgr` is missing or fails (no EFI variables).
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
    let dirty_ntfs = dirty_ntfs();
    let mut hazards: Vec<String> = dirty_ntfs
        .iter()
        .map(|v| format!("{v} is dirty: turn off Fast Startup in Windows (powercfg /h off), then shut Windows down fully"))
        .collect();
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
        match Hive::open(path).and_then(|h| registry(&h)) {
            Ok((rtc, fast)) => {
                if !rtc {
                    hazards.push("Windows keeps the hardware clock in local time, so the clock jumps at each boot: in Windows, run reg add HKLM\\SYSTEM\\CurrentControlSet\\Control\\TimeZoneInformation /v RealTimeIsUniversal /t REG_DWORD /d 1 /f".into());
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
///
/// # Errors
/// No entry starts Windows.
pub fn reboot_plan(state: &WindowsState) -> Result<Vec<Step>> {
    let id = state
        .boot_entry
        .clone()
        .context("no firmware boot entry starts Windows Boot Manager")?;
    Ok(vec![
        Step::Unit(vec!["efibootmgr".into(), "--bootnext".into(), id]),
        Step::Run(vec!["systemctl".into(), "reboot".into()]),
    ])
}
