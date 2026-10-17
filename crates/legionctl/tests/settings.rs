//! The settings registry (DESIGN-20): every config key is a setting with user help, its value in use and the layer
//! that set it, through the real operator.

use std::ffi::OsString;

mod common;
#[path = "common/plain.rs"]
mod plain;

use common::Operator;
use common::scratch::Scratch;
use serde_json::Value;

/// Every setting has plain user help (and the help file names only settings that exist); a value the user set shows
/// with its source, the rest come from the defaults.
#[test]
fn every_setting_has_user_help_and_shows_where_its_value_comes_from() {
    let home =
        Scratch::new(std::env::temp_dir().join(format!("lc-settings-{}", std::process::id())));
    std::fs::create_dir_all(home.join(".config/legionctl")).unwrap();
    std::fs::write(
        home.join(".config/legionctl/mine.jsonc"),
        r#"{"cache": {"tools": {"max_bytes": 7}}}"#,
    )
    .unwrap();
    let op = Operator::start("settings", &[("HOME", OsString::from(home.as_os_str()))]);
    let view: Value = serde_json::from_str(&op.ok(&["config", "settings", "--json"])).unwrap();
    let items = view["items"].as_array().unwrap();
    assert!(items.len() > 50, "{}", items.len());
    let missing: Vec<&str> = items
        .iter()
        .filter(|s| s["help"].as_str().is_none_or(str::is_empty))
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert!(
        missing.is_empty(),
        "settings with no user help ({}):\n{}",
        missing.len(),
        missing.join("\n")
    );
    for s in items {
        plain::assert_plain(s["key"].as_str().unwrap(), s["help"].as_str().unwrap());
    }
    let keys: Vec<&str> = items.iter().map(|s| s["key"].as_str().unwrap()).collect();
    let help = legionctl_core::settings::help().unwrap();
    let stale: Vec<&String> = help
        .keys()
        .filter(|k| !keys.contains(&k.as_str()))
        .collect();
    assert!(
        stale.is_empty(),
        "help for settings that do not exist: {stale:?}"
    );
    let cache = items.iter().find(|s| s["key"] == "cache").unwrap();
    assert_eq!(cache["from"], "user", "{cache}");
    assert!(items.iter().any(|s| s["from"] == "defaults"));
}
