//! The graphics stack (todo-81): what the GPUs, the compositor and the apps need to render well on this machine, as
//! doctor findings with their fix: the NVIDIA libraries match the loaded kernel module, each GPU's Vulkan and EGL
//! drivers are there, NVIDIA's DRM driver does modesetting (Wayland), switcheroo-control runs (GNOME's "Launch using
//! Discrete Graphics Card"), and `OpenCL` reaches both GPUs. Each finding has a stable id; the fixes of the user's own
//! settings and files are operations (`legionctl display fix <id>`), the others stay text.

use std::path::{Path, PathBuf};

use crate::sysroot::SysRoot;

/// A finding: its stable id, what is wrong (with the fix as text), and the fix as changes of the user's own settings
/// and files (empty: a system change, which stays text).
pub struct Finding {
    pub id: String,
    pub text: String,
    pub fixes: Vec<Fix>,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.id, self.text)
    }
}

impl Finding {
    /// A finding whose fix is a system change: text only.
    #[must_use]
    pub fn system(id: &str, text: String) -> Self {
        Self {
            id: id.into(),
            text,
            fixes: Vec::new(),
        }
    }
}

/// One change of the user's own settings or files; the event log keeps the change that gives the old value back.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fix {
    /// The file's new content; `None` removes it.
    File {
        path: PathBuf,
        content: Option<String>,
    },
    /// A key of `org.gnome.desktop.interface` to a value (`GVariant` text).
    Setting { key: String, value: String },
    /// A monitor (connector) to a mode of Mutter's `DisplayConfig` (mode id).
    Mode { connector: String, mode: String },
}

/// The loaded GPU drivers (from the DRM cards).
fn drivers(root: &SysRoot) -> Vec<String> {
    root.children("/sys/class/drm", "card")
        .iter()
        .filter(|c| !c.to_string_lossy().contains('-'))
        .filter_map(|c| root.link(c.join("device/driver")))
        .filter_map(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// The graphics findings for user `home`; empty when the stack is complete.
#[must_use]
pub fn findings(root: &SysRoot, home: &Path) -> Vec<Finding> {
    let drivers = drivers(root);
    let nvidia = drivers.iter().any(|d| d == "nvidia");
    let intel = drivers.iter().any(|d| d == "i915" || d == "xe");
    let mut out = if nvidia {
        nvidia_findings(root)
    } else {
        Vec::new()
    };
    out.extend(driver_findings(root, home, nvidia, intel));
    out
}

/// The NVIDIA module and its libraries, and modesetting.
fn nvidia_findings(root: &SysRoot) -> Vec<Finding> {
    let mut out = Vec::new();
    // "NVRM version: NVIDIA UNIX x86_64 Kernel Module  580.178.04  ..."
    let module = root.read("/proc/driver/nvidia/version").and_then(|v| {
        let mut words = v.split_whitespace().skip_while(|w| *w != "Module");
        words.nth(1).map(String::from)
    });
    let libraries: Vec<String> = root
        .children("/usr/lib", "libnvidia-glcore.so.")
        .iter()
        .filter_map(|p| {
            p.file_name()?
                .to_string_lossy()
                .strip_prefix("libnvidia-glcore.so.")
                .map(String::from)
        })
        .collect();
    if let Some(module) = &module
        && !libraries.is_empty()
        && !libraries.contains(module)
    {
        out.push(Finding::system(
            "nvidia-libraries",
            format!(
                "the NVIDIA kernel module is {module} and its libraries are {}: GPU apps fail until they match \
                 (restart after the driver update, or rebuild the module: dkms autoinstall)",
                libraries.join(" ")
            ),
        ));
    }
    if root
        .read("/sys/module/nvidia_drm/parameters/modeset")
        .as_deref()
        != Some("Y")
    {
        out.push(Finding::system(
            "nvidia-modeset",
            "nvidia-drm does not do modesetting: Wayland cannot use the NVIDIA GPU (add nvidia-drm.modeset=1 to the \
             kernel line)"
                .into(),
        ));
    }
    out
}

/// Each GPU's Vulkan and EGL drivers; with both GPUs, switcheroo-control and `OpenCL` for the iGPU.
fn driver_findings(root: &SysRoot, home: &Path, nvidia: bool, intel: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    for (id, needed, file, what) in [
        (
            "vulkan-nvidia",
            nvidia,
            "/usr/share/vulkan/icd.d/nvidia_icd.json",
            "the NVIDIA Vulkan driver (nvidia-utils)",
        ),
        (
            "vulkan-intel",
            intel,
            "/usr/share/vulkan/icd.d/intel_icd.json",
            "the Intel Vulkan driver (vulkan-intel)",
        ),
        (
            "egl-nvidia",
            nvidia,
            "/usr/share/glvnd/egl_vendor.d/10_nvidia.json",
            "NVIDIA's EGL (nvidia-utils)",
        ),
        (
            "egl-mesa",
            intel,
            "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
            "Mesa's EGL (mesa)",
        ),
    ] {
        if needed && !root.exists(file) {
            out.push(Finding::system(
                id,
                format!("{file} is missing: install {what}"),
            ));
        }
    }
    if !(nvidia && intel) {
        return out;
    }
    if crate::system::unit_state("is-active", "switcheroo-control.service") != "active" {
        out.push(Finding::system(
            "switcheroo",
            "switcheroo-control does not run: GNOME has no \"Launch using Discrete Graphics Card\" (sudo systemctl \
             enable --now switcheroo-control)"
                .into(),
        ));
    }
    let icds: Vec<String> = root
        .children("/etc/OpenCL/vendors", "")
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    // the user session's environment (systemd environment.d) from the next login
    let environment = home.join(".config/environment.d");
    let iris = std::fs::read_dir(&environment)
        .into_iter()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.ok()?.path()).ok())
        .any(|conf| {
            conf.lines().any(|l| {
                l.trim()
                    .strip_prefix("RUSTICL_ENABLE=")
                    .is_some_and(|v| v.contains("iris"))
            })
        });
    if !icds.iter().any(|i| i.starts_with("intel"))
        && icds.iter().any(|i| i == "rusticl.icd")
        && !iris
    {
        out.push(Finding {
            id: "opencl-iris".into(),
            text: "OpenCL reaches the Intel GPU only through Mesa's rusticl, which needs RUSTICL_ENABLE=iris from \
                   the next login (legionctl display fix opencl-iris, or install intel-compute-runtime)"
                .into(),
            fixes: vec![Fix::File {
                path: environment.join("legionctl-opencl.conf"),
                content: Some("RUSTICL_ENABLE=iris\n".into()),
            }],
        });
    }
    out
}

/// Apps whose flags file decides Wayland or `XWayland`: the binary, the flags file in `~/.config`, the flag to add.
pub const FLAGGED_APPS: [(&str, &str, &str); 5] = [
    (
        "/usr/bin/code",
        "code-flags.conf",
        "--ozone-platform-hint=auto",
    ),
    (
        "/usr/bin/chromium",
        "chromium-flags.conf",
        "--ozone-platform-hint=auto",
    ),
    (
        "/usr/bin/thorium-browser",
        "thorium-flags.conf",
        "--ozone-platform=wayland",
    ),
    (
        "/usr/bin/brave",
        "brave-flags.conf",
        "--ozone-platform-hint=auto",
    ),
    (
        "/usr/bin/google-chrome-stable",
        "chrome-flags.conf",
        "--ozone-platform-hint=auto",
    ),
];

/// The Electron and Chromium apps of user `home` whose flags file does not suit the scale: at a fractional scale an
/// app through `XWayland` is blurry (add the Wayland flag); at integer scales `XWayland` is sharper, because Chromium on
/// Wayland draws its text in grayscale (remove the flag; user report 2026-10-14).
#[must_use]
pub fn app_findings(root: &SysRoot, home: &Path, fractional: bool) -> Vec<Finding> {
    let ozone = |l: &str| l.trim_start().starts_with("--ozone-platform");
    FLAGGED_APPS
        .iter()
        .filter(|(binary, _, _)| root.exists(binary))
        .filter_map(|(binary, file, flag)| {
            let path = home.join(".config").join(file);
            let flags = std::fs::read_to_string(&path).unwrap_or_default();
            let app = file.trim_end_matches("-flags.conf");
            let wayland = flags.lines().any(ozone);
            if fractional && !wayland {
                let sep = if flags.is_empty() || flags.ends_with('\n') {
                    ""
                } else {
                    "\n"
                };
                let id = format!("xwayland-{app}");
                return Some(Finding {
                    text: format!(
                        "{binary} draws through XWayland, blurry at this fractional scale: add {flag} on its own \
                         line to ~/.config/{file} (legionctl display fix {id})"
                    ),
                    id,
                    fixes: vec![Fix::File {
                        path,
                        content: Some(format!("{flags}{sep}{flag}\n")),
                    }],
                });
            }
            if fractional || !wayland {
                return None;
            }
            let kept: String = flags
                .split_inclusive('\n')
                .filter(|l| !ozone(l))
                .collect();
            let id = format!("wayland-{app}");
            Some(Finding {
                text: format!(
                    "{binary} draws on Wayland (~/.config/{file}), where its text is grayscale: at integer scales \
                     XWayland is sharper, remove the --ozone-platform line (legionctl display fix {id})"
                ),
                id,
                fixes: vec![Fix::File {
                    path,
                    content: (!kept.is_empty()).then_some(kept),
                }],
            })
        })
        .collect()
}
