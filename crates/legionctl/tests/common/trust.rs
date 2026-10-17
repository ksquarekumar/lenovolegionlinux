//! The user's answer to a repository file (trust on first use), for the tests whose operator has no desktop to ask.

use std::path::Path;

/// Store the user's Allow for the repository file `file` as it is now in the approvals of `home`, as the desktop
/// question stores it: a changed file waits for an answer again.
pub fn allow(home: &Path, file: &Path) {
    allow_value(
        home,
        file,
        &legionctl_machine::profile::read_jsonc(file).unwrap(),
    );
}

/// The same for a file whose content legionctl reads as `value` (a compose file, as `docker compose config` gives it).
pub fn allow_value(home: &Path, file: &Path, value: &serde_json::Value) {
    let (name, hash, _) = legionctl_core::config::Approvals::default().of(file, value);
    let path = home.join(".local/state/legionctl/approvals.json");
    let mut all: serde_json::Value = std::fs::read(&path).map_or_else(
        |_| serde_json::json!({}),
        |b| serde_json::from_slice(&b).unwrap(),
    );
    all[name] = serde_json::json!([hash, true]);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, all.to_string()).unwrap();
}
