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
//! caller, and signals tell the session what changed (`Changed`), the firmware mode after a change (`ModeChanged`),
//! Fn lock (`FnLock`) and which Fn key was pressed (`Key`). Decisions come from `legionctl_machine`; this file only
//! wires event sources and the bus to it.

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
use tokio::io::{Interest, Ready};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;

const TICK: Duration = Duration::from_secs(30);
const BUS_NAME: &str = "org.legionctl.System1";
const OBJECT_PATH: &str = "/org/legionctl/System1";
/// How long to wait before looking for the Fn-key device again after it went away (resume, udev remove).
const REOPEN: Duration = Duration::from_secs(3);
/// Written by the kernel when the firmware toggles Fn lock (Fn+Esc); readable only after the first change.
const FN_LOCK_CHANGED: &str = "/sys/class/leds/platform::fnlock/brightness_hw_changed";
const LEDGER_OVERRIDE: &str = "/run/legionctl/firmware-override";

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

/// What the session shows about the firmware mode: the mode, whether it is the user's Fn+Q choice, and the power
/// source whose default it is otherwise.
#[derive(Debug, Clone, PartialEq)]
struct ModeState {
    mode: String,
    fn_q_choice: bool,
    source: &'static str,
}

fn mode(root: &SysRoot) -> Option<ModeState> {
    let (profile, _) = profile::load(None).ok()?;
    let mode = root.read(&profile.paths.platform_profile)?;
    Some(ModeState {
        fn_q_choice: root.read(LEDGER_OVERRIDE).as_deref() == Some(mode.as_str()),
        source: if root.read(&profile.paths.ac_adapter).as_deref() == Some("1") {
            "ac"
        } else {
            "battery"
        },
        mode,
    })
}

/// One event: log what happened (every event but the tick leaves a line), tell the session if anything changed, and
/// announce the firmware mode when it differs from the last one announced.
async fn handle(
    root: &SysRoot,
    emitter: &SignalEmitter<'_>,
    event: Event,
    announced: &mut Option<ModeState>,
) {
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
    let now = mode(root);
    if now != *announced
        && let Some(m) = &now
    {
        eprintln!(
            "{event:?}: mode {}{}",
            m.mode,
            if m.fn_q_choice { " (Fn+Q choice)" } else { "" }
        );
        if let Err(e) = System1::mode_changed(emitter, &m.mode, m.fn_q_choice, m.source).await {
            eprintln!("{event:?}: cannot send ModeChanged: {e}");
        }
    }
    *announced = now;
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

    /// The firmware mode changed; `fn_q_choice` is true when it is the user's Fn+Q choice, otherwise it is the
    /// default of `source` (`ac` or `battery`).
    #[zbus(signal)]
    async fn mode_changed(
        emitter: &SignalEmitter<'_>,
        mode: &str,
        fn_q_choice: bool,
        source: &str,
    ) -> zbus::Result<()>;

    /// Fn lock (Fn+Esc) was turned on or off.
    #[zbus(signal)]
    async fn fn_lock(emitter: &SignalEmitter<'_>, on: bool) -> zbus::Result<()>;
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

/// The Fn-lock change attribute, watched for sysfs notifications (POLLPRI). Sysfs signals a change only to a handle
/// that has read the attribute, so it is read once here and again after every notification.
fn fn_lock_watch(root: &SysRoot) -> Result<AsyncFd<std::fs::File>> {
    let file = std::fs::File::open(root.path(FN_LOCK_CHANGED))
        .with_context(|| format!("opening {FN_LOCK_CHANGED}"))?;
    // empty (ENODATA) until the first change
    let _ = reread(&file);
    Ok(AsyncFd::with_interest(file, Interest::PRIORITY)?)
}

/// Read a sysfs attribute again through the same handle (this also re-arms its change notification).
fn reread(mut file: &std::fs::File) -> std::io::Result<String> {
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(0))?;
    let mut value = String::new();
    file.read_to_string(&mut value)?;
    Ok(value.trim().to_string())
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

    let fn_lock = fn_lock_watch(&root)?;
    // announce later changes only; the session asks for the current state when it starts
    let mut announced = mode(&root);
    handle(&root, &emitter, Event::Start, &mut announced).await;
    notify(sd_notify::NotifyState::Ready);

    loop {
        tokio::select! {
            ready = monitor.readable() => {
                let mut guard = ready?;
                let events: Vec<Event> = guard.get_inner().iter().filter_map(|e| udev_event(&e)).collect();
                guard.clear_ready();
                for event in events {
                    handle(&root, &emitter, event, &mut announced).await;
                }
            }
            Some(signal) = sleep.next() => {
                if !signal.args()?.start {
                    handle(&root, &emitter, Event::Resume, &mut announced).await;
                }
            }
            Some(()) = presses.recv() => {
                eprintln!("Key: refresh-rate");
                if let Err(e) = System1::key(&emitter, "refresh-rate").await {
                    eprintln!("Key: cannot send: {e}");
                }
            }
            ready = fn_lock.ready(Interest::PRIORITY) => {
                let mut guard = ready?;
                let on = reread(guard.get_inner()).is_ok_and(|v| v == "1");
                guard.clear_ready_matching(Ready::PRIORITY);
                eprintln!("FnLock: {}", if on { "on" } else { "off" });
                if let Err(e) = System1::fn_lock(&emitter, on).await {
                    eprintln!("FnLock: cannot send: {e}");
                }
            }
            _ = tick.tick() => handle(&root, &emitter, Event::Tick, &mut announced).await,
            _ = term.recv() => {
                notify(sd_notify::NotifyState::Stopping);
                return Ok(());
            }
        }
    }
}
