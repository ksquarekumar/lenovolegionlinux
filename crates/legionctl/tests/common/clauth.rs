//! A stand-in clauth for the tests (the user's own never runs: `Operator::start` leaves it out of PATH). It answers
//! from files in the test's home: `clauth-status.json` (`status --json`), `clauth-which` (`which`; none: unknown),
//! `legionctl-clauth.service` (there: the daemon runs; a stand-in systemctl makes it), `clauth-sessions.json` (`sessions --json`), `clauth-version`;
//! `clauth-refuse.<command>.<arg>` makes that call fail with the file's text. Each login, switch and capture is a line
//! of `clauth.log` with what it read on stdin.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Put the stand-in at `home/.cargo/bin/clauth` (where `cargo install` puts clauth); its path.
pub fn install(home: &Path) -> PathBuf {
    let bin = home.join(".cargo/bin/clauth");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, SCRIPT).unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

/// The stand-in's script.
pub const SCRIPT: &str = r#"#!/bin/sh
refuse="$HOME/clauth-refuse.$1.$2"
[ -f "$refuse" ] && { cat "$refuse" >&2; exit 1; }
case "$1" in
  --version) echo "clauth $(cat "$HOME/clauth-version" 2>/dev/null || echo 0.17.0)";;
  which) cat "$HOME/clauth-which" 2>/dev/null || echo unknown;;
  status) cat "$HOME/clauth-status.json";;
  daemon) [ -e "$HOME/legionctl-clauth.service" ] || exit 1; echo "daemon runs";;
  sessions) cat "$HOME/clauth-sessions.json" 2>/dev/null || echo '[]';;
  *) echo "$* stdin=$(cat)" >> "$HOME/clauth.log";;
esac
"#;

/// One profile of clauth's status feed (schema 2): `windows` as (label, percent), `chain` as (position, threshold).
pub fn profile(
    name: &str,
    harness: &str,
    active: bool,
    windows: &[(&str, f64)],
    chain: Option<(usize, f64)>,
) -> Value {
    json!({
        "name": name, "active": active, "rolling_token": false, "provider": "anthropic", "base_url": null,
        "tier": "Max", "harness": harness, "has_live_session": false, "auth_status": "ok", "fetch_status": null,
        "stale": false, "fetched_at": if windows.is_empty() { Value::Null } else { json!(iso_now()) },
        "next_refresh_at": null, "auto_start": false, "auto_start_queue": null, "bell_threshold": null,
        "fallback": chain.map(|(position, threshold)| json!({"position": position, "threshold": threshold, "armed": active})),
        "windows": windows.iter().map(|(label, pct)| json!({"label": label, "utilization_pct": pct, "resets_at": null})).collect::<Vec<_>>(),
        "third_party": null
    })
}

/// Write clauth's status feed with `profiles`.
pub fn status(home: &Path, profiles: &[Value]) {
    std::fs::write(
        home.join("clauth-status.json"),
        json!({"schema": 2, "generated_at": iso_now(), "active_profile": null, "pending_switch": null,
               "wrap_off": false, "active_codex_profile": null, "codex_fallback_chain": [], "codex_wrap_off": false,
               "refresh_interval_ms": 90000, "clauth_version": "0.17.0", "gateway": null, "proxies": [],
               "profiles": profiles})
        .to_string(),
    )
    .unwrap();
}

fn iso_now() -> String {
    let out = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%S+00:00"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}
