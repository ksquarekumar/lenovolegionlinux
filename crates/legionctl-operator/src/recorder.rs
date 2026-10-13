//! The machine daemon's signals in the event log (actor `legionctl-system`), so `legionctl events` and
//! `legionctl trace` show what the machine did next to what agents and people did, and the programs that agent
//! sessions start (`session.exec`, from the daemon's eBPF sensor). Needs only the system bus.

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

pub async fn run() -> Result<()> {
    let system = zbus::Connection::system().await?;
    let machine = System1Proxy::new(&system).await?;
    let mut execs = machine.receive_session_exec().await?;
    // a restarted daemon forgets who watches: ask again
    let mut owners = machine.inner().receive_owner_changed().await?;
    watch_sessions(&machine).await;
    let mut changed = machine.receive_changed().await?;
    let mut modes = machine.receive_mode_changed().await?;
    let mut fn_lock = machine.receive_fn_lock().await?;
    let mut keys = machine.receive_key().await?;
    eprintln!("operator: recording machine events");
    loop {
        let (kind, payload) = tokio::select! {
            Some(s) = changed.next() => {
                let a = s.args()?;
                (format!("machine.{}", a.cause.to_lowercase()), json!({"lines": a.lines}))
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
