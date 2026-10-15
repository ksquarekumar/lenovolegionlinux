//! Firmware updates through fwupd (LVFS): `fwupdmgr get-devices --json` reads the devices and the releases that fwupd
//! can apply, as the user. An update runs `fwupdmgr update`, and fwupd asks polkit itself.

use std::process::Command;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Device {
    /// fwupd's device id (`fwupdmgr update <id>`).
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    /// fwupd can write its firmware.
    pub updatable: bool,
    /// The newest release that is an upgrade, if fwupd's metadata has one.
    pub update: Option<String>,
    pub update_summary: Option<String>,
}

/// `fwupdmgr <args> --json`; `None` when fwupd has nothing to show (exit status 2).
fn fwupdmgr(args: &[&str]) -> Result<Option<Value>> {
    let out = match Command::new("fwupdmgr").args(args).arg("--json").output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("fwupdmgr is not installed: install fwupd (pacman -S fwupd)")
        }
        Err(e) => return Err(e).context("running fwupdmgr"),
    };
    match out.status.code() {
        Some(0) => Ok(Some(
            serde_json::from_slice(&out.stdout).context("fwupdmgr gave no JSON")?,
        )),
        Some(2) => Ok(None),
        _ => bail!(
            "fwupdmgr {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().map(String::from)
}

fn has_flag(v: &Value, flag: &str) -> bool {
    v["Flags"]
        .as_array()
        .is_some_and(|f| f.iter().any(|x| x == flag))
}

/// The devices fwupd knows, with the upgrade it can apply to each.
///
/// # Errors
/// fwupd is not installed or fails.
pub fn devices() -> Result<Vec<Device>> {
    let Some(all) = fwupdmgr(&["get-devices"])? else {
        return Ok(Vec::new());
    };
    Ok(all["Devices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let upgrade = d["Releases"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|r| has_flag(r, "is-upgrade"));
            Some(Device {
                id: text(d, "DeviceId")?,
                name: text(d, "Name").unwrap_or_default(),
                version: text(d, "Version"),
                updatable: has_flag(d, "updatable"),
                update: upgrade.and_then(|r| text(r, "Version")),
                update_summary: upgrade.and_then(|r| text(r, "Summary")),
            })
        })
        .collect())
}

/// The device `wanted` (its id or its name) and the command that updates it.
///
/// # Errors
/// No such device, or fwupd has no upgrade for it.
pub fn update_plan(wanted: &str) -> Result<(Device, Vec<String>)> {
    let devices = devices()?;
    let Some(device) = devices
        .iter()
        .find(|d| d.id == wanted || d.name == wanted)
        .cloned()
    else {
        bail!("fwupd has no device {wanted} (see legionctl firmware status)");
    };
    if device.update.is_none() {
        bail!(
            "{} is at {}: fwupd has no upgrade for it (fwupdmgr refresh gets the newest metadata)",
            device.name,
            device.version.as_deref().unwrap_or("an unknown version")
        );
    }
    // --no-reboot-check: fwupd does not restart the machine; a restart that finishes the update stays the user's
    let argv = [
        "fwupdmgr",
        "update",
        &device.id,
        "--assume-yes",
        "--no-reboot-check",
    ]
    .map(String::from)
    .to_vec();
    Ok((device, argv))
}
