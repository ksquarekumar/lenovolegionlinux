//! GPU wiring: which card drives each display, hybrid vs discrete (MUX) mode, the desktop GPU preference Mutter reads
//! at login, and the environment that makes a program render on a chosen GPU.

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use anyhow::{Context, Result, bail};

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

fn driver(root: &SysRoot, card: &str) -> Result<String> {
    Ok(root
        .link(Path::new(DRM).join(card).join("device/driver"))?
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default())
}

/// Every DRM connector, sorted by name.
///
/// # Errors
/// A connector's files cannot be read.
pub fn outputs(root: &SysRoot) -> Result<Vec<Output>> {
    let mut out = Vec::new();
    for path in root.children(DRM, "card")? {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some((card, connector)) = name.split_once('-') else {
            continue;
        };
        out.push(Output {
            connector: connector.into(),
            card: card.into(),
            driver: driver(root, card)?,
            connected: root.read(path.join("status"))?.as_deref() == Some("connected"),
        });
    }
    Ok(out)
}

/// `hybrid`: built-in panel on the iGPU; `discrete`: panel wired to NVIDIA (MUX); `unknown`: no connected panel.
#[must_use]
pub fn mode(outputs: &[Output]) -> GpuMode {
    match outputs
        .iter()
        .find(|o| o.connector.starts_with("eDP") && o.connected)
    {
        Some(panel) if panel.driver == "nvidia" => GpuMode::Discrete,
        Some(_) => GpuMode::Hybrid,
        None => GpuMode::Unknown,
    }
}

/// Which GPU drives the built-in panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GpuMode {
    /// The panel is on the iGPU.
    Hybrid,
    /// The panel is on NVIDIA.
    Discrete,
    Unknown,
}

/// The GPU Mutter draws the desktop on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DesktopGpu {
    Nvidia,
    Intel,
    /// No rule: Mutter picks the GPU of the built-in panel.
    Auto,
}

impl DesktopGpu {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DesktopGpu::Nvidia => "nvidia",
            DesktopGpu::Intel => "intel",
            DesktopGpu::Auto => "auto",
        }
    }
}

impl std::str::FromStr for DesktopGpu {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        [DesktopGpu::Nvidia, DesktopGpu::Intel, DesktopGpu::Auto]
            .into_iter()
            .find(|g| g.as_str() == s)
            .ok_or_else(|| anyhow::anyhow!("desktop GPU must be nvidia, intel or auto"))
    }
}

impl std::fmt::Display for DesktopGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for GpuMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GpuMode::Hybrid => "hybrid",
            GpuMode::Discrete => "discrete",
            GpuMode::Unknown => "unknown",
        })
    }
}

const PRIMARY_DRIVERS: [(DesktopGpu, &str); 2] =
    [(DesktopGpu::Nvidia, "nvidia"), (DesktopGpu::Intel, "i915")];

/// The GPU Mutter draws the desktop on, from the udev rule; `auto` without one.
///
/// # Errors
/// The rule cannot be read.
pub fn desktop_preference(root: &SysRoot) -> Result<DesktopGpu> {
    let rule = root.read(PRIMARY_GPU_RULE)?.unwrap_or_default();
    Ok(PRIMARY_DRIVERS
        .iter()
        .find(|(_, driver)| rule.contains(&format!("DRIVERS==\"{driver}\"")))
        .map_or(DesktopGpu::Auto, |(gpu, _)| *gpu))
}

/// The udev rule for Mutter: the GPU it draws on (none for `auto`), and with `release_nvidia` the NVIDIA GPU it must
/// not open, so that the GPU can go to a VM (`gpu give`).
#[must_use]
pub fn primary_rule(choice: DesktopGpu, release_nvidia: bool) -> Option<String> {
    let primary = PRIMARY_DRIVERS
        .iter()
        .find(|(gpu, _)| *gpu == choice)
        .map(|(_, driver)| {
            format!("SUBSYSTEM==\"drm\", KERNEL==\"card*\", DRIVERS==\"{driver}\", TAG+=\"mutter-device-preferred-primary\"\n")
        });
    let ignore = release_nvidia.then(|| {
        format!(
            "SUBSYSTEM==\"drm\", KERNEL==\"card*\", DRIVERS==\"nvidia\", TAG+=\"{IGNORE_TAG}\"\n"
        )
    });
    (primary.is_some() || ignore.is_some()).then(|| {
        format!(
            "# legionctl: the GPU Mutter draws the desktop on, and a GPU it leaves alone (read at login)\n{}{}",
            primary.unwrap_or_default(),
            ignore.unwrap_or_default()
        )
    })
}

const IGNORE_TAG: &str = "mutter-device-ignore";

/// CPU list of the performance (`p`) or efficiency (`e`) cores on hybrid Intel CPUs.
///
/// # Errors
/// The CPU list cannot be read.
pub fn core_class_cpus(root: &SysRoot, class: &str) -> Result<Option<String>> {
    let dir = match class {
        "p" => "cpu_core",
        "e" => "cpu_atom",
        _ => return Ok(None),
    };
    root.read(Path::new("/sys/devices").join(dir).join("cpus"))
}

/// Environment that makes a program render on `gpu` (`nvidia`, `intel`); empty for the default.
///
/// # Errors
/// A DRM card's files cannot be read.
pub fn render_env(root: &SysRoot, gpu: &str) -> Result<BTreeMap<&'static str, String>> {
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
            for card in root.children(DRM, "card")? {
                let name = card
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                if name.contains('-') || !["i915", "xe"].contains(&driver(root, &name)?.as_str()) {
                    continue;
                }
                let device = root
                    .read(card.join("device/device"))?
                    .with_context(|| format!("{} has no device id", card.display()))?;
                env.insert(
                    "MESA_VK_DEVICE_SELECT",
                    format!("8086:{}!", device.trim_start_matches("0x")),
                );
            }
        }
        _ => {}
    }
    Ok(env)
}

/// Plan the desktop GPU choice; Mutter reads it at the next login.
///
/// # Errors
/// The plan cannot be made.
pub fn set_desktop(
    machine: &Machine,
    choice: DesktopGpu,
    release_nvidia: bool,
) -> Result<Vec<Step>> {
    if release_nvidia && choice == DesktopGpu::Nvidia {
        bail!("the desktop cannot draw on the NVIDIA GPU and leave it alone: choose intel or auto");
    }
    let mut steps = match choice {
        DesktopGpu::Auto if !release_nvidia => vec![
            Step::Remove {
                path: PRIMARY_GPU_RULE.into(),
            },
            Step::Info("Mutter picks the GPU of the built-in panel".into()),
        ],
        _ => match primary_rule(choice, release_nvidia) {
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

/// The NVIDIA GPU's runtime power management (PCI): `runtime_status` (`active`, `suspended`: D3cold on this laptop) and
/// `control` (`auto`: it may sleep when no program uses it; `on`: it stays powered).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NvidiaPower {
    pub device: String,
    pub runtime_status: Option<String>,
    pub control: Option<String>,
    /// The programs that keep it awake (only the ones the reader may see).
    pub users: Vec<String>,
    /// Processes whose open files the reader may not see (another user's, or not dumpable): not in `users`.
    pub not_visible: usize,
}

pub(crate) fn control_path(device: &str) -> String {
    format!("/sys/bus/pci/devices/{device}/power/control")
}

/// The NVIDIA GPU's power state; `None` without an NVIDIA GPU.
///
/// # Errors
/// The GPU's files cannot be read.
pub fn nvidia_power(root: &SysRoot) -> Result<Option<NvidiaPower>> {
    let Some(device) = crate::vfio::nvidia_device(root)? else {
        return Ok(None);
    };
    let group = crate::vfio::nvidia_devices(root, &device)?;
    let (users, not_visible) = crate::vfio::users(root, &group, false)?;
    Ok(Some(NvidiaPower {
        runtime_status: root.read(format!(
            "/sys/bus/pci/devices/{device}/power/runtime_status"
        ))?,
        control: root.read(control_path(&device))?,
        users,
        not_visible,
        device,
    }))
}

/// Plan the NVIDIA GPU's runtime power management: `sleep` lets it go to D3cold when no program uses it (`auto`),
/// else it stays powered (`on`). Sleep is refused while a program uses it, since it would not sleep then.
///
/// # Errors
/// No NVIDIA GPU, the GPU is not on Linux's `nvidia` driver (its owner manages its power), or a program uses it.
pub fn set_nvidia_power(machine: &Machine, sleep: bool) -> Result<Vec<Step>> {
    let root = machine.root;
    let Some(power) = nvidia_power(root)? else {
        bail!("this machine has no NVIDIA GPU");
    };
    let driver = crate::vfio::driver(root, &power.device)?;
    if driver.as_deref() != Some("nvidia") {
        bail!(
            "the NVIDIA GPU is on {}, not on Linux's nvidia driver: its owner manages its power (legionctl gpu give \
             nvidia linux gives it back)",
            driver.as_deref().unwrap_or("no driver")
        );
    }
    if sleep && !power.users.is_empty() {
        bail!(
            "the NVIDIA GPU cannot sleep while programs use it:\n{}\nclose them; if gnome-shell holds it, run \
             `legionctl gpu primary intel --release-nvidia` and log in again",
            power.users.join("\n")
        );
    }
    let mut steps = Vec::new();
    machine.write(
        &mut steps,
        control_path(&power.device),
        if sleep { "auto" } else { "on" },
    )?;
    if steps.is_empty() {
        steps.push(Step::Info(format!(
            "the NVIDIA GPU's power/control is already {}",
            power.control.as_deref().unwrap_or("?")
        )));
    }
    Ok(steps)
}
