//! Kernel modules this machine relies on: loaded, built in, available for every installed kernel, autoloaded, and
//! built by DKMS; and the installed kernels themselves (`kernel list`, docs/design/08-kernel.md).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::sysroot::SysRoot;

const MODULES_LOAD_DIRS: [&str; 2] = ["/etc/modules-load.d", "/usr/lib/modules-load.d"];

/// The loaded modules.
///
/// # Errors
/// `/proc/modules` cannot be read.
pub fn loaded_modules(root: &SysRoot) -> Result<BTreeSet<String>> {
    Ok(root
        .read("/proc/modules")?
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(String::from))
        .collect())
}

pub fn parse_modules_load(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(['#', ';']))
        .map(String::from)
        .collect()
}

/// `(module, first modules-load.d file that loads it)`, in the order the files and lines appear.
///
/// # Errors
/// A modules-load.d file cannot be read.
pub fn autoload_entries(root: &SysRoot) -> Result<Vec<(String, String)>> {
    let mut entries: Vec<(String, String)> = Vec::new();
    for dir in MODULES_LOAD_DIRS {
        for conf in root.children(dir, "")? {
            if conf.extension().is_none_or(|e| e != "conf") {
                continue;
            }
            for module in parse_modules_load(&root.read(&conf)?.unwrap_or_default()) {
                let module = module.replace('-', "_");
                if !entries.iter().any(|(m, _)| *m == module) {
                    entries.push((module, conf.display().to_string()));
                }
            }
        }
    }
    Ok(entries)
}

/// The releases in `/usr/lib/modules` that have a kernel image.
///
/// # Errors
/// `/usr/lib/modules` cannot be read.
pub fn installed_kernels(root: &SysRoot) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for k in root.children("/usr/lib/modules", "")? {
        if root.exists(k.join("vmlinuz"))?
            && let Some(name) = k.file_name()
        {
            out.push(name.to_string_lossy().into_owned());
        }
    }
    Ok(out)
}

/// The running kernel's release; empty without `/proc`.
///
/// # Errors
/// The release cannot be read.
pub fn running_kernel(root: &SysRoot) -> Result<String> {
    root.read("/proc/sys/kernel/osrelease")?
        .context("/proc/sys/kernel/osrelease is missing: the running kernel is unknown")
}

/// The module `name` exists for `kernel` (`modinfo` finds its file).
///
/// # Errors
/// `modinfo` does not run.
pub fn module_available(name: &str, kernel: &str) -> Result<bool> {
    let out = Command::new("modinfo")
        .args(["-k", kernel, "-F", "filename", name])
        .output()
        .context("running modinfo")?;
    Ok(out.status.success() && !out.stdout.trim_ascii().is_empty())
}

/// The module `name` is built into `kernel`.
///
/// # Errors
/// `modules.builtin` cannot be read.
pub fn module_builtin(root: &SysRoot, name: &str, kernel: &str) -> Result<bool> {
    Ok(root
        .read(
            Path::new("/usr/lib/modules")
                .join(kernel)
                .join("modules.builtin"),
        )?
        .unwrap_or_default()
        .lines()
        .any(|l| {
            Path::new(l)
                .file_name()
                .and_then(|f| f.to_str())
                .and_then(|f| f.split(".ko").next())
                .is_some_and(|m| m.replace('-', "_") == name)
        }))
}

/// `(module/version, kernel, state)` from `dkms status` lines like `nvidia/580, 7.2.9-cachyos, x86_64: installed`.
#[must_use]
pub fn parse_dkms_status(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|line| {
            let (head, state) = line.split_once(": ")?;
            let parts: Vec<&str> = head.split(',').map(str::trim).collect();
            (parts.len() >= 3 && !state.is_empty())
                .then(|| (parts[0].into(), parts[1].into(), state.trim().into()))
        })
        .collect()
}

/// `dkms status` as (module, kernel, state); empty when DKMS is not installed.
///
/// # Errors
/// `dkms` does not run or fails.
pub fn dkms_status() -> Result<Vec<(String, String, String)>> {
    let out = match Command::new("dkms").arg("status").output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        out => out.context("running dkms")?,
    };
    anyhow::ensure!(
        out.status.success(),
        "dkms status failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(parse_dkms_status(&String::from_utf8_lossy(&out.stdout)))
}

/// The DKMS modules that `kernel list` shows for each kernel.
const DKMS_MODULES: [&str; 2] = ["nvidia", "legionctl-wmi"];

/// The package of the kernel `release` (`/usr/lib/modules/<release>/pkgbase`: `linux-cachyos`).
///
/// # Errors
/// `pkgbase` cannot be read.
pub fn package(root: &SysRoot, release: &str) -> Result<Option<String>> {
    root.read(Path::new("/usr/lib/modules").join(release).join("pkgbase"))
}

/// The package of the running kernel: the `kernel` condition of the configsets.
///
/// # Errors
/// The release or its `pkgbase` cannot be read.
pub fn running_package(root: &SysRoot) -> Result<Option<String>> {
    package(root, &running_kernel(root)?)
}

/// The release (`7.2.9-2-cachyos`) of the installed kernel package `name`.
///
/// # Errors
/// `/usr/lib/modules` cannot be read.
pub fn release_of(root: &SysRoot, name: &str) -> Result<Option<String>> {
    for r in installed_kernels(root)? {
        if package(root, &r)?.as_deref() == Some(name) {
            return Ok(Some(r));
        }
    }
    Ok(None)
}

/// The packages of the installed kernels.
///
/// # Errors
/// `/usr/lib/modules` cannot be read.
pub fn installed_packages(root: &SysRoot) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for r in installed_kernels(root)? {
        out.extend(package(root, &r)?);
    }
    Ok(out)
}

/// Every module of the kernel `release`, built in or loadable (DKMS modules too), with `_` for `-`.
///
/// # Errors
/// A module list of the kernel cannot be read.
pub fn modules(root: &SysRoot, release: &str) -> Result<BTreeSet<String>> {
    let dir = Path::new("/usr/lib/modules").join(release);
    let mut out: Vec<String> = root
        .read(dir.join("modules.builtin.modinfo"))?
        .unwrap_or_default()
        .split('\0')
        .filter_map(|entry| entry.split_once('.').map(|(m, _)| m.to_string()))
        .collect();
    for f in ["modules.builtin", "modules.dep"] {
        out.extend(
            root.read(dir.join(f))?
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    let file = l.split(':').next()?.rsplit('/').next()?;
                    Some(file.split(".ko").next()?.to_string())
                }),
        );
    }
    Ok(out.into_iter().map(|m| m.replace('-', "_")).collect())
}

/// The configuration of the kernel `release` (`build/.config`, from the headers package).
fn config(root: &SysRoot, release: &str) -> Result<Option<String>> {
    root.read(
        Path::new("/usr/lib/modules")
            .join(release)
            .join("build/.config"),
    )
}

/// The Clang LTO mode of the kernel `release`: `thin`, `full` or `none`; the `_use_llvm_lto` value of the kernel
/// PKGBUILDs. `None` without the headers (no `build/.config`).
///
/// # Errors
/// The configuration cannot be read.
pub fn lto(root: &SysRoot, release: &str) -> Result<Option<&'static str>> {
    let Some(config) = config(root, release)? else {
        return Ok(None);
    };
    let on = |option: &str| config.lines().any(|l| l == format!("{option}=y"));
    Ok(Some(if on("CONFIG_LTO_CLANG_THIN") {
        "thin"
    } else if on("CONFIG_LTO_CLANG_FULL") {
        "full"
    } else {
        "none"
    }))
}

/// The scheduler and build of the kernel `release`, from its configuration: `bore` or `eevdf`, and `lto`. Empty
/// without the headers.
///
/// # Errors
/// The configuration cannot be read.
pub fn flavour(root: &SysRoot, release: &str) -> Result<Vec<String>> {
    let Some(config) = config(root, release)? else {
        return Ok(Vec::new());
    };
    let mut out = vec![
        if config.lines().any(|l| l == "CONFIG_SCHED_BORE=y") {
            "bore"
        } else {
            "eevdf"
        }
        .to_string(),
    ];
    if lto(root, release)? != Some("none") {
        out.push("lto".into());
    }
    Ok(out)
}

/// An installed kernel, as `kernel list` shows it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Installed {
    /// `linux-cachyos-bore-lto`
    pub package: String,
    /// `7.2.9-2-cachyos-bore-lto`
    pub release: String,
    /// `bore` or `eevdf`, and `lto`; empty without the headers.
    pub flavour: Vec<String>,
    /// legionctl's signed image of it on the ESP.
    pub image: Option<String>,
    /// Its command line (the source of its image); `None` until the boot takeover writes it.
    pub cmdline: Option<String>,
    /// `nvidia/580.178.04: installed`, `legionctl-wmi: not built`.
    pub dkms: Vec<String>,
    pub running: bool,
    /// legionctl's boot entry starts it when no one chooses.
    pub default: bool,
}

/// The installed kernels and how they start.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Kernels {
    /// The boot takeover is installed (`legionctl boot install`): `kernel default` and `kernel try` need it.
    pub takeover: bool,
    pub kernels: Vec<Installed>,
}

/// Every installed kernel: its package, flavour, image, command line, DKMS modules, and whether it runs and is the
/// default.
///
/// # Errors
/// A kernel's files or the boot entries cannot be read.
pub fn list(root: &SysRoot) -> Result<Kernels> {
    let running = running_kernel(root)?;
    // a dkms failure is the state of each kernel's DKMS modules, not a failure of the list
    let dkms = dkms_status().map_err(|e| format!("unknown: {e:#}"));
    let default = crate::boot::default_kernel(root)?;
    let images = crate::boot::images(root)?;
    let mut kernels = Vec::new();
    for release in installed_kernels(root)? {
        let Some(package) = package(root, &release)? else {
            continue;
        };
        let image = crate::boot::image(&package);
        kernels.push(Installed {
            flavour: flavour(root, &release)?,
            image: images.contains(&image).then_some(image),
            cmdline: crate::boot::saved_line(root, &package)?,
            dkms: DKMS_MODULES
                .iter()
                .map(|m| match &dkms {
                    Err(e) => format!("{m}: {e}"),
                    Ok(rows) => rows
                        .iter()
                        .find(|(module, kernel, _)| {
                            *kernel == release && module.split('/').next() == Some(m)
                        })
                        .map_or_else(
                            || format!("{m}: not built"),
                            |(module, _, state)| format!("{module}: {state}"),
                        ),
                })
                .collect(),
            running: release == running,
            default: default.as_deref() == Some(package.as_str()),
            package,
            release,
        });
    }
    Ok(Kernels {
        takeover: default.is_some(),
        kernels,
    })
}
