//! What legionctl does by itself with sessions (S4, decided 2026-10-13; off by default): an interactive session whose
//! agent ended a reply and got no prompt for `session.policy.idle_freeze_minutes` is frozen; the user hears it and
//! thaws it with `legionctl session unpause <id>`. The operator's audit of the hook calls is the record of activity.
//! With `session.policy.offload_headless`, headless runs stop while memory is under pressure (the `load.busy` limits)
//! and go on in their sessions when it eases.

use std::time::Duration;

use anyhow::Result;
use legionctl_core::config::{Dirs, resolve};
use legionctl_core::db::{Db, EventFilter};
use legionctl_core::dossier::Dossiers;
use legionctl_core::session::{Control, control, state};

use crate::Ready;

const CHECK: Duration = Duration::from_secs(10);

pub async fn run(ready: Ready, operator: std::sync::Arc<crate::engine::Operator>) -> Result<()> {
    let desktop = zbus::Connection::session().await.ok();
    ready.set();
    let mut tick = tokio::time::interval(CHECK);
    loop {
        tick.tick().await;
        let engine = operator.clone();
        let offloads = tokio::task::spawn_blocking(move || offload(&engine)).await?;
        match (offloads, &desktop) {
            (Ok(notices), Some(bus)) => {
                for (summary, body) in notices {
                    if let Err(e) =
                        crate::desktop::tell(bus, "dialog-information", &summary, &body).await
                    {
                        eprintln!("operator: {summary}; not shown: {e:#}");
                    }
                }
            }
            (Ok(_), None) => {}
            (Err(e), _) => eprintln!("operator: the offload policy failed: {e:#}"),
        }
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
        match state(&session.id, session.inside) {
            Ok(s) if s == "active" => {}
            Ok(_) => continue,
            Err(e) => {
                // this session is not frozen; the others still are
                eprintln!("operator: idle freeze: {e:#}");
                continue;
            }
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
        match control(&session.id, session.inside, Control::Pause) {
            Ok(()) => {
                db.record(
                    "legionctl",
                    "session.freeze",
                    &serde_json::json!({"session": session.id, "idle_minutes": minutes}),
                )?;
                frozen.push((session.id, minutes));
            }
            Err(e) => eprintln!("operator: session {} not frozen: {e:#}", session.id),
        }
    }
    Ok(frozen)
}

/// Memory under pressure by the `load.busy` limits: stalls ("some") at or above them, or less available memory.
fn memory_pressed(
    load: &legionctl_machine::load::Load,
    busy: &legionctl_machine::load::Limits,
) -> bool {
    let stalls = load
        .memory
        .as_ref()
        .is_some_and(|p| p.some_avg10 >= busy.memory_pressure);
    // whole MiB keep the ratio exact in an f64
    let mib = |bytes: u64| f64::from(u32::try_from(bytes >> 20).unwrap_or(u32::MAX));
    let available = mib(load.mem_available_bytes) * 100.0 / mib(load.mem_total_bytes).max(1.0);
    stalls || (load.mem_total_bytes > 0 && available < busy.memory_available_percent)
}

/// Offload the headless runs while memory is under pressure, and go on with them when it eases; what to tell the user.
fn offload(operator: &std::sync::Arc<crate::engine::Operator>) -> Result<Vec<(String, String)>> {
    let resolved = resolve(&Dirs::from_env(), None)?;
    if !resolved.config.session.policy.offload_headless {
        return Ok(Vec::new());
    }
    let load = legionctl_machine::load::read_load(
        &legionctl_machine::sysroot::SysRoot::from_env(),
        &resolved.config.load,
    )?;
    let db = Db::open_default()?;
    let mut told = Vec::new();
    if memory_pressed(&load, &resolved.config.load.busy) {
        for job in db
            .jobs_in("running")?
            .into_iter()
            .filter(|j| j.kind == "session.run")
        {
            // offloaded first: the run's end then leaves the state alone
            db.job_offload(job.id)?;
            if crate::engine::stop_run(job.id) {
                db.record(
                    "legionctl",
                    "session.offload",
                    &serde_json::json!({"job": job.id, "session": job.input["session"]}),
                )?;
                told.push((
                    format!("legionctl: headless run {} offloaded", job.id),
                    "Memory is under pressure; it goes on in its session when memory eases (legionctl jobs list).".into(),
                ));
            }
        }
    } else if !db.jobs_in("offloaded")?.is_empty() {
        operator.resume_jobs("offloaded")?;
        db.record("legionctl", "session.onload", &serde_json::json!({}))?;
    }
    Ok(told)
}
