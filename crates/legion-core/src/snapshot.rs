//! Read-only view of this machine's power, firmware, GPU and load state. Needs no privileges.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use crate::load::{Load, read_load};
use crate::read;

const AC_ADAPTER: &str = "/sys/class/power_supply/ADP0/online";
const PLATFORM_PROFILE: &str = "/sys/firmware/acpi/platform_profile";
const FIRMWARE_OVERRIDE: &str = "/run/legion-control/firmware-override";
const FIRMWARE_ATTRIBUTES: &str = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes";
const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
const RAPL: &str = "/sys/class/powercap/intel-rapl:0";
const DRM: &str = "/sys/class/drm";
const PRIMARY_GPU_RULE: &str = "/etc/udev/rules.d/61-legion-mutter-primary.rules";

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GpuOutput {
    pub connector: String,
    pub card: String,
    pub driver: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Snapshot {
    /// "ac", "battery" or "unknown".
    pub power_source: String,
    /// Firmware thermal mode (Fn+Q): low-power, balanced, performance, max-power, custom.
    pub platform_profile: Option<String>,
    /// Set while legionctl follows a mode chosen with Fn+Q.
    pub fn_q_override: Option<String>,
    /// Lenovo firmware limits (W / °C), e.g. ppt_pl1_spl, ppt_pl2_sppt, ppt_cpu_cl, cpu_temp.
    pub firmware_limits: BTreeMap<String, String>,
    /// CPU frequency governors and how many policies use each.
    pub governors: BTreeMap<String, usize>,
    pub energy_performance_preference: BTreeMap<String, usize>,
    pub rapl_sustained_w: Option<f64>,
    pub rapl_burst_w: Option<f64>,
    pub package_temp_c: Option<f64>,
    /// Connected displays and the GPU driving each.
    pub displays: Vec<GpuOutput>,
    /// "hybrid" (panel on the iGPU), "discrete" (panel on NVIDIA) or "unknown".
    pub gpu_mode: String,
    /// GPU GNOME draws the desktop on: nvidia, intel or auto.
    pub desktop_gpu: String,
    pub load: Load,
}

fn count_values(dir: &Path, file: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with("policy")
            && let Some(value) = read(entry.path().join(file))
        {
            *counts.entry(value).or_default() += 1;
        }
    }
    counts
}

fn microwatts_to_w(path: PathBuf) -> Option<f64> {
    read(path)?.parse::<f64>().ok().map(|uw| uw / 1e6)
}

fn package_temp() -> Option<f64> {
    std::fs::read_dir("/sys/class/hwmon")
        .ok()?
        .flatten()
        .find(|e| read(e.path().join("name")).as_deref() == Some("coretemp"))
        .and_then(|e| read(e.path().join("temp1_input")))
        .and_then(|t| t.parse::<f64>().ok())
        .map(|millideg| millideg / 1000.0)
}

pub fn displays(drm: &Path) -> Vec<GpuOutput> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(drm).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((card, connector)) = name.split_once('-') else {
            continue;
        };
        if read(entry.path().join("status")).as_deref() != Some("connected") {
            continue;
        }
        let driver = std::fs::read_link(drm.join(card).join("device/driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        out.push(GpuOutput {
            connector: connector.into(),
            card: card.into(),
            driver,
        });
    }
    out.sort_by(|a, b| a.connector.cmp(&b.connector));
    out
}

/// Hybrid when the built-in panel (eDP) hangs off the iGPU; discrete when NVIDIA drives it (MUX switched).
pub fn gpu_mode(displays: &[GpuOutput]) -> &'static str {
    match displays.iter().find(|d| d.connector.starts_with("eDP")) {
        Some(panel) if panel.driver == "nvidia" => "discrete",
        Some(_) => "hybrid",
        None => "unknown",
    }
}

fn desktop_gpu() -> String {
    match read(PRIMARY_GPU_RULE) {
        Some(rule) if rule.contains("DRIVERS==\"nvidia\"") => "nvidia".into(),
        Some(rule) if rule.contains("DRIVERS==\"i915\"") => "intel".into(),
        _ => "auto".into(),
    }
}

impl Snapshot {
    pub fn read() -> Self {
        let firmware_limits = std::fs::read_dir(FIRMWARE_ATTRIBUTES)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                Some((
                    e.file_name().to_string_lossy().to_string(),
                    read(e.path().join("current_value"))?,
                ))
            })
            .collect();
        let displays = displays(Path::new(DRM));
        Snapshot {
            power_source: match read(AC_ADAPTER).as_deref() {
                Some("1") => "ac".into(),
                Some("0") => "battery".into(),
                _ => "unknown".into(),
            },
            platform_profile: read(PLATFORM_PROFILE),
            fn_q_override: read(FIRMWARE_OVERRIDE),
            firmware_limits,
            governors: count_values(Path::new(CPUFREQ), "scaling_governor"),
            energy_performance_preference: count_values(
                Path::new(CPUFREQ),
                "energy_performance_preference",
            ),
            rapl_sustained_w: microwatts_to_w(Path::new(RAPL).join("constraint_0_power_limit_uw")),
            rapl_burst_w: microwatts_to_w(Path::new(RAPL).join("constraint_1_power_limit_uw")),
            package_temp_c: package_temp(),
            gpu_mode: gpu_mode(&displays).into(),
            displays,
            desktop_gpu: desktop_gpu(),
            load: read_load(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(connector: &str, driver: &str) -> GpuOutput {
        GpuOutput {
            connector: connector.into(),
            card: "card0".into(),
            driver: driver.into(),
        }
    }

    #[test]
    fn gpu_mode_follows_the_panel() {
        assert_eq!(
            gpu_mode(&[out("DP-3", "nvidia"), out("eDP-1", "i915")]),
            "hybrid"
        );
        assert_eq!(gpu_mode(&[out("eDP-2", "nvidia")]), "discrete");
        assert_eq!(gpu_mode(&[out("DP-3", "nvidia")]), "unknown");
    }
}
