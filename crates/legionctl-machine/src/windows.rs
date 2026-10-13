//! Dual boot (note 05): the Windows boot entry, a pending one-time boot, NTFS volumes Windows left dirty, and the
//! plan that boots Windows once (`BootNext`, then reboot). The firmware's boot entries come from `efibootmgr`.

use std::process::Command;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::policy::Step;

/// Windows' own boot loader, whatever the entry's label says.
const BOOTMGR: &str = "\\EFI\\Microsoft\\Boot\\bootmgfw.efi";

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

/// The current state.
///
/// # Errors
/// `efibootmgr` is missing or fails (no EFI variables).
pub fn state() -> Result<WindowsState> {
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
    Ok(WindowsState {
        boot_entry: entry.as_ref().map(|(id, _)| id.clone()),
        boot_entry_label: entry.map(|(_, label)| label),
        boot_next,
        dirty_ntfs,
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
        Step::Run(vec!["efibootmgr".into(), "--bootnext".into(), id]),
        Step::Run(vec!["systemctl".into(), "reboot".into()]),
    ])
}
