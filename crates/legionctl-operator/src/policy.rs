//! What legionctl does by itself with sessions (S4, decided 2026-10-13; off by default): an interactive session whose
//! agent ended a reply and got no prompt for `session.policy.idle_freeze_minutes` is frozen; the user hears it and
//! thaws it with `legionctl session unpause <id>`. The operator's audit of the hook calls is the record of activity.

use std::time::Duration;

use anyhow::Result;
use legionctl_core::config::{Dirs, resolve};
use legionctl_core::db::{Db, EventFilter};
use legionctl_core::dossier::Dossiers;
use legionctl_core::session::scope_state;

use crate::Ready;

const CHECK: Duration = Duration::from_secs(10);

pub async fn run(ready: Ready) -> Result<()> {
    let desktop = zbus::Connection::session().await.ok();
    ready.set();
    let mut tick = tokio::time::interval(CHECK);
    loop {
        tick.tick().await;
        let frozen = tokio::task::spawn_blocking(freeze_idle).await?;
        match (frozen, &desktop) {
            (Ok(sessions), Some(bus)) => {
                for (id, minutes) in sessions {
                    let short = &id[..id.len().min(8)];
                    let body = format!(
                        "No prompt for {minutes} min after its last reply. Thaw it with: legionctl session unpause {id}"
                    );
                    let summary = format!("legionctl: session {short} frozen");
                    if let Err(e) =
                        crate::desktop::tell(bus, "media-playback-pause", &summary, &body).await
                    {
                        eprintln!("operator: {summary}; not shown: {e:#}");
                    }
                }
            }
            (Ok(_), None) => {}
            (Err(e), _) => eprintln!("operator: the session policy failed: {e:#}"),
        }
    }
}

/// Freeze each running session that is idle for the configured time; the ids frozen, with the minutes.
fn freeze_idle() -> Result<Vec<(String, u32)>> {
    let resolved = resolve(&Dirs::from_env(), None)?;
    let minutes = resolved.config.session.policy.idle_freeze_minutes;
    if minutes == 0 {
        return Ok(Vec::new());
    }
    let db = Db::open_default()?;
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?;
    let mut frozen = Vec::new();
    for session in Dossiers::open(&resolved.config.dossier)?.sessions()? {
        if scope_state(&session.id) != "active" {
            continue;
        }
        let filter = EventFilter {
            kinds: &["internal.hooks.run"],
            field: Some(("$.input.session", &session.id)),
            naming: None,
        };
        let Some(last) = db.last_event(&filter)? else {
            continue;
        };
        if last.payload["input"]["event"] != "stop" || now - last.ts < i64::from(minutes) * 60 {
            continue;
        }
        // once per idle stretch: a freeze or an unpause after that reply ends it
        let after = |kind: &str, path: &str| -> Result<bool> {
            let filter = EventFilter {
                kinds: &[kind],
                field: Some((path, &session.id)),
                naming: None,
            };
            Ok(db.last_event(&filter)?.is_some_and(|e| e.id > last.id))
        };
        if after("session.freeze", "$.session")? || after("session.unpause", "$.input.id")? {
            continue;
        }
        let unit = format!("legionctl-session-{}.scope", session.id);
        let out = std::process::Command::new("systemctl")
            .args(["--user", "freeze", &unit])
            .output()?;
        if out.status.success() {
            db.record(
                "legionctl",
                "session.freeze",
                &serde_json::json!({"session": session.id, "idle_minutes": minutes}),
            )?;
            frozen.push((session.id, minutes));
        } else {
            eprintln!(
                "operator: {unit} not frozen: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    Ok(frozen)
}
