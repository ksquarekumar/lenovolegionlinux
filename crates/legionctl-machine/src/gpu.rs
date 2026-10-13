//! GPU wiring: which card drives each display, hybrid vs discrete (MUX) mode, the desktop GPU preference Mutter reads
//! at login, and the environment that makes a program render on a chosen GPU.

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use anyhow::{Result, bail};

use crate::policy::{Machine, Step};
use crate::sysroot::SysRoot;

const DRM: &str = "/sys/class/drm";
pub const PRIMARY_GPU_RULE: &str = "/etc/udev/rules.d/61-legionctl-mutter-primary.rules";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Output {
    pub connector: String,
    pub card: String,
    pub driver: String,
    pub connected: bool,
}

fn driver(root: &SysRoot, card: &str) -> String {
    root.link(Path::new(DRM).join(card).join("device/driver"))
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// Every DRM connector, sorted by name.
#[must_use]
pub fn outputs(root: &SysRoot) -> Vec<Output> {
    root.children(DRM, "card")
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let (card, connector) = name.split_once('-')?;
            Some(Output {
                connector: connector.into(),
                card: card.into(),
                driver: driver(root, card),
                connected: root.read(path.join("status")).as_deref() == Some("connected"),
            })
        })
        .collect()
}

/// `hybrid`: built-in panel on the iGPU; `discrete`: panel wired to NVIDIA (MUX); `unknown`: no connected panel.
#[must_use]
pub fn mode(outputs: &[Output]) -> &'static str {
    match outputs
        .iter()
        .find(|o| o.connector.starts_with("eDP") && o.connected)
    {
        Some(panel) if panel.driver == "nvidia" => "discrete",
        Some(_) => "hybrid",
        None => "unknown",
    }
}

const PRIMARY_DRIVERS: [(&str, &str); 2] = [("nvidia", "nvidia"), ("intel", "i915")];

/// The GPU Mutter draws the desktop on (`nvidia`, `intel`), or `auto` without a rule.
#[must_use]
pub fn desktop_preference(root: &SysRoot) -> &'static str {
    let rule = root.read(PRIMARY_GPU_RULE).unwrap_or_default();
    PRIMARY_DRIVERS
        .iter()
        .find(|(_, driver)| rule.contains(&format!("DRIVERS==\"{driver}\"")))
        .map_or("auto", |(name, _)| name)
}

#[must_use]
pub fn primary_rule(choice: &str) -> Option<String> {
    let (_, driver) = PRIMARY_DRIVERS.iter().find(|(name, _)| *name == choice)?;
    Some(format!(
        "# legionctl: GPU that Mutter uses to draw the desktop (read at login)\n\
         SUBSYSTEM==\"drm\", KERNEL==\"card*\", DRIVERS==\"{driver}\", TAG+=\"mutter-device-preferred-primary\"\n"
    ))
}

/// CPU list of the performance (`p`) or efficiency (`e`) cores on hybrid Intel CPUs.
#[must_use]
pub fn core_class_cpus(root: &SysRoot, class: &str) -> Option<String> {
    let dir = match class {
        "p" => "cpu_core",
        "e" => "cpu_atom",
        _ => return None,
    };
    root.read(Path::new("/sys/devices").join(dir).join("cpus"))
}

/// Environment that makes a program render on `gpu` (`nvidia`, `intel`); empty for the default.
#[must_use]
pub fn render_env(root: &SysRoot, gpu: &str) -> BTreeMap<&'static str, String> {
    let mut env = BTreeMap::new();
    match gpu {
        "nvidia" => {
            env.insert("__NV_PRIME_RENDER_OFFLOAD", "1".into());
            env.insert("__GLX_VENDOR_LIBRARY_NAME", "nvidia".into());
            env.insert("__VK_LAYER_NV_optimus", "NVIDIA_only".into());
        }
        "intel" => {
            env.insert("__GLX_VENDOR_LIBRARY_NAME", "mesa".into());
            env.insert(
                "__EGL_VENDOR_LIBRARY_FILENAMES",
                "/usr/share/glvnd/egl_vendor.d/50_mesa.json".into(),
            );
            for card in root.children(DRM, "card") {
                let name = card
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                if name.contains('-') || !["i915", "xe"].contains(&driver(root, &name).as_str()) {
                    continue;
                }
                let device = root.read(card.join("device/device")).unwrap_or_default();
                env.insert(
                    "MESA_VK_DEVICE_SELECT",
                    format!("8086:{}!", device.trim_start_matches("0x")),
                );
            }
        }
        _ => {}
    }
    env
}

/// Plan the desktop GPU choice (`nvidia`, `intel`, `auto`); Mutter reads it at the next login.
///
/// # Errors
/// `choice` is not `nvidia`, `intel` or `auto`.
pub fn set_desktop(machine: &Machine, choice: &str) -> Result<Vec<Step>> {
    let mut steps = match choice {
        "auto" => vec![
            Step::Remove {
                path: PRIMARY_GPU_RULE.into(),
            },
            Step::Info("Mutter picks the GPU of the built-in panel".into()),
        ],
        _ => match primary_rule(choice) {
            Some(content) => vec![Step::Create {
                path: PRIMARY_GPU_RULE.into(),
                content,
            }],
            None => bail!("desktop GPU must be nvidia, intel or auto"),
        },
    };
    machine.run(&mut steps, &["udevadm", "control", "--reload"]);
    machine.run(&mut steps, &["udevadm", "trigger", "--subsystem-match=drm"]);
    steps.push(Step::Info(
        "log out and back in for GNOME to draw the desktop on the new GPU".into(),
    ));
    Ok(steps)
}
