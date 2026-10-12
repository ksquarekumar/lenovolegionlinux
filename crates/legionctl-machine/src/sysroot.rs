//! Machine paths (`/sys/...`, `/proc/...`, `/run/...`) resolved under a root: `/` on the machine, a recorded tree in
//! tests and replays. Callers always use machine paths; only this module knows the root.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SysRoot {
    root: PathBuf,
}

impl SysRoot {
    pub fn new(root: impl Into<PathBuf>) -> SysRoot {
        SysRoot { root: root.into() }
    }

    /// `LEGIONCTL_SYSROOT`, or `/`.
    pub fn from_env() -> SysRoot {
        SysRoot::new(std::env::var_os("LEGIONCTL_SYSROOT").unwrap_or_else(|| "/".into()))
    }

    pub fn path(&self, machine_path: impl AsRef<Path>) -> PathBuf {
        let p = machine_path.as_ref();
        self.root.join(p.strip_prefix("/").unwrap_or(p))
    }

    pub fn exists(&self, machine_path: impl AsRef<Path>) -> bool {
        self.path(machine_path).exists()
    }

    /// Trimmed file content, like reading a sysfs attribute.
    pub fn read(&self, machine_path: impl AsRef<Path>) -> Option<String> {
        std::fs::read_to_string(self.path(machine_path))
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Sorted machine paths of the entries in `dir` whose names start with `prefix`.
    pub fn children(&self, dir: impl AsRef<Path>, prefix: &str) -> Vec<PathBuf> {
        let dir = dir.as_ref();
        let mut out: Vec<PathBuf> = std::fs::read_dir(self.path(dir))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(prefix))
            .map(|name| dir.join(name))
            .collect();
        out.sort();
        out
    }
}
