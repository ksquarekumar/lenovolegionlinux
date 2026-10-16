//! Mounts and what uses them (todo-85, note 22): each mounted block device and each `/etc/fstab` entry, and the
//! processes whose working folder or an open file is on it, as `fuser -m` finds them.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::sysroot::{SysRoot, denied};

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

/// The mounted block devices (sources in `/dev`), then the other fstab entries, mounted when the mount table has their
/// target; swap is not a mount. With the number of processes whose files this reader may not see.
///
/// # Errors
/// The mount table, fstab or a process's files cannot be read.
pub fn list(root: &SysRoot) -> Result<(Vec<Mount>, usize)> {
    let table = |file: &str| -> Result<Vec<(String, PathBuf, String)>> {
        Ok(root
            .read(file)?
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
            .collect())
    };
    // an automount point is no mount: its file system mounts at the first access
    let table_now: Vec<(String, PathBuf, String)> = table("/proc/self/mounts")?
        .into_iter()
        .filter(|(_, _, fstype)| fstype != "autofs")
        .collect();
    let mounted: Vec<(String, PathBuf, String)> = table_now
        .iter()
        .filter(|(source, _, _)| source.starts_with("/dev/"))
        .cloned()
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
        table("/etc/fstab")?
            .into_iter()
            .filter(|(_, target, _)| !mounted.iter().any(|(_, t, _)| t == target))
            .map(|(source, target, fstype)| Mount {
                // a file system without a device (tmpfs, a network share) is mounted when the mount table has it
                mounted: table_now.iter().any(|(_, t, _)| *t == target),
                target,
                source,
                fstype,
                users: Vec::new(),
            }),
    );
    // each path a process uses belongs to the deepest mount that holds it; the root file system holds every process
    let targets: Vec<PathBuf> = mounts
        .iter()
        .filter(|m| m.mounted && m.target != Path::new("/"))
        .map(|m| m.target.clone())
        .collect();
    let processes = processes(root)?;
    for p in &processes.visible {
        let user = format!("{} {}", p.pid, p.program);
        let mut on: Vec<&PathBuf> = p
            .paths
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
    Ok((mounts, processes.not_visible))
}

/// `<pid> <program>` of each process with an open file that starts with `prefix` (`/dev/nvidia`), and the number of
/// processes whose files this reader may not see.
///
/// # Errors
/// A process's files cannot be read.
pub fn users_of(root: &SysRoot, prefix: &str) -> Result<(Vec<String>, usize)> {
    let processes = processes(root)?;
    let users = processes
        .visible
        .into_iter()
        .filter(|p| {
            p.paths
                .iter()
                .any(|f| f.to_string_lossy().starts_with(prefix))
        })
        .map(|p| format!("{} {}", p.pid, p.program))
        .collect();
    Ok((users, processes.not_visible))
}

/// A process with files open: its pid, its program, its working folder and its open files.
pub struct Process {
    pub pid: String,
    pub program: String,
    pub paths: Vec<PathBuf>,
}

/// The processes that this reader may see, and how many it may not: another user's processes, and the reader's own
/// that are not dumpable (`systemd --user`), deny their working folder and open files.
pub struct Processes {
    pub visible: Vec<Process>,
    pub not_visible: usize,
}

/// Each process with its working folder and its open files, as `fuser -m` finds them.
///
/// # Errors
/// `/proc` or a process's files cannot be read for another reason than a denied read.
pub fn processes(root: &SysRoot) -> Result<Processes> {
    let mut out = Processes {
        visible: Vec::new(),
        not_visible: 0,
    };
    for dir in root.children("/proc", "")? {
        let Some(pid) = dir.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let proc = Path::new("/proc").join(&pid);
        let files = || -> Result<Vec<PathBuf>> {
            let mut paths: Vec<PathBuf> = root.link(proc.join("cwd"))?.into_iter().collect();
            for fd in root.children(proc.join("fd"), "")? {
                paths.extend(root.link(fd)?);
            }
            Ok(paths)
        };
        let paths = match files() {
            Err(e) if denied(&e) => {
                out.not_visible += 1;
                continue;
            }
            paths => paths?,
        };
        if !paths.is_empty() {
            // a process that ended since the listing has no comm
            let program = root.read(proc.join("comm"))?.unwrap_or_default();
            out.visible.push(Process {
                pid,
                program,
                paths,
            });
        }
    }
    Ok(out)
}
