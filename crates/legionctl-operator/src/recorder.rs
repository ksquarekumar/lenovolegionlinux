//! The machine daemon's signals in the event log (actor `legionctl-system`), so `legionctl events` and
//! `legionctl trace` show what the machine did next to what agents and people did. Needs only the system bus.

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

pub async fn run() -> Result<()> {
    let system = zbus::Connection::system().await?;
    let machine = System1Proxy::new(&system).await?;
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
            else => return Ok(()),
        };
        tokio::task::spawn_blocking(move || record(&kind, &payload)).await?;
    }
}
