//! Session module: turns machine events into session actions.
//!
//! | Input | Action |
//! | --- | --- |
//! | `org.legionctl.System1.Key("refresh-rate")` (Fn+R) | toggle the built-in panel's refresh rate through Mutter |
//! | Mutter `MonitorsChanged` | OSD "refresh" when the built-in panel's rate changed |
//! | `org.legionctl.System1.ModeChanged` | OSD "mode" |
//! | `org.legionctl.System1.FnLock` | OSD "fnlock" |
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
    /// `kind`: mode, refresh, fnlock; `value`: the new state; `detail`: one line under it; `level`: 0..1, or -1 for
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

fn mode_detail(fn_q_choice: bool, source: &str) -> String {
    if fn_q_choice {
        "Fn+Q choice until unplug or reboot".into()
    } else {
        format!("{} default", if source == "ac" { "AC" } else { "battery" })
    }
}

/// Runs until the session bus or the system bus goes away; the operations keep working without it.
pub async fn run() -> Result<()> {
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
    let mut rate = display::built_in_rate(&mutter).await;
    eprintln!(
        "operator: session module running (built-in panel at {} Hz)",
        rate.as_deref().unwrap_or("?")
    );

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
                let now = display::built_in_rate(&mutter).await;
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
        }
    }
}
