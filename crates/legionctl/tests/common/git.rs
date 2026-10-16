//! git for the tests that make their own repositories (e2e.rs, checks.rs).

use std::path::Path;
use std::process::Command;

/// git with the test's own HOME (its identity and default branch), in `dir`.
pub fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
