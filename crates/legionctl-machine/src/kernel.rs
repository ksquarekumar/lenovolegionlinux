//! Kernel modules this machine relies on: loaded, built in, available for every installed kernel, autoloaded, and
//! built by DKMS (docs/design/08-kernel.md).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::sysroot::SysRoot;

const MODULES_LOAD_DIRS: [&str; 2] = ["/etc/modules-load.d", "/usr/lib/modules-load.d"];

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

pub fn installed_kernels(root: &SysRoot) -> Vec<String> {
    root.children("/usr/lib/modules", "")
        .into_iter()
        .filter(|k| root.exists(k.join("vmlinuz")))
        .filter_map(|k| k.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

pub fn running_kernel(root: &SysRoot) -> String {
    root.read("/proc/sys/kernel/osrelease").unwrap_or_default()
}

pub fn module_available(name: &str, kernel: &str) -> bool {
    Command::new("modinfo")
        .args(["-k", kernel, "-F", "filename", name])
        .output()
        .map_or(true, |o| {
            o.status.success() && !o.stdout.trim_ascii().is_empty()
        })
}

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

pub fn dkms_status() -> Vec<(String, String, String)> {
    Command::new("dkms")
        .arg("status")
        .output()
        .map(|o| parse_dkms_status(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}
