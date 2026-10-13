//! Machine paths (`/sys/...`, `/proc/...`, `/run/...`) resolved under a root: `/` on the machine, a recorded tree in
//! tests and replays. Callers always use machine paths; only this module knows the root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What a traced read found at a machine path (`legionctl record` copies it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    File(String),
    Dir,
    Link(PathBuf),
}

#[derive(Debug, Clone)]
pub struct SysRoot {
    root: PathBuf,
    trace: Option<Arc<Mutex<BTreeMap<PathBuf, Seen>>>>,
}

impl SysRoot {
    pub fn new(root: impl Into<PathBuf>) -> SysRoot {
        SysRoot {
            root: root.into(),
            trace: None,
        }
    }

    /// The same root, noting every path that a read finds; [`SysRoot::seen`] returns them.
    #[must_use]
    pub fn traced(self) -> SysRoot {
        SysRoot {
            trace: Some(Arc::default()),
            ..self
        }
    }

    /// The paths that reads found since [`SysRoot::traced`].
    #[must_use]
    pub fn seen(&self) -> BTreeMap<PathBuf, Seen> {
        self.trace
            .as_ref()
            .map(|t| {
                t.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .unwrap_or_default()
    }

    /// A read's content wins over an existence check of the same path.
    fn note(&self, machine_path: &Path, seen: Seen) {
        if let Some(trace) = &self.trace {
            let mut trace = trace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = trace.entry(machine_path.to_path_buf());
            if seen == Seen::File(String::new()) {
                entry.or_insert(seen);
            } else {
                *entry.or_insert(Seen::Dir) = seen;
            }
        }
    }

    /// `LEGIONCTL_SYSROOT`, or `/`.
    #[must_use]
    pub fn from_env() -> SysRoot {
        SysRoot::new(std::env::var_os("LEGIONCTL_SYSROOT").unwrap_or_else(|| "/".into()))
    }

    pub fn path(&self, machine_path: impl AsRef<Path>) -> PathBuf {
        let p = machine_path.as_ref();
        self.root.join(p.strip_prefix("/").unwrap_or(p))
    }

    pub fn exists(&self, machine_path: impl AsRef<Path>) -> bool {
        let machine_path = machine_path.as_ref();
        let path = self.path(machine_path);
        if self.trace.is_some() {
            if path.is_dir() {
                self.note(machine_path, Seen::Dir);
            } else if path.is_file() {
                // only that it exists counts (a kernel image): an empty file stands in for it
                self.note(machine_path, Seen::File(String::new()));
            }
        }
        path.exists()
    }

    /// Trimmed file content, like reading a sysfs attribute.
    pub fn read(&self, machine_path: impl AsRef<Path>) -> Option<String> {
        let machine_path = machine_path.as_ref();
        let content = std::fs::read_to_string(self.path(machine_path)).ok()?;
        let trimmed = content.trim().to_string();
        self.note(machine_path, Seen::File(content));
        Some(trimmed)
    }

    /// Where a symlink points (a device's `driver`).
    pub fn link(&self, machine_path: impl AsRef<Path>) -> Option<PathBuf> {
        let machine_path = machine_path.as_ref();
        let target = std::fs::read_link(self.path(machine_path)).ok()?;
        self.note(machine_path, Seen::Link(target.clone()));
        Some(target)
    }

    /// Write what the trace saw under `to`, as a tree that a [`SysRoot`] at `to` reads back the same; returns the
    /// number of paths.
    ///
    /// # Errors
    /// A file under `to` cannot be written.
    pub fn record_into(&self, to: &Path) -> anyhow::Result<usize> {
        use anyhow::Context;
        let seen = self.seen();
        for (machine_path, what) in &seen {
            let target = SysRoot::new(to).path(machine_path);
            let parent = target.parent().unwrap_or(to);
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
            match what {
                Seen::Dir => std::fs::create_dir_all(&target),
                Seen::File(content) => std::fs::write(&target, content),
                Seen::Link(link) => std::os::unix::fs::symlink(link, &target),
            }
            .with_context(|| format!("recording {}", target.display()))?;
        }
        Ok(seen.len())
    }

    /// Sorted machine paths of the entries in `dir` whose names start with `prefix`.
    pub fn children(&self, dir: impl AsRef<Path>, prefix: &str) -> Vec<PathBuf> {
        let dir = dir.as_ref();
        self.note(dir, Seen::Dir);
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
