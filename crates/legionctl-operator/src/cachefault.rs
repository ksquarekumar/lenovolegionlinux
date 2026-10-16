//! A cache that cannot be read or written (user decisions Q2 and Q3, todo-142): the call goes upstream as a miss, and
//! the error is reported: an event (`cache.error` with the cache and the reason, once per new reason) and an open
//! doctor finding until that cache works again.

use std::path::PathBuf;

use anyhow::Result;
use legionctl_core::db::Db;
use serde_json::json;

/// The marker of an open finding of the cache `ns` (`gateway`, `tools`): the reason.
fn marker(ns: &str) -> PathBuf {
    legionctl_core::db::default_path().with_file_name(format!("cache-{ns}.error"))
}

/// The cache `ns` failed: report it; the caller goes on as with a miss.
pub fn failed(ns: &str, e: &anyhow::Error) {
    let reason = format!("{e:#}");
    eprintln!("operator: the {ns} cache failed, the call goes upstream: {reason}");
    let path = marker(ns);
    if std::fs::read_to_string(&path).is_ok_and(|old| old == reason) {
        return;
    }
    if let Err(e) = std::fs::write(&path, &reason) {
        eprintln!("operator: cannot write {}: {e}", path.display());
    }
    let event = json!({"cache": ns, "error": reason});
    if let Err(e) =
        Db::open_default().and_then(|db| db.record("legionctl-operator", "cache.error", &event))
    {
        eprintln!("operator: cannot record cache.error: {e:#}");
    }
}

/// The cache `ns` works: its finding closes.
pub fn works(ns: &str) {
    let path = marker(ns);
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("operator: cannot remove {}: {e}", path.display());
    }
}

/// doctor's findings: each cache that failed and has not worked since.
///
/// # Errors
/// A marker is there but cannot be read.
pub fn findings() -> Result<Vec<String>> {
    let mut out = Vec::new();
    for ns in ["gateway", "tools"] {
        if let Some(reason) = legionctl_machine::sysroot::read_file(&marker(ns))? {
            out.push(format!(
                "the {ns} cache fails, so its calls go upstream: {reason}"
            ));
        }
    }
    Ok(out)
}
