//! Session module: turns machine events into session actions.
//!
//! | Input | Action |
//! | --- | --- |
//! | `org.legionctl.System1.Key("refresh-rate")` (Fn+R) | toggle the built-in panel's refresh rate through Mutter |
//! | Mutter `MonitorsChanged` | OSD "refresh" when the built-in panel's rate changed |
//! | `org.legionctl.System1.ModeChanged` | OSD "mode" |
//! | `org.legionctl.System1.FnLock` | OSD "fnlock" |
//! | `legionctl overlay on`, every 2 s | the vitals overlay (OSD "vitals" with the lines; an empty value hides it) |
//! | `/run/legionctl/safe-mode` appears (the daemon failed and the firmware runs balanced) | a desktop notification |
//! | `/run/legionctl/boot-confirmed` appears (a start through legionctl's boot entry made it the default) | a desktop notification, once per start |
//!
//! The OSD is drawn by the GNOME Shell extension `legionctl-osd@legionctl` (`org.legionctl.Osd.Show`); it is the only
//! OSD path, so without the extension the operator logs that once and shows nothing.

use anyhow::{Context, Result};
use futures_util::StreamExt;

use crate::display;
use crate::system1::System1Proxy;

#[zbus::proxy(
    default_service = "org.legionctl.Osd",
    default_path = "/org/legionctl/Osd",
    interface = "org.legionctl.Osd"
)]
trait Osd {
    /// `kind`: mode, refresh, fnlock, vitals; `value`: the new state; `detail`: one line under it; `level`: 0..1, or -1 for
    /// no level bar.
    fn show(&self, kind: &str, value: &str, detail: &str, level: f64) -> zbus::Result<()>;
}

/// Shows OSDs through the extension; says once when it is missing instead of repeating on every event.
struct OsdSink<'a> {
    proxy: OsdProxy<'a>,
    missing_reported: bool,
}

impl OsdSink<'_> {
    async fn show(&mut self, kind: &str, value: &str, detail: &str) {
        match self.proxy.show(kind, value, detail, -1.0).await {
            Ok(()) => self.missing_reported = false,
            Err(e) if !self.missing_reported => {
                eprintln!(
                    "OSD: the legionctl-osd GNOME Shell extension does not answer ({e}); no OSD until it is enabled"
                );
                self.missing_reported = true;
            }
            Err(_) => {}
        }
    }
}

/// Tell the user once per reason that the machine daemon is in safe mode (the marker `legionctl-safe` writes).
async fn safe_mode_notice(session: &zbus::Connection, told: &mut Option<String>) {
    let reason =
        match legionctl_machine::sysroot::SysRoot::from_env().read("/run/legionctl/safe-mode") {
            Ok(reason) => reason,
            Err(e) => {
                eprintln!("operator: the safe mode marker cannot be read: {e:#}");
                return;
            }
        };
    if reason.is_none() || reason == *told {
        *told = reason;
        return;
    }
    let body = format!(
        "{}\nSee journalctl -u legionctl-system, then systemctl restart legionctl-system",
        reason.as_deref().unwrap_or_default().trim()
    );
    match crate::desktop::tell(session, "dialog-warning", "legionctl: safe mode", &body).await {
        Ok(()) => *told = reason,
        Err(e) => eprintln!("operator: safe mode notice not shown: {e:#}"),
    }
}

/// Tell the user once per start that legionctl's boot entry became the default (the marker `--boot-confirm` writes:
/// that start's boot id). The notice is an event of the user's log, so a new login in the same start does not tell it
/// again.
async fn boot_notice(session: &zbus::Connection) -> Result<()> {
    let Some(boot) =
        legionctl_machine::sysroot::SysRoot::from_env().read(legionctl_machine::boot::CONFIRMED)?
    else {
        return Ok(());
    };
    let boot = boot.trim();
    let db = legionctl_core::db::Db::open_default()?;
    let filter = legionctl_core::db::EventFilter {
        kinds: &["machine.boot-default"],
        field: Some(("$.boot", boot)),
        naming: None,
    };
    if db.last_event(&filter)?.is_some() {
        return Ok(());
    }
    crate::desktop::tell(
        session,
        "dialog-information",
        "legionctl: boot entry",
        "legionctl's boot entry worked and is now the default.",
    )
    .await?;
    db.record(
        "legionctl-system",
        "machine.boot-default",
        &serde_json::json!({ "boot": boot }),
    )?;
    Ok(())
}

fn mode_detail(fn_q_choice: bool, source: &str) -> String {
    if fn_q_choice {
        "Fn+Q choice until unplug or reboot".into()
    } else {
        format!("{} default", if source == "ac" { "AC" } else { "battery" })
    }
}

/// Runs until the session bus or the system bus goes away; the operations keep working without it.
pub async fn run(
    ready: crate::Ready,
    operator: std::sync::Arc<crate::engine::Operator>,
) -> Result<()> {
    let system = zbus::Connection::system()
        .await
        .context("connecting to the system bus")?;
    let session = zbus::Connection::session()
        .await
        .context("connecting to the session bus")?;
    let machine = System1Proxy::new(&system).await?;
    let mutter = display::DisplayConfigProxy::new(&session).await?;
    let mut osd = OsdSink {
        proxy: OsdProxy::new(&session).await?,
        missing_reported: false,
    };

    let mut keys = machine.receive_key().await?;
    let mut modes = machine.receive_mode_changed().await?;
    let mut fn_lock = machine.receive_fn_lock().await?;
    let mut monitors = mutter.receive_monitors_changed().await?;
    let mut owners = machine.inner().receive_owner_changed().await?;
    let mut told = None;
    // the first tick is at once: a safe mode from before this start is told too
    let mut check = tokio::time::interval(std::time::Duration::from_secs(30));
    let mut vitals = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut overlay_shown = false;
    // a Mutter that does not answer is said in the log; the OSD then has no rate to compare
    let rated = |rate: Result<Option<String>>| {
        rate.unwrap_or_else(|e| {
            eprintln!("operator: the built-in panel's rate cannot be read from Mutter: {e:#}");
            None
        })
    };
    let mut rate = rated(display::built_in_rate(&mutter).await);
    eprintln!(
        "operator: session module running (built-in panel at {} Hz)",
        rate.as_deref().unwrap_or("?")
    );
    ready.set();
    loop {
        tokio::select! {
            Some(signal) = keys.next() => {
                if signal.args()?.name == "refresh-rate" {
                    match display::toggle_refresh(&mutter).await {
                        Ok(what) => eprintln!("Fn+R: {what}"),
                        Err(e) => eprintln!("Fn+R: {e:#}"),
                    }
                }
            }
            Some(_) = monitors.next() => {
                let now = rated(display::built_in_rate(&mutter).await);
                if let (Some(before), Some(after)) = (&rate, &now)
                    && before != after
                {
                    osd.show("refresh", after, "Built-in display").await;
                }
                rate = now;
            }
            Some(signal) = modes.next() => {
                let args = signal.args()?;
                osd.show("mode", &args.mode, &mode_detail(args.fn_q_choice, &args.source)).await;
            }
            Some(signal) = fn_lock.next() => {
                osd.show("fnlock", if signal.args()?.on { "on" } else { "off" }, "Fn+Esc").await;
            }
            // the daemon stopping or starting, and a slow check besides (systemd starts legionctl-safe after it)
            Some(_) = owners.next() => safe_mode_notice(&session, &mut told).await,
            _ = check.tick() => {
                safe_mode_notice(&session, &mut told).await;
                if let Err(e) = boot_notice(&session).await {
                    eprintln!("operator: boot entry notice not shown: {e:#}");
                }
            }
            _ = vitals.tick() => {
                let on = operator.overlay.load(std::sync::atomic::Ordering::Relaxed);
                if on || overlay_shown {
                    let text = if on {
                        let operator = operator.clone();
                        match tokio::task::spawn_blocking(move || operator.vitals()).await? {
                            Ok(lines) => lines.join("\n"),
                            Err(e) => format!("no vitals: {e:#}"),
                        }
                    } else {
                        String::new()
                    };
                    osd.show("vitals", &text, "").await;
                    overlay_shown = on;
                }
            }
        }
    }
}
