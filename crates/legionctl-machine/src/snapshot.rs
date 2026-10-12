//! One read of this machine's state: identity, power source and policy, CPU, power limits, fans, GPU wiring,
//! units, modules and load. Fan RPM and throttle reasons need root; they are `None` otherwise.

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::gpu::{self, Output};
use crate::ledger::{Key, Ledger};
use crate::load::{Load, read_load};
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
const MODULES: [&str; 6] = [
    "ideapad_laptop",
    "lenovo_wmi_gamezone",
    "lenovo_wmi_events",
    "thinkpad_acpi",
    "legion_laptop",
    "lenovo_legion",
];

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DmiMismatch {
    pub key: String,
    pub expected: String,
    pub actual: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RaplConstraint {
    pub name: String,
    pub limit_w: Option<f64>,
    pub window_us: Option<u64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RaplZone {
    pub zone: String,
    pub constraints: Vec<RaplConstraint>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
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

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Snapshot {
    pub profile_id: String,
    /// Empty when the machine matches the profile's DMI identity.
    pub dmi_mismatches: Vec<DmiMismatch>,
    /// bios_version, bios_release, ec_firmware_release, board_name.
    pub firmware_versions: BTreeMap<String, Option<String>>,
    /// "ac", "battery" or "unknown".
    pub power_source: String,
    /// Firmware thermal mode: low-power, balanced, performance, max-power, custom.
    pub platform_profile: Option<String>,
    pub platform_profile_choices: Vec<String>,
    /// Set while legionctl follows a mode chosen with Fn+Q.
    pub fn_q_override: Option<String>,
    /// no_turbo, max_perf_pct (intel_pstate) and ignore_ppc (processor).
    pub cpu_settings: BTreeMap<String, Option<String>>,
    /// CPU frequency governors and how many policies use each.
    pub governors: BTreeMap<String, usize>,
    pub energy_performance_preference: BTreeMap<String, usize>,
    /// Lenovo firmware limits (W / °C), e.g. ppt_pl1_spl, ppt_pl2_sppt, cpu_temp.
    pub firmware_limits: BTreeMap<String, String>,
    /// Package RAPL zones (MSR and MMIO) with their limits; read for watts, never written.
    pub rapl: Vec<RaplZone>,
    pub nvidia_power_limit_w: Option<String>,
    /// Firmware fan RPM (needs root).
    pub fan_rpm: Option<BTreeMap<String, u64>>,
    /// Throttle reasons logged since boot (needs root).
    pub throttle_reasons: Option<Vec<String>>,
    pub package_temp_c: Option<f64>,
    pub outputs: Vec<Output>,
    /// "hybrid" (panel on the iGPU), "discrete" (panel on NVIDIA) or "unknown".
    pub gpu_mode: String,
    /// GPU GNOME draws the desktop on: nvidia, intel or auto.
    pub desktop_gpu: String,
    pub units: Vec<UnitState>,
    /// Modules that matter on this machine, and whether each is loaded.
    pub modules: BTreeMap<String, bool>,
    pub load: Load,
}

pub fn dmi_mismatches(root: &SysRoot, profile: &Profile) -> Vec<DmiMismatch> {
    profile
        .dmi_match
        .iter()
        .filter_map(|(key, expected)| {
            let actual = root.read(Path::new(DMI).join(key));
            (actual.as_deref() != Some(expected.as_str())).then(|| DmiMismatch {
                key: key.clone(),
                expected: expected.clone(),
                actual,
            })
        })
        .collect()
}

pub fn cpufreq_counts(root: &SysRoot, file: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for policy in root.children(CPUFREQ, "policy") {
        if let Some(value) = root.read(policy.join(file)) {
            *counts.entry(value).or_default() += 1;
        }
    }
    counts
}

fn rapl(root: &SysRoot) -> Vec<RaplZone> {
    root.children(POWERCAP, "intel-rapl")
        .into_iter()
        .filter(|z| {
            z.file_name()
                .is_some_and(|n| n.to_string_lossy().matches(':').count() == 1)
                && root.read(z.join("name")).as_deref() == Some("package-0")
        })
        .map(|z| RaplZone {
            zone: z
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            constraints: (0..)
                .map_while(|i| {
                    let name = root.read(z.join(format!("constraint_{i}_name")))?;
                    Some(RaplConstraint {
                        name,
                        limit_w: root
                            .read(z.join(format!("constraint_{i}_power_limit_uw")))
                            .and_then(|v| v.parse::<f64>().ok())
                            .map(|uw| uw / 1e6),
                        window_us: root
                            .read(z.join(format!("constraint_{i}_time_window_us")))
                            .and_then(|v| v.parse().ok()),
                    })
                })
                .collect(),
        })
        .collect()
}

fn package_temp(root: &SysRoot) -> Option<f64> {
    root.children("/sys/class/hwmon", "hwmon")
        .into_iter()
        .find(|h| root.read(h.join("name")).as_deref() == Some("coretemp"))
        .and_then(|h| root.read(h.join("temp1_input")))
        .and_then(|t| t.parse::<f64>().ok())
        .map(|millideg| millideg / 1000.0)
}

impl Snapshot {
    pub fn read(root: &SysRoot, profile: &Profile, detail: Detail) -> Snapshot {
        let full = detail == Detail::Full;
        let outputs = gpu::outputs(root);
        let loaded = kernel::loaded_modules(root);
        let firmware_limits = root
            .children(FIRMWARE_ATTRIBUTES, "")
            .into_iter()
            .filter_map(|a| {
                Some((
                    a.file_name()?.to_string_lossy().into_owned(),
                    root.read(a.join("current_value"))?,
                ))
            })
            .collect();
        Snapshot {
            profile_id: profile.id.clone(),
            dmi_mismatches: dmi_mismatches(root, profile),
            firmware_versions: [
                "bios_version",
                "bios_release",
                "ec_firmware_release",
                "board_name",
            ]
            .into_iter()
            .map(|k| (k.to_string(), root.read(Path::new(DMI).join(k))))
            .collect(),
            power_source: match root.read(&profile.paths.ac_adapter).as_deref() {
                Some("1") => "ac".into(),
                Some("0") => "battery".into(),
                _ => "unknown".into(),
            },
            platform_profile: root.read(&profile.paths.platform_profile),
            platform_profile_choices: root
                .read(&profile.paths.platform_profile_choices)
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect(),
            fn_q_override: Ledger::open(root).ok().and_then(|l| l.get(Key::Override)),
            cpu_settings: [
                ("no_turbo", "/sys/devices/system/cpu/intel_pstate/no_turbo"),
                (
                    "max_perf_pct",
                    "/sys/devices/system/cpu/intel_pstate/max_perf_pct",
                ),
                ("ignore_ppc", "/sys/module/processor/parameters/ignore_ppc"),
            ]
            .into_iter()
            .map(|(k, p)| (k.to_string(), root.read(p)))
            .collect(),
            governors: cpufreq_counts(root, "scaling_governor"),
            energy_performance_preference: cpufreq_counts(root, "energy_performance_preference"),
            firmware_limits,
            rapl: rapl(root),
            nvidia_power_limit_w: full.then(system::nvidia_power_limit).flatten(),
            fan_rpm: full
                .then(|| firmware::fan_rpm(root))
                .flatten()
                .map(|fans| fans.iter().map(|(n, v)| (n.to_string(), *v)).collect()),
            throttle_reasons: firmware::logged_throttle_reasons(root)
                .map(|r| r.into_iter().map(String::from).collect()),
            package_temp_c: package_temp(root),
            gpu_mode: gpu::mode(&outputs).into(),
            outputs,
            desktop_gpu: gpu::desktop_preference(root).into(),
            units: UNITS
                .iter()
                .filter(|_| full)
                .map(|u| UnitState {
                    unit: u.to_string(),
                    active: system::unit_state("is-active", u),
                    enabled: system::unit_state("is-enabled", u),
                })
                .collect(),
            modules: MODULES
                .iter()
                .map(|m| (m.to_string(), loaded.contains(*m)))
                .collect(),
            load: read_load(root),
        }
    }
}
