//! `legionctl-system`: the machine daemon. It turns events into the power policy, one at a time, in one queue:
//!
//! | Event | Source | Replaces |
//! | --- | --- | --- |
//! | start | the daemon starts | `legion-apply.service` |
//! | power source | udev `power_supply` change of the AC adapter | udev `RUN+=` rules, `legion-ac/battery.target` |
//! | Fn+Q | udev `platform-profile` change | `legion-follow.service` |
//! | resume | logind `PrepareForSleep(false)` | the system-sleep hook |
//! | tick | every 30 s | `legion-reassert-ac.timer` |
//!
//! Decisions come from `legionctl_machine::react`; this file only wires event sources to it.

use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use legionctl_machine::policy::{self, Machine};
use legionctl_machine::profile;
use legionctl_machine::react::{Event, react};
use legionctl_machine::sysroot::SysRoot;
use tokio::io::unix::AsyncFd;

const TICK: Duration = Duration::from_secs(30);

#[zbus::proxy(
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1",
    interface = "org.freedesktop.login1.Manager"
)]
trait Login1 {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Tell systemd our state; outside systemd (a manual run) there is nobody to tell, and that is fine to say so.
fn notify(state: sd_notify::NotifyState) {
    if let Err(e) = sd_notify::notify(&[state]) {
        eprintln!("sd_notify: {e}");
    }
}

fn has_command(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

fn ppd_current() -> Option<String> {
    let out = std::process::Command::new("powerprofilesctl")
        .arg("get")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Plan and carry out one event. The profile is read each time, so an installed profile change applies without a
/// restart; a failure is logged and the daemon keeps running.
fn handle(root: &SysRoot, event: Event) {
    let result = (|| -> Result<Vec<String>> {
        let (profile, _) = profile::load(None)?;
        let machine = Machine {
            root,
            profile: &profile,
            ppd_current: has_command("powerprofilesctl").then(ppd_current).flatten(),
            has_command: &has_command,
        };
        Ok(policy::execute(root, &react(&machine, event)?))
    })();
    match result {
        // every event but the periodic tick leaves a line, so the journal shows what the daemon saw
        Ok(lines) if lines.is_empty() && event != Event::Tick => eprintln!("{event:?}: no changes"),
        Ok(lines) => {
            for line in lines {
                eprintln!("{event:?}: {line}");
            }
        }
        Err(e) => eprintln!("{event:?}: error: {e:#}"),
    }
}

/// The udev events this daemon reacts to, or `None` for anything else on the socket.
fn udev_event(e: &udev::Event) -> Option<Event> {
    if e.event_type() != udev::EventType::Change {
        return None;
    }
    match e.subsystem()?.to_str()? {
        "power_supply" if e.sysname() == "ADP0" => Some(Event::PowerSource),
        "platform-profile" => Some(Event::PlatformProfile),
        _ => None,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let root = SysRoot::from_env();
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("power_supply")?
        .match_subsystem("platform-profile")?
        .listen()
        .context("listening for udev events")?;
    let monitor = AsyncFd::new(monitor)?;
    let bus = zbus::Connection::system()
        .await
        .context("connecting to the system bus")?;
    let mut sleep = Login1Proxy::new(&bus)
        .await?
        .receive_prepare_for_sleep()
        .await?;
    let mut tick = tokio::time::interval(TICK);
    tick.tick().await;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    handle(&root, Event::Start);
    notify(sd_notify::NotifyState::Ready);

    loop {
        tokio::select! {
            ready = monitor.readable() => {
                let mut guard = ready?;
                let events: Vec<Event> = guard.get_inner().iter().filter_map(|e| udev_event(&e)).collect();
                guard.clear_ready();
                for event in events {
                    handle(&root, event);
                }
            }
            Some(signal) = sleep.next() => {
                if !signal.args()?.start {
                    handle(&root, Event::Resume);
                }
            }
            _ = tick.tick() => handle(&root, Event::Tick),
            _ = term.recv() => {
                notify(sd_notify::NotifyState::Stopping);
                return Ok(());
            }
        }
    }
}
