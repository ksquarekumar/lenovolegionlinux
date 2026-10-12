//! The machine profile: identity, paths and the policy for each power source. Read as JSONC (comments allowed).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

const DEFAULT_PATH: &str = "/usr/share/legionctl/profiles/lenovo-82wq-legion-pro-7-16irx8h.json";

#[derive(Debug, Deserialize)]
pub struct Profile {
    pub id: String,
    pub dmi_match: BTreeMap<String, String>,
    pub paths: Paths,
    /// Applies in every mode; a mode's own `sysctl` overrides per key.
    #[serde(default)]
    pub sysctl: BTreeMap<String, String>,
    /// CPU policy that follows a firmware mode the user chose with Fn+Q.
    #[serde(default)]
    pub firmware_follow: BTreeMap<String, Follow>,
    pub ac: ModePolicy,
    pub battery: ModePolicy,
    pub conflicts: Conflicts,
    #[serde(default)]
    pub kernel_modules: KernelModules,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

/// Units, packages and modules from earlier Legion tooling that fight this policy.
#[derive(Debug, Default, Deserialize)]
pub struct Conflicts {
    #[serde(default)]
    pub systemd_units: Vec<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub modules: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct KernelModules {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub unwanted_autoload: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct Capability {
    pub id: String,
    pub name: String,
    pub status: Option<String>,
    pub current_owner: Option<String>,
    pub planned_owner: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Paths {
    pub ac_adapter: String,
    pub platform_profile: String,
    pub platform_profile_choices: String,
    pub fan_mode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Follow {
    pub power_profile: Option<String>,
    pub governor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ModePolicy {
    #[serde(default)]
    pub cpu: Cpu,
    #[serde(default)]
    pub firmware: Firmware,
    /// Written in file order; the firmware may care which limit changes first.
    #[serde(default)]
    pub firmware_attributes: serde_json::Map<String, Value>,
    #[serde(default)]
    pub nvidia: Nvidia,
    pub power_profile: Option<String>,
    #[serde(default)]
    pub sysctl: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Cpu {
    pub ignore_ppc: Option<String>,
    pub no_turbo: Option<String>,
    pub max_perf_pct: Option<String>,
    pub governor: Option<String>,
    pub energy_performance_preference: Option<String>,
    pub energy_perf_bias: Option<String>,
    #[serde(default)]
    pub enable_all_idle_states: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct Firmware {
    #[serde(default)]
    pub platform_profile_preference: Vec<String>,
    pub fan_mode: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Nvidia {
    #[serde(default)]
    pub start_persistenced: bool,
    #[serde(default)]
    pub start_powerd: bool,
    #[serde(default)]
    pub set_persistence_mode: bool,
    /// NVIDIA board power limit expected on AC; nvidia-powerd may add up to 25 W (Dynamic Boost).
    pub expected_dynamic_power_w: Option<f64>,
}

/// Text written to a sysfs file for a profile value: strings as they are, numbers in decimal.
#[must_use]
pub fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A JSONC file (JSON with comments) as JSON.
///
/// # Errors
/// The file cannot be opened or is not valid JSONC.
pub fn read_jsonc(path: &Path) -> Result<Value> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    serde_json::from_reader(json_comments::StripComments::new(file))
        .with_context(|| format!("parsing {}", path.display()))
}

/// The installed profile, under the sysroot like every machine path (tests use a recorded machine).
#[must_use]
pub fn default_path() -> PathBuf {
    crate::sysroot::SysRoot::from_env().path(DEFAULT_PATH)
}

/// The profile at `path`, or the installed one ([`default_path`]).
///
/// # Errors
/// The file is not readable or is not a valid profile.
pub fn load(path: Option<&Path>) -> Result<(Profile, PathBuf)> {
    let path = path.map_or_else(default_path, Path::to_path_buf);
    let profile = serde_json::from_value(read_jsonc(&path)?)
        .with_context(|| format!("reading profile {}", path.display()))?;
    Ok((profile, path))
}

const PPD_PROFILES: [&str; 3] = ["performance", "balanced", "power-saver"];

/// Structure and ac/battery consistency checks that typed parsing cannot express. No hardware access.
///
/// # Errors
/// The file is not readable JSONC. Profile faults are findings, not errors.
pub fn validate(path: &Path) -> Result<Vec<String>> {
    let raw = read_jsonc(path)?;
    let mut errors = Vec::new();
    if let Err(e) = serde_json::from_value::<Profile>(raw.clone()) {
        errors.push(e.to_string());
    }
    let mode = |m: &str| raw.get(m).and_then(Value::as_object);
    let keys = |m: &str, block: &str| -> Vec<String> {
        mode(m)
            .and_then(|o| o.get(block))
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    for m in ["ac", "battery"] {
        if let Some(wanted) = mode(m)
            .and_then(|o| o.get("power_profile"))
            .and_then(Value::as_str)
            && !PPD_PROFILES.contains(&wanted)
        {
            errors.push(format!(
                "{m}.power_profile '{wanted}' is not one of {}",
                PPD_PROFILES.join(", ")
            ));
        }
    }
    // every policy knob must be set for both power sources, or a mode switch leaves the other's value behind
    for block in ["cpu", "firmware", "firmware_attributes"] {
        let (ac, bat) = (keys("ac", block), keys("battery", block));
        let mut drift: Vec<&String> = ac
            .iter()
            .filter(|k| !bat.contains(k))
            .chain(bat.iter().filter(|k| !ac.contains(k)))
            .collect();
        drift.sort();
        if !drift.is_empty() {
            let names: Vec<&str> = drift.iter().map(|s| s.as_str()).collect();
            errors.push(format!(
                "{block} keys differ between ac and battery: {}",
                names.join(", ")
            ));
        }
    }
    let has_ppd = |m: &str| mode(m).is_some_and(|o| o.contains_key("power_profile"));
    if has_ppd("ac") != has_ppd("battery") {
        errors.push("power_profile must be set for both ac and battery or neither".into());
    }
    let follow = raw.get("firmware_follow").and_then(Value::as_object);
    for (firmware_mode, entry) in follow.into_iter().flatten() {
        if let Some(p) = entry.get("power_profile").and_then(Value::as_str)
            && !PPD_PROFILES.contains(&p)
        {
            errors.push(format!(
                "firmware_follow.{firmware_mode}.power_profile '{p}' is not a PPD profile"
            ));
        }
        if let Some(g) = entry.get("governor").and_then(Value::as_str)
            && !["performance", "powersave"].contains(&g)
        {
            errors.push(format!(
                "firmware_follow.{firmware_mode}.governor '{g}' is not performance/powersave"
            ));
        }
    }
    for m in ["ac", "battery"] {
        let prefs = mode(m)
            .and_then(|o| o.get("firmware"))
            .and_then(|f| f.get("platform_profile_preference"))
            .and_then(Value::as_array);
        for choice in prefs.into_iter().flatten().filter_map(Value::as_str) {
            if follow.is_some_and(|f| !f.contains_key(choice)) {
                errors.push(format!(
                    "{m} platform_profile '{choice}' has no firmware_follow entry"
                ));
            }
        }
    }
    Ok(errors)
}
