//! The battery (todo-82, Windows parity): its charge mode (`charge_types`: `Standard`, `Long_Life` = conservation near
//! 80 %, `Fast` = rapid charge; the kernel keeps `Fast` and `Long_Life` apart), USB charging while the laptop is off
//! (`ideapad_acpi`), and its health.

use anyhow::{Context, Result, bail};

use crate::policy::Step;
use crate::sysroot::SysRoot;

const BAT: &str = "/sys/class/power_supply/BAT0";
const IDEAPAD: &str = "/sys/bus/platform/drivers/ideapad_acpi";

/// The battery now.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Battery {
    /// standard, conservation or rapid.
    pub mode: Option<String>,
    /// Charge state (Charging, Discharging, Not charging, Full).
    pub status: Option<String>,
    pub capacity_pct: Option<u32>,
    pub cycle_count: Option<u32>,
    pub energy_full_wh: Option<f64>,
    pub energy_full_design_wh: Option<f64>,
    /// `energy_full` of `energy_full_design`, in percent.
    pub health_pct: Option<f64>,
    /// Watts into (charging) or out of the battery.
    pub power_w: Option<f64>,
    /// USB ports give power while the laptop is off.
    pub usb_charging: Option<bool>,
}

/// The mode names and their `charge_types` values.
const MODES: [(&str, &str); 3] = [
    ("standard", "Standard"),
    ("conservation", "Long_Life"),
    ("rapid", "Fast"),
];

fn usb_charging_path(root: &SysRoot) -> Result<Option<std::path::PathBuf>> {
    for d in root.children(IDEAPAD, "VPC")? {
        let p = d.join("usb_charging");
        if root.exists(&p)? {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// The battery's state, from sysfs.
///
/// # Errors
/// A battery file cannot be read.
pub fn state(root: &SysRoot) -> Result<Battery> {
    let read = |f: &str| root.read(format!("{BAT}/{f}"));
    let number =
        |f: &str| -> Result<Option<f64>> { Ok(read(f)?.and_then(|v| v.parse::<f64>().ok())) };
    // the bracketed value is the one in use: `Fast Standard [Long_Life]`
    let mode = read("charge_types")?.and_then(|types| {
        let active = types
            .split_whitespace()
            .find(|t| t.starts_with('['))?
            .trim_matches(['[', ']']);
        MODES
            .iter()
            .find(|(_, v)| *v == active)
            .map(|(n, _)| (*n).to_string())
    });
    let full = number("energy_full")?.map(|uwh| uwh / 1e6);
    let design = number("energy_full_design")?.map(|uwh| uwh / 1e6);
    let usb_charging = match usb_charging_path(root)? {
        Some(p) => root.read(p)?.map(|v| v == "1"),
        None => None,
    };
    Ok(Battery {
        mode,
        status: read("status")?,
        capacity_pct: read("capacity")?.and_then(|v| v.parse().ok()),
        cycle_count: read("cycle_count")?.and_then(|v| v.parse().ok()),
        health_pct: full
            .zip(design)
            .filter(|(_, d)| *d > 0.0)
            .map(|(f, d)| (f / d * 1000.0).round() / 10.0),
        energy_full_wh: full,
        energy_full_design_wh: design,
        power_w: number("power_now")?.map(|uw| uw / 1e6),
        usb_charging,
    })
}

/// Plan the charge mode `mode` (standard, conservation, rapid).
///
/// # Errors
/// An unknown mode, or the battery offers no such charge type.
pub fn mode_plan(root: &SysRoot, mode: &str) -> Result<Vec<Step>> {
    let Some((_, value)) = MODES.iter().find(|(n, _)| *n == mode) else {
        bail!("the battery mode must be standard, conservation or rapid, not {mode}");
    };
    let types = root
        .read(format!("{BAT}/charge_types"))?
        .context("this battery has no charge_types (the kernel's battery driver does not offer charge modes)")?;
    anyhow::ensure!(
        types
            .split_whitespace()
            .any(|t| t.trim_matches(['[', ']']) == *value),
        "this battery does not offer {value} (it offers: {types})"
    );
    Ok(vec![Step::Write {
        path: format!("{BAT}/charge_types"),
        value: (*value).to_string(),
    }])
}

/// Plan USB charging while the laptop is off.
///
/// # Errors
/// The laptop has no such setting (`ideapad_acpi`'s `usb_charging`).
pub fn usb_charging_plan(root: &SysRoot, on: bool) -> Result<Vec<Step>> {
    let path = usb_charging_path(root)?
        .context("this laptop has no usb_charging setting (ideapad_acpi)")?;
    Ok(vec![Step::Write {
        path: path.to_string_lossy().into_owned(),
        value: if on { "1" } else { "0" }.into(),
    }])
}
