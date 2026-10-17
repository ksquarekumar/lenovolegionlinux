//! One read of this machine's state: identity, power source and policy, CPU, power limits, fans, GPU wiring,
//! units, modules and load. Fan RPM and throttle reasons need root; they are `None` otherwise.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::gpu::{self, Output};
use crate::ledger::{Key, Ledger};
use crate::load::{Load, Thresholds, read_load};
use crate::profile::Profile;
use crate::sysroot::SysRoot;
use crate::{firmware, kernel, system};

const DMI: &str = "/sys/class/dmi/id";
const FIRMWARE_ATTRIBUTES: &str = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes";
const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
const POWERCAP: &str = "/sys/class/powercap";
const UNITS: [&str; 4] = [
    "legionctl-system.service",
    "scx_loader.service",
    "nvidia-powerd.service",
    "nvidia-persistenced.service",
];
const MODULES: [&str; 5] = [
    "ideapad_laptop",
    "legionctl_wmi",
    "thinkpad_acpi",
    "legion_laptop",
    "lenovo_legion",
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DmiMismatch {
    pub key: String,
    pub expected: String,
    pub actual: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RaplConstraint {
    pub name: String,
    pub limit_w: Option<f64>,
    pub window_us: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RaplZone {
    pub zone: String,
    pub constraints: Vec<RaplConstraint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UnitState {
    pub unit: String,
    pub active: String,
    pub enabled: String,
}

/// How much to read: `Fast` reads only sysfs and procfs, for views that refresh every second; `Full` also asks
/// systemd and nvidia-smi (which wakes the NVIDIA GPU) and calls the firmware for fan RPM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Fast,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Snapshot {
    pub profile_id: String,
    /// Empty when the machine matches the profile's DMI identity.
    pub dmi_mismatches: Vec<DmiMismatch>,
    /// `bios_version`, `bios_release`, `ec_firmware_release`, `board_name`.
    pub firmware_versions: BTreeMap<String, Option<String>>,
    /// "ac" (the barrel adapter), "usb-c" (USB-C power only; the battery policy applies), "battery" or "unknown".
    pub power_source: String,
    /// Firmware thermal mode: low-power, balanced, performance, max-power, custom.
    pub platform_profile: Option<String>,
    pub platform_profile_choices: Vec<String>,
    /// Set while legionctl follows a mode chosen with Fn+Q.
    pub fn_q_override: Option<String>,
    /// `no_turbo`, `max_perf_pct` (`intel_pstate`) and `ignore_ppc` (processor).
    pub cpu_settings: BTreeMap<String, Option<String>>,
    /// CPU frequency governors and how many policies use each.
    pub governors: BTreeMap<String, usize>,
    pub energy_performance_preference: BTreeMap<String, usize>,
    /// Lenovo firmware limits (W / °C), e.g. `ppt_pl1_spl`, `ppt_pl2_sppt`, `cpu_temp`.
    pub firmware_limits: BTreeMap<String, String>,
    /// Package RAPL zones (MSR and MMIO) with their limits; read for watts, never written.
    pub rapl: Vec<RaplZone>,
    pub nvidia_power_limit_w: Option<String>,
    /// Fan RPM from the `legionctl-wmi` module's hwmon device; `None` without the module.
    pub fan_rpm: Option<BTreeMap<String, u64>>,
    /// Throttle reasons logged since boot, as `legionctl-system` reads them (the MSR is root's).
    pub throttle_reasons: Option<Vec<String>>,
    /// Why `throttle_reasons` is unknown.
    pub throttle_unknown: Option<String>,
    pub package_temp_c: Option<f64>,
    pub outputs: Vec<Output>,
    pub gpu_mode: gpu::GpuMode,
    /// GPU GNOME draws the desktop on.
    pub desktop_gpu: gpu::DesktopGpu,
    pub units: Vec<UnitState>,
    /// Modules that matter on this machine, and whether each is loaded.
    pub modules: BTreeMap<String, bool>,
    pub load: Load,
}

/// The DMI keys of `profile` that this machine does not match.
///
/// # Errors
/// A DMI file cannot be read.
pub fn dmi_mismatches(root: &SysRoot, profile: &Profile) -> Result<Vec<DmiMismatch>> {
    let mut out = Vec::new();
    for (key, expected) in &profile.dmi_match {
        let actual = root.read(Path::new(DMI).join(key))?;
        if actual.as_deref() != Some(expected.as_str()) {
            out.push(DmiMismatch {
                key: key.clone(),
                expected: expected.clone(),
                actual,
            });
        }
    }
    Ok(out)
}

/// How many CPU policies have each value of the cpufreq `file`.
///
/// # Errors
/// A policy's file cannot be read.
pub fn cpufreq_counts(root: &SysRoot, file: &str) -> Result<BTreeMap<String, usize>> {
    let mut counts = BTreeMap::new();
    for policy in root.children(CPUFREQ, "policy")? {
        if let Some(value) = root.read(policy.join(file))? {
            *counts.entry(value).or_default() += 1;
        }
    }
    Ok(counts)
}

fn rapl(root: &SysRoot) -> Result<Vec<RaplZone>> {
    let mut zones = Vec::new();
    for z in root.children(POWERCAP, "intel-rapl")? {
        let Some(zone) = z.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if zone.matches(':').count() != 1
            || root.read(z.join("name"))?.as_deref() != Some("package-0")
        {
            continue;
        }
        let mut constraints = Vec::new();
        for i in 0.. {
            let Some(name) = root.read(z.join(format!("constraint_{i}_name")))? else {
                break;
            };
            constraints.push(RaplConstraint {
                name,
                limit_w: root
                    .read(z.join(format!("constraint_{i}_power_limit_uw")))?
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|uw| uw / 1e6),
                window_us: root
                    .read(z.join(format!("constraint_{i}_time_window_us")))?
                    .and_then(|v| v.parse().ok()),
            });
        }
        zones.push(RaplZone { zone, constraints });
    }
    Ok(zones)
}

/// USB-C power delivery is online. It gives less than the barrel adapter, so the battery policy applies (issue 41).
fn usb_c_online(root: &SysRoot) -> Result<bool> {
    for p in root.children("/sys/class/power_supply", "ucsi-source-psy-")? {
        if root.read(p.join("online"))?.as_deref() == Some("1") {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Snapshot {
    /// The machine now, with the throttle reasons that `legionctl-system` read (`Err`: why they are unknown).
    ///
    /// # Errors
    /// A machine file or the ledger cannot be read.
    pub fn read(
        root: &SysRoot,
        profile: &Profile,
        detail: Detail,
        thresholds: &Thresholds,
        throttle: std::result::Result<Vec<String>, String>,
    ) -> Result<Snapshot> {
        let full = detail == Detail::Full;
        let outputs = gpu::outputs(root)?;
        let loaded = kernel::loaded_modules(root)?;
        let mut firmware_limits = BTreeMap::new();
        for a in root.children(FIRMWARE_ATTRIBUTES, "")? {
            if let Some(name) = a.file_name().map(|n| n.to_string_lossy().into_owned())
                && let Some(value) = root.read(a.join("current_value"))?
            {
                firmware_limits.insert(name, value);
            }
        }
        let mut firmware_versions = BTreeMap::new();
        for k in [
            "bios_version",
            "bios_release",
            "ec_firmware_release",
            "board_name",
        ] {
            firmware_versions.insert(k.to_string(), root.read(Path::new(DMI).join(k))?);
        }
        let mut cpu_settings = BTreeMap::new();
        for (k, p) in [
            ("no_turbo", "/sys/devices/system/cpu/intel_pstate/no_turbo"),
            (
                "max_perf_pct",
                "/sys/devices/system/cpu/intel_pstate/max_perf_pct",
            ),
            ("ignore_ppc", "/sys/module/processor/parameters/ignore_ppc"),
        ] {
            cpu_settings.insert(k.to_string(), root.read(p)?);
        }
        let power_source = match root.read(&profile.paths.ac_adapter)?.as_deref() {
            Some("1") => "ac".into(),
            Some("0") if usb_c_online(root)? => "usb-c".into(),
            Some("0") => "battery".into(),
            _ => "unknown".into(),
        };
        // unknown without the legionctl-wmi module; doctor names the missing module
        let fan_rpm = if full && firmware::hwmon(root, firmware::LEGIONCTL_WMI)?.is_some() {
            Some(firmware::fan_rpm(root)?)
        } else {
            None
        };
        Ok(Snapshot {
            profile_id: profile.id.clone(),
            dmi_mismatches: dmi_mismatches(root, profile)?,
            firmware_versions,
            power_source,
            platform_profile: root.read(&profile.paths.platform_profile)?,
            platform_profile_choices: root
                .read(&profile.paths.platform_profile_choices)?
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect(),
            fn_q_override: Ledger::read(root)?.get(Key::Override)?,
            cpu_settings,
            governors: cpufreq_counts(root, "scaling_governor")?,
            energy_performance_preference: cpufreq_counts(root, "energy_performance_preference")?,
            firmware_limits,
            rapl: rapl(root)?,
            nvidia_power_limit_w: full.then(system::nvidia_power_limit).flatten(),
            fan_rpm,
            throttle_unknown: throttle.as_ref().err().cloned(),
            throttle_reasons: throttle.ok(),
            package_temp_c: crate::telemetry::cpu_temp(root)?,
            gpu_mode: gpu::mode(&outputs),
            outputs,
            desktop_gpu: gpu::desktop_preference(root)?,
            units: UNITS
                .iter()
                .filter(|_| full)
                .map(|u| UnitState {
                    unit: u.to_string(),
                    // a state that cannot be read is that unit's state, with the cause
                    active: system::unit_state("is-active", u)
                        .unwrap_or_else(|e| format!("unknown: {e:#}")),
                    enabled: system::unit_state("is-enabled", u)
                        .unwrap_or_else(|e| format!("unknown: {e:#}")),
                })
                .collect(),
            modules: MODULES
                .iter()
                .map(|m| (m.to_string(), loaded.contains(*m)))
                .collect(),
            load: read_load(root, thresholds)?,
        })
    }
}
