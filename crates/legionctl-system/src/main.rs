//! `legionctl-system`: the machine daemon. It turns events into the power policy, one at a time, in one queue:
//!
//! | Event | Source |
//! | --- | --- |
//! | start | the daemon starts |
//! | power source | udev `power_supply` change of the AC adapter |
//! | Fn+Q | udev `platform-profile` change |
//! | resume | logind `PrepareForSleep(false)` |
//! | tick | every 30 s, on AC (knobs that cannot notify) |
//!
//! While a tune runs (`Tune`), events wait: the sweep owns the firmware limits, and the policy applies again after.
//!
//! It serves `org.legionctl.System1` on the system bus: methods change the machine after polkit authorizes the
//! caller, and signals tell the session what changed (`Changed`), the firmware mode after a change (`ModeChanged`),
//! Fn lock (`FnLock`) and which Fn key was pressed (`Key`). Decisions come from `legionctl_machine`; this file only
//! wires event sources and the bus to it.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use legionctl_machine::keys::{self, Debounce, EVENT_SIZE, FN_KEYS_DEVICE};
use legionctl_machine::ledger::{Key, Ledger};
use legionctl_machine::policy::{self, Machine, Options, Source, Step};
use legionctl_machine::react::{Event, react};
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::{cleanup, gpu, profile, telemetry, tune};
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
    let ledger = Ledger::open(root)?;
    let machine = Machine {
        root,
        profile: &profile,
        ledger: &ledger,
        ppd_current: has_command("powerprofilesctl").then(ppd_current).flatten(),
        has_command: &has_command,
    };
    Ok(policy::execute(root, &ledger, &plan(&machine)?))
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
        fn_q_choice: Ledger::open(root).ok()?.get(Key::Override).as_deref() == Some(mode.as_str()),
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

fn failed(e: &anyhow::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(format!("{e:#}"))
}

struct System1 {
    root: SysRoot,
    /// A tune runs: the event loop waits.
    tuning: Arc<AtomicBool>,
}

/// The caller's uid and primary gid, when it is not this process's user (the workload then runs as the caller).
async fn caller(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
) -> zbus::fdo::Result<Option<(u32, u32)>> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("caller has no bus name".into()))?;
    let uid = zbus::fdo::DBusProxy::new(conn)
        .await?
        .get_connection_unix_user(sender.clone().into())
        .await?;
    // /proc/self belongs to this process's effective user
    if std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == uid) {
        return Ok(None);
    }
    let passwd = std::fs::read_to_string("/etc/passwd")
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let gid = passwd
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .find(|f| f.get(2) == Some(&uid.to_string().as_str()))
        .and_then(|f| f.get(3)?.parse().ok())
        .ok_or_else(|| zbus::fdo::Error::Failed(format!("uid {uid} is not in /etc/passwd")))?;
    Ok(Some((uid, gid)))
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
            .map_err(|e| failed(&e))?;
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
        let lines =
            carry_out(&self.root, |m| gpu::set_desktop(m, &choice)).map_err(|e| failed(&e))?;
        if !lines.is_empty() {
            Self::changed(&emitter, "SetDesktopGpu", &lines).await?;
        }
        Ok(lines)
    }

    /// Disable conflicting units and, with `unload_modules`, unload conflicting kernel modules. Conflicting packages
    /// are reported with the command that removes them.
    async fn cleanup(
        &self,
        unload_modules: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.cleanup").await?;
        let opts = cleanup::Options {
            force: false,
            unload_modules,
        };
        let lines = carry_out(&self.root, |m| cleanup::plan(m, opts)).map_err(|e| failed(&e))?;
        if !lines.is_empty() {
            Self::changed(&emitter, "Cleanup", &lines).await?;
        }
        Ok(lines)
    }

    /// Step a firmware limit through values with a workload at each (`tune::TuneSpec` as JSON) and return the report
    /// (`tune::TuneReport` as JSON). The workloads run as the caller; the original value comes back; then the policy
    /// applies again for the current power source (an Fn+Q choice stays).
    async fn tune(
        &self,
        spec: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        authorize(conn, &header, "org.legionctl.tune").await?;
        let spec: tune::TuneSpec = serde_json::from_str(&spec)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let user = caller(conn, &header).await?;
        if self.tuning.swap(true, Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("a tune runs already".into()));
        }
        let root = self.root.clone();
        let report = tokio::task::spawn_blocking(move || {
            let report = tune::run(
                &root,
                &spec,
                &|argv, cancel| tune::run_workload(argv, user, cancel),
                &|| telemetry::sample(&root, Duration::from_secs(1), telemetry::root_sample(&root)),
            );
            let reapplied = carry_out(&root, |m| {
                Ok(m.apply(
                    None,
                    Options {
                        keep_override: true,
                        ..Options::default()
                    },
                )?
                .1)
            });
            (report, reapplied)
        })
        .await;
        self.tuning.store(false, Ordering::SeqCst);
        let (report, reapplied) = report.map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let report = report.map_err(|e| failed(&e))?;
        let mut lines = vec![format!(
            "tune {}: {} steps, recommended {}{}",
            report.attribute,
            report.steps.len(),
            report.recommended.map_or("none".into(), |v| v.to_string()),
            report
                .aborted
                .as_ref()
                .map(|a| format!(", stopped: {a}"))
                .unwrap_or_default()
        )];
        lines.extend(reapplied.map_err(|e| failed(&e))?);
        Self::changed(&emitter, "Tune", &lines).await?;
        serde_json::to_string(&report).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// The readings only root can make (fan RPM, throttle reasons from the MSR), as JSON (`telemetry::RootSample`).
    /// Read-only, so polkit is not asked.
    async fn sample(&self) -> zbus::fdo::Result<String> {
        let root = self.root.clone();
        let sample = tokio::task::spawn_blocking(move || telemetry::root_sample(&root))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        serde_json::to_string(&sample).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
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
    let tuning = Arc::new(AtomicBool::new(false));
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("power_supply")?
        .match_subsystem("platform-profile")?
        .listen()
        .context("listening for udev events")?;
    let monitor = AsyncFd::new(monitor)?;
    let bus = zbus::connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            System1 {
                root: root.clone(),
                tuning: tuning.clone(),
            },
        )?
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

    // a missing sensor is reported, and the daemon runs without it (note 01)
    let fn_lock = fn_lock_watch(&root)
        .inspect_err(|e| eprintln!("FnLock: not watched: {e:#}"))
        .ok();
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
                for event in events.into_iter().filter(|_| !tuning.load(Ordering::SeqCst)) {
                    handle(&root, &emitter, event, &mut announced).await;
                }
            }
            Some(signal) = sleep.next() => {
                if !signal.args()?.start && !tuning.load(Ordering::SeqCst) {
                    handle(&root, &emitter, Event::Resume, &mut announced).await;
                }
            }
            Some(()) = presses.recv() => {
                eprintln!("Key: refresh-rate");
                if let Err(e) = System1::key(&emitter, "refresh-rate").await {
                    eprintln!("Key: cannot send: {e}");
                }
            }
            ready = async {
                match &fn_lock {
                    Some(fd) => fd.ready(Interest::PRIORITY).await,
                    None => std::future::pending().await,
                }
            } => {
                let mut guard = ready?;
                let on = reread(guard.get_inner()).is_ok_and(|v| v == "1");
                guard.clear_ready_matching(Ready::PRIORITY);
                eprintln!("FnLock: {}", if on { "on" } else { "off" });
                if let Err(e) = System1::fn_lock(&emitter, on).await {
                    eprintln!("FnLock: cannot send: {e}");
                }
            }
            _ = tick.tick() => if !tuning.load(Ordering::SeqCst) {
                handle(&root, &emitter, Event::Tick, &mut announced).await;
            },
            _ = term.recv() => {
                notify(sd_notify::NotifyState::Stopping);
                return Ok(());
            }
        }
    }
}
