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
//! It serves `org.legionctl.System1` on the system bus: methods change the machine after polkit authorizes the
//! caller, and signals tell the session what changed (`Changed`) and which Fn key was pressed (`Key`). Decisions
//! come from `legionctl_machine`; this file only wires event sources and the bus to it.

use std::collections::HashMap;
use std::io::Read;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use legionctl_machine::keys::{self, Debounce, EVENT_SIZE, FN_KEYS_DEVICE};
use legionctl_machine::policy::{self, Machine, Options, Source, Step};
use legionctl_machine::react::{Event, react};
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::{gpu, profile};
use tokio::io::unix::AsyncFd;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;

const TICK: Duration = Duration::from_secs(30);
const BUS_NAME: &str = "org.legionctl.System1";
const OBJECT_PATH: &str = "/org/legionctl/System1";
/// How long to wait before looking for the Fn-key device again after it went away (resume, udev remove).
const REOPEN: Duration = Duration::from_secs(3);

#[zbus::proxy(
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1",
    interface = "org.freedesktop.login1.Manager"
)]
trait Login1 {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority",
    interface = "org.freedesktop.PolicyKit1.Authority"
)]
trait Authority {
    fn check_authorization(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        action_id: &str,
        details: &HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<(bool, bool, HashMap<String, String>)>;
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

/// Plan with the current profile (read each time, so an installed change applies without a restart) and execute.
fn carry_out(
    root: &SysRoot,
    plan: impl FnOnce(&Machine) -> Result<Vec<Step>>,
) -> Result<Vec<String>> {
    let (profile, _) = profile::load(None)?;
    let machine = Machine {
        root,
        profile: &profile,
        ppd_current: has_command("powerprofilesctl").then(ppd_current).flatten(),
        has_command: &has_command,
    };
    Ok(policy::execute(root, &plan(&machine)?))
}

/// One event: log what happened (every event but the tick leaves a line) and tell the session if anything changed.
async fn handle(root: &SysRoot, emitter: &SignalEmitter<'_>, event: Event) {
    match carry_out(root, |m| react(m, event)) {
        Ok(lines) if lines.is_empty() => {
            if event != Event::Tick {
                eprintln!("{event:?}: no changes");
            }
        }
        Ok(lines) => {
            for line in &lines {
                eprintln!("{event:?}: {line}");
            }
            if let Err(e) = System1::changed(emitter, &format!("{event:?}"), &lines).await {
                eprintln!("{event:?}: cannot send Changed: {e}");
            }
        }
        Err(e) => eprintln!("{event:?}: error: {e:#}"),
    }
}

/// Ask polkit whether the caller of this method may perform `action`; polkit may show an authentication dialog.
async fn authorize(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
    action: &str,
) -> zbus::fdo::Result<()> {
    const ALLOW_USER_INTERACTION: u32 = 1;
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("caller has no bus name".into()))?;
    let subject = (
        "system-bus-name",
        HashMap::from([("name", Value::from(sender.as_str()))]),
    );
    let (authorized, _, _) = AuthorityProxy::new(conn)
        .await?
        .check_authorization(
            &subject,
            action,
            &HashMap::new(),
            ALLOW_USER_INTERACTION,
            "",
        )
        .await?;
    if authorized {
        Ok(())
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "not authorized for {action}"
        )))
    }
}

fn failed(e: anyhow::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(format!("{e:#}"))
}

struct System1 {
    root: SysRoot,
}

#[zbus::interface(name = "org.legionctl.System1")]
impl System1 {
    /// Apply the power policy for `ac`, `battery` or `auto` (the current power source). Clears an Fn+Q choice.
    async fn apply(
        &self,
        source: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.power.apply").await?;
        let source = match source.as_str() {
            "ac" => Some(Source::Ac),
            "battery" => Some(Source::Battery),
            "auto" => None,
            other => {
                return Err(zbus::fdo::Error::InvalidArgs(format!(
                    "source must be ac, battery or auto, not {other}"
                )));
            }
        };
        let lines = carry_out(&self.root, |m| Ok(m.apply(source, Options::default())?.1))
            .map_err(failed)?;
        if !lines.is_empty() {
            Self::changed(&emitter, "Apply", &lines).await?;
        }
        Ok(lines)
    }

    /// Choose the GPU GNOME draws the desktop on: `nvidia`, `intel` or `auto`; takes effect at the next login.
    async fn set_desktop_gpu(
        &self,
        choice: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.gpu.desktop").await?;
        let lines = carry_out(&self.root, |m| gpu::set_desktop(m, &choice)).map_err(failed)?;
        if !lines.is_empty() {
            Self::changed(&emitter, "SetDesktopGpu", &lines).await?;
        }
        Ok(lines)
    }

    /// Something changed the machine: an event or a method (`cause`), with what it did. Never sent for no change.
    #[zbus(signal)]
    async fn changed(
        emitter: &SignalEmitter<'_>,
        cause: &str,
        lines: &[String],
    ) -> zbus::Result<()>;

    /// An Fn key that the session acts on was pressed (`refresh-rate`), once per press.
    #[zbus(signal)]
    async fn key(emitter: &SignalEmitter<'_>, name: &str) -> zbus::Result<()>;
}

/// Read the Fn-key device on a thread (blocking reads) and send one message per debounced Fn+R press. The device is
/// looked up by name and opened again when it goes away.
fn watch_fn_keys(root: SysRoot, presses: tokio::sync::mpsc::UnboundedSender<()>) {
    std::thread::spawn(move || {
        let mut debounce = Debounce::default();
        loop {
            let Some(device) = keys::find_device(&root, FN_KEYS_DEVICE) else {
                std::thread::sleep(REOPEN);
                continue;
            };
            let mut file = match std::fs::File::open(root.path(&device)) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Fn keys: cannot read {}: {e}", device.display());
                    std::thread::sleep(REOPEN);
                    continue;
                }
            };
            let mut buf = [0u8; EVENT_SIZE * 64];
            while let Ok(n) = file.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if keys::has_fn_r_press(&buf[..n])
                    && debounce.press(Instant::now())
                    && presses.send(()).is_err()
                {
                    return;
                }
            }
            std::thread::sleep(REOPEN);
        }
    });
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
    let bus = zbus::connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, System1 { root: root.clone() })?
        .build()
        .await
        .with_context(|| format!("owning {BUS_NAME} on the system bus"))?;
    let emitter = SignalEmitter::new(&bus, OBJECT_PATH)?;
    let mut sleep = Login1Proxy::new(&bus)
        .await?
        .receive_prepare_for_sleep()
        .await?;
    let (press_tx, mut presses) = tokio::sync::mpsc::unbounded_channel();
    watch_fn_keys(root.clone(), press_tx);
    let mut tick = tokio::time::interval(TICK);
    tick.tick().await;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    handle(&root, &emitter, Event::Start).await;
    notify(sd_notify::NotifyState::Ready);

    loop {
        tokio::select! {
            ready = monitor.readable() => {
                let mut guard = ready?;
                let events: Vec<Event> = guard.get_inner().iter().filter_map(|e| udev_event(&e)).collect();
                guard.clear_ready();
                for event in events {
                    handle(&root, &emitter, event).await;
                }
            }
            Some(signal) = sleep.next() => {
                if !signal.args()?.start {
                    handle(&root, &emitter, Event::Resume).await;
                }
            }
            Some(()) = presses.recv() => {
                eprintln!("Key: refresh-rate");
                if let Err(e) = System1::key(&emitter, "refresh-rate").await {
                    eprintln!("Key: cannot send: {e}");
                }
            }
            _ = tick.tick() => handle(&root, &emitter, Event::Tick).await,
            _ = term.recv() => {
                notify(sd_notify::NotifyState::Stopping);
                return Ok(());
            }
        }
    }
}
