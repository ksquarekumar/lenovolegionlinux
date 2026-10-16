//! Machine paths (`/sys/...`, `/proc/...`, `/run/...`) resolved under a root: `/` on the machine, a recorded tree in
//! tests and replays. Callers always use machine paths; only this module knows the root.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

/// The content of the file `path` (not under a root: a user's own file); `None` when it is not there.
///
/// # Errors
/// The file is there but cannot be read.
pub fn read_file(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if missing(&e) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// A path that is not there: not found, or a part of it is a file (`<file>/name`).
fn missing(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// A read that failed because the reader may not see the path (another user's process in `/proc`).
#[must_use]
pub fn denied(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == ErrorKind::PermissionDenied)
}

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

    /// Whether `machine_path` exists. Only "not found" is false: any other IO error (permission denied, IO
    /// error) is an error, never a missing path.
    ///
    /// # Errors
    /// The path cannot be checked.
    pub fn exists(&self, machine_path: impl AsRef<Path>) -> Result<bool> {
        let machine_path = machine_path.as_ref();
        let path = self.path(machine_path);
        let exists = match path.try_exists() {
            Err(e) if missing(&e) => false,
            there => there.with_context(|| format!("checking {}", machine_path.display()))?,
        };
        if exists && self.trace.is_some() {
            if path.is_dir() {
                self.note(machine_path, Seen::Dir);
            } else {
                // only that it exists counts (a kernel image): an empty file stands in for it
                self.note(machine_path, Seen::File(String::new()));
            }
        }
        Ok(exists)
    }

    /// Trimmed file content, like reading a sysfs attribute; None when the file is not there.
    ///
    /// # Errors
    /// The file is there but cannot be read (permission denied, IO error, not UTF-8).
    pub fn read(&self, machine_path: impl AsRef<Path>) -> Result<Option<String>> {
        let machine_path = machine_path.as_ref();
        let content = match std::fs::read_to_string(self.path(machine_path)) {
            Ok(content) => content,
            Err(e) if missing(&e) => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", machine_path.display())),
        };
        let trimmed = content.trim().to_string();
        self.note(machine_path, Seen::File(content));
        Ok(Some(trimmed))
    }

    /// Where a symlink points (a device's `driver`); None when the link is not there.
    ///
    /// # Errors
    /// The path is there but cannot be read as a link.
    pub fn link(&self, machine_path: impl AsRef<Path>) -> Result<Option<PathBuf>> {
        let machine_path = machine_path.as_ref();
        let target = match std::fs::read_link(self.path(machine_path)) {
            Ok(target) => target,
            // not a link (a folder, a file): it points nowhere
            Err(e) if missing(&e) || e.kind() == ErrorKind::InvalidInput => return Ok(None),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("reading the link {}", machine_path.display()));
            }
        };
        self.note(machine_path, Seen::Link(target.clone()));
        Ok(Some(target))
    }

    /// Write what the trace saw under `to`, as a tree that a [`SysRoot`] at `to` reads back the same; returns the
    /// number of paths.
    ///
    /// # Errors
    /// A file under `to` cannot be written.
    pub fn record_into(&self, to: &Path) -> Result<usize> {
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

    /// Sorted machine paths of the entries in `dir` whose names start with `prefix`; empty when `dir` is not there.
    ///
    /// # Errors
    /// `dir` is there but cannot be listed.
    pub fn children(&self, dir: impl AsRef<Path>, prefix: &str) -> Result<Vec<PathBuf>> {
        let dir = dir.as_ref();
        let entries = match std::fs::read_dir(self.path(dir)) {
            Ok(entries) => entries,
            Err(e) if missing(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
        };
        self.note(dir, Seen::Dir);
        let mut out = Vec::new();
        for entry in entries {
            let name = entry
                .with_context(|| format!("listing {}", dir.display()))?
                .file_name()
                .to_string_lossy()
                .into_owned();
            if name.starts_with(prefix) {
                out.push(dir.join(name));
            }
        }
        out.sort();
        Ok(out)
    }
}
