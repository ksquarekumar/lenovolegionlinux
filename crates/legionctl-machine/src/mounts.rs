//! Mounts and what uses them (todo-85, note 22): each mounted block device and each `/etc/fstab` entry, and the
//! processes whose working folder or an open file is on it, as `fuser -m` finds them.

use std::path::{Path, PathBuf};

use crate::sysroot::SysRoot;

/// One mount point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub target: PathBuf,
    /// `/dev/nvme0n1p4`, `UUID=...`.
    pub source: String,
    pub fstype: String,
    pub mounted: bool,
    /// `<pid> <program>` of each process that uses it.
    pub users: Vec<String>,
}

/// `\040` and the other octal escapes of the mount table and fstab.
fn unescape(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\134", "\\")
}

/// The mounted block devices (sources in `/dev`), then the fstab entries that are not mounted; swap is not a mount.
#[must_use]
pub fn list(root: &SysRoot) -> Vec<Mount> {
    let table = |file: &str| -> Vec<(String, PathBuf, String)> {
        root.read(file)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                (f.len() > 2 && f[2] != "swap" && f[1].starts_with('/')).then(|| {
                    (
                        f[0].to_string(),
                        PathBuf::from(unescape(f[1])),
                        f[2].to_string(),
                    )
                })
            })
            .collect()
    };
    let mounted: Vec<(String, PathBuf, String)> = table("/proc/self/mounts")
        .into_iter()
        .filter(|(source, _, _)| source.starts_with("/dev/"))
        .collect();
    let mut mounts: Vec<Mount> = mounted
        .iter()
        .map(|(source, target, fstype)| Mount {
            target: target.clone(),
            source: source.clone(),
            fstype: fstype.clone(),
            mounted: true,
            users: Vec::new(),
        })
        .collect();
    mounts.extend(
        table("/etc/fstab")
            .into_iter()
            .filter(|(_, target, _)| !mounted.iter().any(|(_, t, _)| t == target))
            .map(|(source, target, fstype)| Mount {
                target,
                source,
                fstype,
                mounted: false,
                users: Vec::new(),
            }),
    );
    // each path a process uses belongs to the deepest mount that holds it; the root file system holds every process
    let targets: Vec<PathBuf> = mounts
        .iter()
        .filter(|m| m.mounted && m.target != Path::new("/"))
        .map(|m| m.target.clone())
        .collect();
    for (user, paths) in processes(root) {
        let mut on: Vec<&PathBuf> = paths
            .iter()
            .filter_map(|p| {
                targets
                    .iter()
                    .filter(|t| p.starts_with(t))
                    .max_by_key(|t| t.components().count())
            })
            .collect();
        on.sort();
        on.dedup();
        for target in on {
            if let Some(m) = mounts.iter_mut().find(|m| m.mounted && m.target == *target) {
                m.users.push(user.clone());
            }
        }
    }
    mounts
}

/// `<pid> <program>` of each process with an open file that starts with `prefix` (`/dev/nvidia`).
#[must_use]
pub fn users_of(root: &SysRoot, prefix: &str) -> Vec<String> {
    processes(root)
        .into_iter()
        .filter(|(_, paths)| {
            paths
                .iter()
                .any(|p| p.to_string_lossy().starts_with(prefix))
        })
        .map(|(user, _)| user)
        .collect()
}

/// Each process (`<pid> <program>`) with its working folder and its open files. A process of another user, whose
/// links this process may not read, is not there.
fn processes(root: &SysRoot) -> Vec<(String, Vec<PathBuf>)> {
    root.children("/proc", "")
        .into_iter()
        .filter_map(|dir| {
            let pid = dir.file_name()?.to_str()?.to_string();
            if !pid.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let proc = Path::new("/proc").join(&pid);
            let mut paths: Vec<PathBuf> = root.link(proc.join("cwd")).into_iter().collect();
            paths.extend(
                root.children(proc.join("fd"), "")
                    .iter()
                    .filter_map(|fd| root.link(fd)),
            );
            let program = root.read(proc.join("comm")).unwrap_or_default();
            (!paths.is_empty()).then(|| (format!("{pid} {program}"), paths))
        })
        .collect()
}
