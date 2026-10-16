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

/// What to install when fwupd is not there.
pub const NOT_INSTALLED: &str = "fwupd is not installed: install fwupd (pacman -S fwupd)";

/// `fwupdmgr <args> --json`: `None` when fwupd is not installed, `Null` when it has nothing to show (exit status 2).
fn fwupdmgr(args: &[&str]) -> Result<Option<Value>> {
    let out = match Command::new("fwupdmgr").args(args).arg("--json").output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("running fwupdmgr"),
    };
    match out.status.code() {
        Some(0) => Ok(Some(
            serde_json::from_slice(&out.stdout).context("fwupdmgr gave no JSON")?,
        )),
        Some(2) => Ok(Some(Value::Null)),
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

/// The devices fwupd knows, with the upgrade it can apply to each; `None` when fwupd is not installed.
///
/// # Errors
/// fwupd fails.
pub fn devices() -> Result<Option<Vec<Device>>> {
    let Some(all) = fwupdmgr(&["get-devices"])? else {
        return Ok(None);
    };
    Ok(Some(
        all["Devices"]
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
            .collect(),
    ))
}

/// The device `wanted` (its id or its name) and the command that updates it.
///
/// # Errors
/// fwupd is not installed, no such device, more than one device has the name (two drives of one model), or fwupd has
/// no upgrade for it.
pub fn update_plan(wanted: &str) -> Result<(Device, Vec<String>)> {
    let devices = devices()?.context(NOT_INSTALLED)?;
    let found: Vec<&Device> = devices
        .iter()
        .filter(|d| d.id == wanted || d.name == wanted)
        .collect();
    let device = match found.as_slice() {
        [device] => (*device).clone(),
        [] => bail!("fwupd has no device {wanted} (see legionctl firmware status)"),
        many => bail!(
            "{} devices are named {wanted}: give the device id ({})",
            many.len(),
            many.iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
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
