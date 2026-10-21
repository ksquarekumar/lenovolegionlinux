//! The one way a test makes a temporary folder: a new, empty folder that goes when its owner is dropped, also when the
//! test panics. Thousands of folders left by earlier test runs used all inodes of /tmp.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::time::Duration;

/// A folder at `path` that is removed on drop. A failed test keeps it when `LEGIONCTL_TEST_KEEP=1` is set, and tells
/// where it is. While it lives, its test has a time limit (the second field tells the watch that it ended).
pub struct Scratch(PathBuf, Sender<()>);

/// How long a test may hold a scratch folder: 15 minutes, or `LEGIONCTL_TEST_MINUTES` (the VM tests of CI build disks).
fn time_limit() -> Duration {
    let minutes = std::env::var("LEGIONCTL_TEST_MINUTES")
        .ok()
        .map_or(15, |m| {
            m.parse()
                .expect("LEGIONCTL_TEST_MINUTES is a number of minutes")
        });
    Duration::from_secs(minutes * 60)
}

impl Scratch {
    /// Removes what an earlier run left at `path` and makes it again, empty. A test that still holds it after the time
    /// limit stops the test run, with its name: a wait that never ends fails in minutes (CI run 39631433941 waited
    /// 62 min in one test).
    pub fn new(path: impl Into<PathBuf>) -> Scratch {
        let path = path.into();
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        let (alive, watch) = std::sync::mpsc::channel::<()>();
        let test = std::thread::current()
            .name()
            .unwrap_or("a test")
            .to_string();
        let (limit, folder) = (time_limit(), path.clone());
        std::thread::spawn(move || {
            if watch.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
                // straight to stderr: the test harness keeps print output of a test thread and its threads
                let _ = std::io::Write::write_all(
                    &mut std::io::stderr(),
                    format!(
                        "{test} still runs after {} min (its folder {}): the test run stops\n",
                        limit.as_secs() / 60,
                        folder.display()
                    )
                    .as_bytes(),
                );
                std::process::exit(124);
            }
        });
        Scratch(path, alive)
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
        // the watch ends (it is gone only when the time limit stopped the run)
        let _ = self.1.send(());
        let failed = std::thread::panicking();
        if failed && std::env::var_os("LEGIONCTL_TEST_KEEP").is_some_and(|v| v == "1") {
            eprintln!("LEGIONCTL_TEST_KEEP=1: kept {}", self.0.display());
            return;
        }
        open_folders(&self.0);
        match std::fs::remove_dir_all(&self.0) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound && !failed => {
                panic!("{} was not removed: {e}", self.0.display());
            }
            _ => {}
        }
    }
}

/// Make every folder under `dir` writable by its owner: an overlay's work folder (`work/work`, a session's layers over
/// the home) has mode 000.
fn open_folders(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            let path = entry.path();
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            open_folders(&path);
        }
    }
}
