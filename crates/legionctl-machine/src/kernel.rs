//! Kernel modules this machine relies on: loaded, built in, available for every installed kernel, autoloaded, and
//! built by DKMS; and the installed kernels themselves (`kernel list`, docs/design/08-kernel.md).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::sysroot::SysRoot;

const MODULES_LOAD_DIRS: [&str; 2] = ["/etc/modules-load.d", "/usr/lib/modules-load.d"];

#[must_use]
pub fn loaded_modules(root: &SysRoot) -> BTreeSet<String> {
    root.read("/proc/modules")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(String::from))
        .collect()
}

pub fn parse_modules_load(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(['#', ';']))
        .map(String::from)
        .collect()
}

/// `(module, first modules-load.d file that loads it)`, in the order the files and lines appear.
#[must_use]
pub fn autoload_entries(root: &SysRoot) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = Vec::new();
    for dir in MODULES_LOAD_DIRS {
        for conf in root.children(dir, "") {
            if conf.extension().is_none_or(|e| e != "conf") {
                continue;
            }
            for module in parse_modules_load(&root.read(&conf).unwrap_or_default()) {
                let module = module.replace('-', "_");
                if !entries.iter().any(|(m, _)| *m == module) {
                    entries.push((module, conf.display().to_string()));
                }
            }
        }
    }
    entries
}

#[must_use]
pub fn installed_kernels(root: &SysRoot) -> Vec<String> {
    root.children("/usr/lib/modules", "")
        .into_iter()
        .filter(|k| root.exists(k.join("vmlinuz")))
        .filter_map(|k| k.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

#[must_use]
pub fn running_kernel(root: &SysRoot) -> String {
    root.read("/proc/sys/kernel/osrelease").unwrap_or_default()
}

#[must_use]
pub fn module_available(name: &str, kernel: &str) -> bool {
    Command::new("modinfo")
        .args(["-k", kernel, "-F", "filename", name])
        .output()
        .map_or(true, |o| {
            o.status.success() && !o.stdout.trim_ascii().is_empty()
        })
}

#[must_use]
pub fn module_builtin(root: &SysRoot, name: &str, kernel: &str) -> bool {
    root.read(
        Path::new("/usr/lib/modules")
            .join(kernel)
            .join("modules.builtin"),
    )
    .unwrap_or_default()
    .lines()
    .any(|l| {
        Path::new(l)
            .file_name()
            .and_then(|f| f.to_str())
            .and_then(|f| f.split(".ko").next())
            .is_some_and(|m| m.replace('-', "_") == name)
    })
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

#[must_use]
pub fn dkms_status() -> Vec<(String, String, String)> {
    Command::new("dkms")
        .arg("status")
        .output()
        .map(|o| parse_dkms_status(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// The DKMS modules that `kernel list` shows for each kernel.
const DKMS_MODULES: [&str; 2] = ["nvidia", "legionctl-wmi"];

/// The package of the kernel `release` (`/usr/lib/modules/<release>/pkgbase`: `linux-cachyos`).
#[must_use]
pub fn package(root: &SysRoot, release: &str) -> Option<String> {
    root.read(Path::new("/usr/lib/modules").join(release).join("pkgbase"))
}

/// The package of the running kernel: the `kernel` condition of the configsets.
#[must_use]
pub fn running_package(root: &SysRoot) -> Option<String> {
    package(root, &running_kernel(root))
}

/// The release (`7.2.9-2-cachyos`) of the installed kernel package `name`.
#[must_use]
pub fn release_of(root: &SysRoot, name: &str) -> Option<String> {
    installed_kernels(root)
        .into_iter()
        .find(|r| package(root, r).as_deref() == Some(name))
}

/// The packages of the installed kernels.
#[must_use]
pub fn installed_packages(root: &SysRoot) -> Vec<String> {
    installed_kernels(root)
        .iter()
        .filter_map(|r| package(root, r))
        .collect()
}

/// Every module of the kernel `release`, built in or loadable (DKMS modules too), with `_` for `-`.
#[must_use]
pub fn modules(root: &SysRoot, release: &str) -> BTreeSet<String> {
    let dir = Path::new("/usr/lib/modules").join(release);
    let builtin = root
        .read(dir.join("modules.builtin.modinfo"))
        .unwrap_or_default()
        .split('\0')
        .filter_map(|entry| entry.split_once('.').map(|(m, _)| m.to_string()))
        .collect::<Vec<_>>();
    let files = ["modules.builtin", "modules.dep"]
        .iter()
        .flat_map(|f| {
            root.read(dir.join(f))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    let file = l.split(':').next()?.rsplit('/').next()?;
                    Some(file.split(".ko").next()?.to_string())
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    builtin
        .into_iter()
        .chain(files)
        .map(|m| m.replace('-', "_"))
        .collect()
}

/// The scheduler and build of the kernel `release`, from its configuration (`build/.config`, from the headers
/// package): `bore` or `eevdf`, and `lto`. Empty without the headers.
#[must_use]
pub fn flavour(root: &SysRoot, release: &str) -> Vec<String> {
    let Some(config) = root.read(
        Path::new("/usr/lib/modules")
            .join(release)
            .join("build/.config"),
    ) else {
        return Vec::new();
    };
    let on = |option: &str| config.lines().any(|l| l == format!("{option}=y"));
    let mut out = vec![
        if on("CONFIG_SCHED_BORE") {
            "bore"
        } else {
            "eevdf"
        }
        .to_string(),
    ];
    if on("CONFIG_LTO_CLANG_THIN") || on("CONFIG_LTO_CLANG_FULL") {
        out.push("lto".into());
    }
    out
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
#[must_use]
pub fn list(root: &SysRoot) -> Kernels {
    let running = running_kernel(root);
    let dkms = dkms_status();
    let default = crate::boot::default_kernel(root);
    let images = crate::boot::images(root);
    let kernels = installed_kernels(root)
        .into_iter()
        .filter_map(|release| {
            let package = package(root, &release)?;
            let image = crate::boot::image(&package);
            Some(Installed {
                flavour: flavour(root, &release),
                image: images.contains(&image).then_some(image),
                cmdline: crate::boot::saved_line(root, &package),
                dkms: DKMS_MODULES
                    .iter()
                    .map(|m| {
                        dkms.iter()
                            .find(|(module, kernel, _)| {
                                *kernel == release && module.split('/').next() == Some(m)
                            })
                            .map_or_else(
                                || format!("{m}: not built"),
                                |(module, _, state)| format!("{module}: {state}"),
                            )
                    })
                    .collect(),
                running: release == running,
                default: default.as_deref() == Some(package.as_str()),
                package,
                release,
            })
        })
        .collect();
    Kernels {
        takeover: default.is_some(),
        kernels,
    }
}
