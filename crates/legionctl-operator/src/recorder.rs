//! The machine daemon's signals in the event log (actor `legionctl-system`), so `legionctl events` and
//! `legionctl trace` show what the machine did next to what agents and people did, and the programs that agent
//! sessions start and the files they open (`session.exec`, `session.open`, from the daemon's eBPF sensor). Needs only
//! the system bus.

use anyhow::Result;
use futures_util::StreamExt;
use legionctl_core::db::Db;
use serde_json::{Value, json};

use crate::system1::System1Proxy;

fn record(kind: &str, payload: &Value) {
    let result = Db::open_default().and_then(|db| db.record("legionctl-system", kind, payload));
    if let Err(e) = result {
        eprintln!("operator: cannot record {kind}: {e:#}");
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
    ready.set();
    // the battery's health once a day (the trend in `battery status`); beside the loop, which ends with the bus
    tokio::spawn(async {
        let mut hourly = tokio::time::interval(std::time::Duration::from_hours(1));
        loop {
            hourly.tick().await;
            if let Ok(Some(sample)) = tokio::task::spawn_blocking(battery_sample).await {
                tokio::task::spawn_blocking(move || record("battery.sample", &sample));
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
        tokio::task::spawn_blocking(move || record(&kind, &payload)).await?;
    }
}

/// A battery sample when the last one is a day old or there is none.
fn battery_sample() -> Option<Value> {
    let filter = legionctl_core::db::EventFilter {
        kinds: &["battery.sample"],
        ..Default::default()
    };
    let db = Db::open_default().ok()?;
    // one a day: the page holds every sample of years
    let newest = db
        .events_page(&filter, 0, 1000)
        .ok()?
        .0
        .last()
        .map(|e| e.ts);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if newest.is_some_and(|ts| now.saturating_sub(u64::try_from(ts).unwrap_or(0)) < 86_400) {
        return None;
    }
    let b = match legionctl_machine::battery::state(&legionctl_machine::sysroot::SysRoot::from_env())
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("recorder: no battery sample: {e:#}");
            return None;
        }
    };
    Some(
        json!({"health_pct": b.health_pct?, "cycle_count": b.cycle_count, "energy_full_wh": b.energy_full_wh}),
    )
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
