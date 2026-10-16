//! The operator's own failures that no caller sees (user decision Q1, todo-142): a background step that failed (log
//! retention, the dossier registry, resumed jobs) and a cache that cannot be used (user decisions Q2 and Q3: its call
//! goes upstream as a miss). Each is a journal line (stderr), one `operator.fault` event per new reason, and an open
//! doctor finding until the same step works again.

use std::path::PathBuf;

use anyhow::Result;
use legionctl_core::db::Db;
use serde_json::json;

/// The folder of the open findings: one file per fault, named by its key, holding the finding.
fn folder() -> PathBuf {
    legionctl_core::db::default_path().with_file_name("faults")
}

/// The step `key` failed with `e`; `what` says what that means (the finding's text before the reason).
pub fn failed(key: &str, what: &str, e: &anyhow::Error) {
    let finding = format!("{what}: {e:#}");
    let path = folder().join(key);
    // a retry with the same reason adds nothing (a loop retries often)
    if std::fs::read_to_string(&path).is_ok_and(|old| old == finding) {
        return;
    }
    eprintln!("operator: {finding}");
    if let Err(e) = std::fs::create_dir_all(folder()).and_then(|()| std::fs::write(&path, &finding))
    {
        eprintln!("operator: cannot write {}: {e}", path.display());
    }
    let event = json!({"fault": key, "finding": finding});
    if let Err(e) =
        Db::open_default().and_then(|db| db.record("legionctl-operator", "operator.fault", &event))
    {
        eprintln!("operator: cannot record operator.fault: {e:#}");
    }
}

/// The step `key` works: its finding closes.
pub fn works(key: &str) {
    let path = folder().join(key);
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("operator: cannot remove {}: {e}", path.display());
    }
}

/// doctor's findings: each fault that is open.
///
/// # Errors
/// The folder or a finding is there but cannot be read.
pub fn findings() -> Result<Vec<String>> {
    let dir = folder();
    let mut files = match std::fs::read_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?,
    };
    files.sort();
    files
        .iter()
        .map(|f| Ok(std::fs::read_to_string(f)?))
        .collect()
}
