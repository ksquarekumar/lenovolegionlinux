//! The one git runner: every git call of legionctl is `git -C <dir> ...` through this module. A git that does not
//! start is an error; an exit code other than the ones a caller expects is an error with git's message.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail, ensure};

/// `git -C <dir>`, for a caller that streams (archive, patch-id) or sets an environment of its own.
#[must_use]
pub fn command(dir: &Path) -> Command {
    let mut git = Command::new("git");
    git.arg("-C").arg(dir);
    git
}

/// The output of `cmd` whatever its exit code.
///
/// # Errors
/// git does not start.
pub fn output(cmd: &mut Command) -> Result<Output> {
    cmd.output().context("running git")
}

/// The stdout of `cmd`, which must exit 0.
///
/// # Errors
/// git does not start or fails: its message.
pub fn run(cmd: &mut Command) -> Result<Vec<u8>> {
    let out = output(cmd)?;
    ensure!(
        out.status.success(),
        "git {}: {}",
        cmd.get_args()
            .skip(2)
            .map(OsStr::to_string_lossy)
            .collect::<Vec<_>>()
            .join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out.stdout)
}

/// The stdout of `git -C <dir> <args>`, which must exit 0.
///
/// # Errors
/// As [`run`].
pub fn bytes<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<Vec<u8>> {
    run(command(dir).args(args))
}

/// The trimmed stdout of `git -C <dir> <args>`, which must exit 0.
///
/// # Errors
/// As [`run`].
pub fn text<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<String> {
    Ok(String::from_utf8_lossy(&bytes(dir, args)?)
        .trim()
        .to_string())
}

/// The trimmed stdout when git exits 0, `None` when it exits 1 (no such ref, no such config value, not an
/// ancestor).
///
/// # Errors
/// git does not start, or exits with another code: its message.
pub fn found<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<Option<String>> {
    let out = output(command(dir).args(args))?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => bail!(
            "git {}: {}",
            args.iter()
                .map(|a| a.as_ref().to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}
