//! The machine daemon's signals in the event log (actor `legionctl-system`), so `legionctl events` and
//! `legionctl trace` show what the machine did next to what agents and people did, and the programs that agent
//! sessions start and the files they open (`session.exec`, `session.open`, from the daemon's eBPF sensor). Needs only
//! the system bus.

use anyhow::Result;
use futures_util::StreamExt;
use legionctl_core::db::Db;
use serde_json::{Value, json};

use crate::system1::System1Proxy;

/// The recorder's writes: all events that came in while the last write ran, in one transaction. A burst of session
/// activity (a package install opens thousands of new paths, ~50 per second) is then a few short writes, not one
/// commit per event that keeps other writers waiting (todo-189).
fn write(events: &std::sync::mpsc::Receiver<(String, Value)>) {
    while let Ok(first) = events.recv() {
        let batch: Vec<_> = std::iter::once(first).chain(events.try_iter()).collect();
        if let Err(e) = Db::open_log(&legionctl_core::db::default_path())
            .and_then(|db| db.record_all("legionctl-system", &batch))
        {
            eprintln!("operator: cannot record {} events: {e:#}", batch.len());
        }
    }
}

/// Ask the daemon for this user's session activity; a daemon without the sensor says why, and recording goes on.
async fn watch_sessions(machine: &System1Proxy<'_>) {
    match machine.watch_sessions().await {
        Ok(()) => eprintln!("operator: recording session activity"),
        Err(e) => eprintln!("operator: no session activity: {e}"),
    }
}

pub async fn run(ready: crate::Ready) -> Result<()> {
    let system = zbus::Connection::system().await?;
    let machine = System1Proxy::new(&system).await?;
    let mut execs = machine.receive_session_exec().await?;
    let mut opens = machine.receive_session_open().await?;
    // a restarted daemon forgets who watches: ask again
    let mut owners = machine.inner().receive_owner_changed().await?;
    watch_sessions(&machine).await;
    let mut changed = machine.receive_changed().await?;
    let mut modes = machine.receive_mode_changed().await?;
    let mut fn_lock = machine.receive_fn_lock().await?;
    let mut keys = machine.receive_key().await?;
    eprintln!("operator: recording machine events");
    let (record, events) = std::sync::mpsc::channel();
    std::thread::spawn(move || write(&events));
    ready.set();
    // the battery's health once a day (the trend in `battery status`); beside the loop, which ends with the bus
    let battery = record.clone();
    tokio::spawn(async move {
        let mut hourly = tokio::time::interval(std::time::Duration::from_hours(1));
        loop {
            hourly.tick().await;
            match tokio::task::spawn_blocking(battery_sample).await {
                Ok(Ok(sample)) => {
                    crate::faults::works("battery");
                    if let Some(sample) = sample {
                        let _ = battery.send(("battery.sample".into(), sample));
                    }
                }
                Ok(Err(e)) => crate::faults::failed("battery", BATTERY, &e),
                Err(e) => crate::faults::failed("battery", BATTERY, &e.into()),
            }
        }
    });
    loop {
        let (kind, payload) = tokio::select! {
            Some(s) = changed.next() => {
                let a = s.args()?;
                (format!("machine.{}", kebab(&a.cause)), json!({"lines": a.lines}))
            }
            Some(s) = modes.next() => {
                let a = s.args()?;
                ("machine.mode".into(), json!({"mode": a.mode, "fn_q_choice": a.fn_q_choice, "source": a.source}))
            }
            Some(s) = fn_lock.next() => ("machine.fn-lock".into(), json!({"on": s.args()?.on})),
            Some(s) = keys.next() => ("machine.key".into(), json!({"name": s.args()?.name})),
            Some(s) = execs.next() => {
                let a = s.args()?;
                ("session.exec".into(), json!({"session": a.session, "pid": a.pid, "argv": a.argv}))
            }
            Some(s) = opens.next() => {
                let a = s.args()?;
                ("session.open".into(), json!({"session": a.session, "pid": a.pid, "path": a.path}))
            }
            Some(owner) = owners.next() => {
                if owner.is_some() {
                    watch_sessions(&machine).await;
                }
                continue;
            }
            else => return Ok(()),
        };
        record.send((kind, payload))?;
    }
}

/// A battery sample when the last one is a day old or there is none.
/// The fault of a daily battery sample that cannot be taken (user decision Q1).
const BATTERY: &str = "the daily battery sample (the trend in battery status) is not taken";

/// A battery sample when the last one is a day old or there is none; `None` when one is not due, or the battery
/// reports no health.
///
/// # Errors
/// The event log or the battery cannot be read.
fn battery_sample() -> Result<Option<Value>> {
    let filter = legionctl_core::db::EventFilter {
        kinds: &["battery.sample"],
        ..Default::default()
    };
    let db = Db::open_default()?;
    // one a day: the page holds every sample of years
    let newest = db.events_page(&filter, 0, 1000)?.0.last().map(|e| e.ts);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    if newest.is_some_and(|ts| now.saturating_sub(u64::try_from(ts).unwrap_or(0)) < 86_400) {
        return Ok(None);
    }
    let b = legionctl_machine::battery::state(&legionctl_machine::sysroot::SysRoot::from_env())?;
    Ok(b.health_pct.map(|health| {
        json!({"health_pct": health, "cycle_count": b.cycle_count, "energy_full_wh": b.energy_full_wh})
    }))
}

/// `ApplyPower` -> `apply-power`: the daemon's cause as an event kind.
fn kebab(cause: &str) -> String {
    let mut out = String::new();
    for (i, c) in cause.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}
