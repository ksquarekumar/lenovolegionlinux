//! The one way a test makes a temporary folder: a new, empty folder that goes when its owner is dropped, also when the
//! test panics. Thousands of folders left by earlier test runs used all inodes of /tmp.

use std::path::{Path, PathBuf};

/// A folder at `path` that is removed on drop. A failed test keeps it when `LEGIONCTL_TEST_KEEP=1` is set, and tells
/// where it is.
pub struct Scratch(PathBuf);

impl Scratch {
    /// Removes what an earlier run left at `path` and makes it again, empty.
    pub fn new(path: impl Into<PathBuf>) -> Scratch {
        let path = path.into();
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let failed = std::thread::panicking();
        if failed && std::env::var_os("LEGIONCTL_TEST_KEEP").is_some_and(|v| v == "1") {
            eprintln!("LEGIONCTL_TEST_KEEP=1: kept {}", self.0.display());
            return;
        }
        match std::fs::remove_dir_all(&self.0) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound && !failed => {
                panic!("{} was not removed: {e}", self.0.display());
            }
            _ => {}
        }
    }
}
