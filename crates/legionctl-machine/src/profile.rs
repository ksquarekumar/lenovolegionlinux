//! The machine profile: identity, DMI match, paths, capabilities and conflicts. Read as JSONC (comments allowed). The
//! policy is in the profile's configset layers (`configsets/<profile id>/`, note 01).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

pub const DEFAULT_PATH: &str =
    "/usr/share/legionctl/profiles/lenovo-82wq-legion-pro-7-16irx8h.json";

#[derive(Debug, Deserialize)]
pub struct Profile {
    pub id: String,
    pub dmi_match: BTreeMap<String, String>,
    pub paths: Paths,
    pub conflicts: Conflicts,
    #[serde(default)]
    pub kernel_modules: KernelModules,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Fan control (`legionctl fan`); without it legionctl does not touch the fans.
    pub fan: Option<FanControl>,
    #[serde(default)]
    pub nvidia: Nvidia,
}

#[derive(Debug, Deserialize)]
pub struct FanControl {
    /// The curve when the user has none: written at each entry to custom mode and by `fan curve reset`.
    pub default_curve: Vec<u8>,
    /// The fan table and full-speed writes through `legionctl_wmi` are verified on this firmware (the user's first
    /// live write, todo-17 step 4); until then legionctl makes none of them, also not the automatic write at
    /// custom-mode entry.
    #[serde(default)]
    pub curve_verified: bool,
    /// The EC fan lock (a raw EC write) is verified for this EC; until then it is refused.
    #[serde(default)]
    pub ec_fan_lock_verified: bool,
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
pub struct Nvidia {
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

/// The shipped configset layers of a profile file: `configsets/<id>/` beside its `profiles/` folder, in the repository
/// and installed (`/usr/share/legionctl`).
#[must_use]
pub fn layers_dir(path: &Path, id: &str) -> PathBuf {
    path.parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/"))
        .join("configsets")
        .join(id)
}

/// The profile's structure, its fan curve, and its shipped configset layers (each layer's knobs and values, and the
/// ac/battery rule). No hardware access.
///
/// # Errors
/// The file is not readable JSONC. Profile faults are findings, not errors.
pub fn validate(path: &Path) -> Result<Vec<String>> {
    let raw = read_jsonc(path)?;
    Ok(match serde_json::from_value::<Profile>(raw) {
        Err(e) => vec![e.to_string()],
        Ok(profile) => {
            let mut errors = Vec::new();
            if let Some(fan) = &profile.fan
                && let Err(e) = crate::fan::validate(&fan.default_curve)
            {
                errors.push(format!("fan.default_curve: {e}"));
            }
            errors.extend(crate::configset::validate(
                &profile,
                &layers_dir(path, &profile.id),
            ));
            errors
        }
    })
}
