//! The graphics stack (todo-81): what the GPUs, the compositor and the apps need to render well on this machine, as
//! doctor findings with their fix: the NVIDIA libraries match the loaded kernel module, each GPU's Vulkan and EGL
//! drivers are there, NVIDIA's DRM driver does modesetting (Wayland), switcheroo-control runs (GNOME's "Launch using
//! Discrete Graphics Card"), and `OpenCL` reaches both GPUs.

use crate::sysroot::SysRoot;

/// The loaded GPU drivers (from the DRM cards).
fn drivers(root: &SysRoot) -> Vec<String> {
    root.children("/sys/class/drm", "card")
        .iter()
        .filter(|c| !c.to_string_lossy().contains('-'))
        .filter_map(|c| root.link(c.join("device/driver")))
        .filter_map(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// The graphics findings; empty when the stack is complete.
#[must_use]
pub fn findings(root: &SysRoot) -> Vec<String> {
    let drivers = drivers(root);
    let nvidia = drivers.iter().any(|d| d == "nvidia");
    let intel = drivers.iter().any(|d| d == "i915" || d == "xe");
    let mut out = if nvidia {
        nvidia_findings(root)
    } else {
        Vec::new()
    };
    out.extend(driver_findings(root, nvidia, intel));
    out
}

/// The NVIDIA module and its libraries, and modesetting.
fn nvidia_findings(root: &SysRoot) -> Vec<String> {
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
        out.push(format!(
            "the NVIDIA kernel module is {module} and its libraries are {}: GPU apps fail until they match \
             (restart after the driver update, or rebuild the module: dkms autoinstall)",
            libraries.join(" ")
        ));
    }
    if root
        .read("/sys/module/nvidia_drm/parameters/modeset")
        .as_deref()
        != Some("Y")
    {
        out.push(
            "nvidia-drm does not do modesetting: Wayland cannot use the NVIDIA GPU (add nvidia-drm.modeset=1 to the \
             kernel line)"
                .into(),
        );
    }
    out
}

/// Each GPU's Vulkan and EGL drivers; with both GPUs, switcheroo-control and `OpenCL` for the iGPU.
fn driver_findings(root: &SysRoot, nvidia: bool, intel: bool) -> Vec<String> {
    let mut out = Vec::new();
    for (needed, file, what) in [
        (
            nvidia,
            "/usr/share/vulkan/icd.d/nvidia_icd.json",
            "the NVIDIA Vulkan driver (nvidia-utils)",
        ),
        (
            intel,
            "/usr/share/vulkan/icd.d/intel_icd.json",
            "the Intel Vulkan driver (vulkan-intel)",
        ),
        (
            nvidia,
            "/usr/share/glvnd/egl_vendor.d/10_nvidia.json",
            "NVIDIA's EGL (nvidia-utils)",
        ),
        (
            intel,
            "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
            "Mesa's EGL (mesa)",
        ),
    ] {
        if needed && !root.exists(file) {
            out.push(format!("{file} is missing: install {what}"));
        }
    }
    if !(nvidia && intel) {
        return out;
    }
    if crate::system::unit_state("is-active", "switcheroo-control.service") != "active" {
        out.push(
            "switcheroo-control does not run: GNOME has no \"Launch using Discrete Graphics Card\" (sudo systemctl \
             enable --now switcheroo-control)"
                .into(),
        );
    }
    let icds: Vec<String> = root
        .children("/etc/OpenCL/vendors", "")
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    if !icds.iter().any(|i| i.starts_with("intel")) && icds.iter().any(|i| i == "rusticl.icd") {
        out.push(
            "OpenCL reaches the Intel GPU only through Mesa's rusticl, which needs RUSTICL_ENABLE=iris (or install \
             intel-compute-runtime)"
                .into(),
        );
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

/// Electron and Chromium apps of user `home` that draw through `XWayland`: blurry at a fractional scale, but sharper
/// at integer scales, where Chromium on Wayland draws its text in grayscale (user report 2026-10-14).
#[must_use]
pub fn xwayland_apps(root: &SysRoot, home: &std::path::Path) -> Vec<String> {
    FLAGGED_APPS
        .iter()
        .filter(|(binary, _, _)| root.exists(binary))
        .filter(|(_, file, _)| {
            let flags = std::fs::read_to_string(home.join(".config").join(file)).unwrap_or_default();
            !flags.lines().any(|l| l.trim_start().starts_with("--ozone-platform"))
        })
        .map(|(binary, file, flag)| {
            format!(
                "{binary} draws through XWayland, blurry at this fractional scale: add {flag} on its own line to \
                 ~/.config/{file}"
            )
        })
        .collect()
}
