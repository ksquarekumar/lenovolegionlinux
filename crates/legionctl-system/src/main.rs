//! `legionctl-system`: the machine daemon. It turns events into the power policy, one at a time, in one queue:
//!
//! | Event | Source |
//! | --- | --- |
//! | start | the daemon starts |
//! | power source | udev `power_supply` change of the AC adapter |
//! | Fn+Q | udev `platform-profile` change |
//! | resume | logind `PrepareForSleep(false)` |
//! | tick | every 30 s, on AC (knobs that cannot notify) |
//! | reload | a configset layer changed (inotify), or the user in front did |
//! | workload | a game, build or VM started, or the last one ended (the eBPF workload sensor) |
//!
//! While a tune runs (`Tune`), events wait: the sweep owns the firmware limits, and the policy applies again after.
//!
//! It serves `org.legionctl.System1` on the system bus: methods change the machine after polkit authorizes the
//! caller, and signals tell the session what changed (`Changed`), the firmware mode after a change (`ModeChanged`),
//! Fn lock (`FnLock`) and which Fn key was pressed (`Key`). Decisions come from `legionctl_machine`; this file only
//! wires event sources and the bus to it.

mod exec;
mod folders;
mod pins;
mod runq;
mod workload;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use legionctl_machine::keys::{self, Debounce, EVENT_SIZE, FN_KEYS_DEVICE};
use legionctl_machine::ledger::{Key, Ledger};
use legionctl_machine::policy::{self, Machine, Options, Source, Step};
use legionctl_machine::react::{Event, react};
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::{
    battery, boot, cleanup, configset, fan, gpu, greeter, knobs, profile, telemetry, tune, vfio,
    windows, winvm,
};
use tokio::io::unix::AsyncFd;
use tokio::io::{Interest, Ready};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;

const TICK: Duration = Duration::from_secs(30);
/// Written by `legionctl-system --safe` (legionctl-safe.service, after repeated failures); `doctor` reports it.
const SAFE_MODE: &str = "/run/legionctl/safe-mode";

/// Safe mode (note 13): the daemon failed again and again, so nothing enforces the policy. Hand the machine to the
/// firmware's balanced mode and leave the reason where `doctor` reads it.
fn safe(root: &SysRoot) -> Result<()> {
    let (profile, _) = profile::load(None)?;
    let choices = root
        .read(&profile.paths.platform_profile_choices)
        .unwrap_or_default();
    let mut done = Vec::new();
    if choices.split_whitespace().any(|c| c == "balanced") {
        std::fs::write(root.path(&profile.paths.platform_profile), "balanced\n")?;
        done.push("platform_profile balanced");
    }
    let marker = root.path(SAFE_MODE);
    std::fs::create_dir_all(marker.parent().context("no parent")?)?;
    std::fs::write(
        &marker,
        format!(
            "legionctl-system failed repeatedly and stopped; set: {}. See journalctl -u legionctl-system, then \
             systemctl restart legionctl-system\n",
            if done.is_empty() {
                "nothing".to_string()
            } else {
                done.join(", ")
            }
        ),
    )?;
    eprintln!("safe mode: {}", std::fs::read_to_string(&marker)?.trim());
    Ok(())
}
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

/// The machine with the current profile and configset layers (read each time, so an installed change applies without a
/// restart).
fn on_machine<T>(root: &SysRoot, f: impl FnOnce(&Machine) -> Result<T>) -> Result<T> {
    let (profile, _) = profile::load(None)?;
    let ledger = Ledger::open(root)?;
    let layers = configset::load(root, &profile, &ledger);
    f(&Machine {
        root,
        profile: &profile,
        ledger: &ledger,
        ppd_current: has_command("powerprofilesctl").then(ppd_current).flatten(),
        has_command: &has_command,
        layers: &layers,
    })
}

/// Plan with the current profile and execute.
fn carry_out(
    root: &SysRoot,
    plan: impl FnOnce(&Machine) -> Result<Vec<Step>>,
) -> Result<Vec<String>> {
    on_machine(root, |m| Ok(policy::execute(root, m.ledger, &plan(m)?)))
}

/// A fan change: a refused plan is the caller's error; the session hears what changed.
async fn machine_change(
    root: &SysRoot,
    emitter: &SignalEmitter<'_>,
    cause: &str,
    plan: impl FnOnce(&Machine) -> Result<Vec<Step>>,
) -> zbus::fdo::Result<Vec<String>> {
    let lines =
        carry_out(root, plan).map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{e:#}")))?;
    System1::changed(emitter, cause, &lines).await?;
    Ok(lines)
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

/// Carry out a boot plan step by step, stopping at the first failure: a later step (a firmware entry) must not follow a
/// failed one (an unsigned loader).
fn boot_steps(root: &SysRoot, steps: &[Step]) -> Result<Vec<String>> {
    let ledger = Ledger::open(root)?;
    let mut lines = Vec::new();
    for step in steps {
        let done = policy::execute(root, &ledger, std::slice::from_ref(step));
        if let Some(failure) = done.iter().find(|l| l.starts_with("warning")) {
            anyhow::bail!("{failure}\ndone before it:\n- {}", lines.join("\n- "));
        }
        lines.extend(done);
    }
    Ok(lines)
}

/// A mode that runs once and ends, instead of the daemon: `--safe` (the safe-mode unit), `--boot-rebuild` (the pacman
/// hook after a kernel or initramfs change), `--greeter-remove` (the package's removal: GDM back, so the computer keeps
/// a login screen).
fn one_shot(root: &SysRoot) -> Option<Result<()>> {
    let print = |lines: Vec<String>| {
        for line in lines {
            println!("{line}");
        }
    };
    match std::env::args().nth(1).as_deref() {
        Some("--safe") => Some(safe(root)),
        Some("--boot-rebuild") => Some(boot_rebuild(root).map(print)),
        Some("--greeter-remove") if greeter::state(root).installed => Some(
            greeter::remove_plan(root)
                .and_then(|plan| boot_steps(root, &plan))
                .map(print),
        ),
        Some("--greeter-remove") => Some(Ok(())),
        _ => None,
    }
}

/// Take the NVIDIA GPU from Linux for `uid` (its services kept in the ledger for the return).
fn gpu_release(root: &SysRoot, uid: u32) -> Result<Vec<String>> {
    let ledger = Ledger::open(root)?;
    let (lines, services) = vfio::release_nvidia(root, uid)?;
    ledger.set(Key::GpuServices, Some(&services.join(" ")))?;
    Ok(lines)
}

/// Give the NVIDIA GPU back to Linux, with the services that the release stopped.
fn gpu_return(root: &SysRoot) -> Result<Vec<String>> {
    let ledger = Ledger::open(root)?;
    let services: Vec<String> = ledger
        .get(Key::GpuServices)
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect();
    let lines = vfio::return_nvidia(root, &services)?;
    ledger.set(Key::GpuServices, None)?;
    Ok(lines)
}

/// A choice of the greeter before login, for `user` (a person's account): the Windows VM (`vm-start`, `vm-stop`,
/// `autostart-off|login|boot`), the Windows disk's partitions (`disk-attach` to Linux, `disk-detach`), the NVIDIA GPU
/// (`gpu-linux`, `gpu-none`: on vfio-pci for the user's VM).
fn greeter_act(root: &SysRoot, user: &str, action: &str) -> Result<Vec<String>> {
    let uid = greeter::person(root, user)?;
    let vm_runs = Ledger::open(root)?.get(Key::WindowsVm).is_some();
    match action {
        "disk-attach" | "disk-detach" => {
            anyhow::ensure!(!vm_runs, "the Windows VM has the disk now");
            boot_steps(root, &winvm::disk_plan(root, action == "disk-attach")?)
        }
        "gpu-none" => gpu_release(root, uid),
        "gpu-linux" => gpu_return(root),
        _ => boot_steps(root, &greeter::vm_plan(root, user, uid, action)?),
    }
}

/// Every kernel's UKI again (the pacman hook); nothing when legionctl does not own the boot.
fn boot_rebuild(root: &SysRoot) -> Result<Vec<String>> {
    let state = boot::state(root)?;
    if state.ours.is_none() {
        return Ok(vec!["no legionctl boot entry: nothing to rebuild".into()]);
    }
    boot_steps(root, &boot::rebuild_plan(root, &state)?)
}

/// The uid of the caller of a method.
async fn caller_uid(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
) -> zbus::fdo::Result<u32> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("caller has no bus name".into()))?;
    zbus::fdo::DBusProxy::new(conn)
        .await?
        .get_connection_unix_user(sender.clone().into())
        .await
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
    /// The run-queue latency sensor, or why it does not run.
    runq: Arc<Result<runq::Runq, String>>,
    /// Who gets the session activity: uid -> its connection (`WatchSessions`; the newest call of a user wins).
    watchers: Arc<Mutex<BTreeMap<u32, String>>>,
    /// Why the session activity sensor does not run.
    exec_error: Option<String>,
    /// The hash of the interface this build serves (`Version`).
    version: String,
}

impl System1 {
    /// A layer changed (`line` says how): apply the layers again and tell the session.
    async fn layers_changed(
        &self,
        emitter: &SignalEmitter<'_>,
        cause: &str,
        line: String,
    ) -> zbus::fdo::Result<Vec<String>> {
        let mut lines = vec![line];
        if !self.tuning.load(Ordering::SeqCst) {
            lines.extend(
                carry_out(&self.root, |m| react(m, Event::Reload)).map_err(|e| failed(&e))?,
            );
        }
        Self::changed(emitter, cause, &lines).await?;
        Ok(lines)
    }
}

/// Write the system layer `name` with `content`, or remove it when `content` is empty; a layer that fails its own
/// checks is refused. Returns what was done.
fn write_layer(root: &SysRoot, name: &str, content: &str) -> Result<String> {
    anyhow::ensure!(
        configset::good_name(name),
        "{name}: a layer name is <two digits>-<a-z, 0-9 and ->"
    );
    let file = format!("{}/{name}.jsonc", configset::SYSTEM);
    if content.is_empty() {
        return match std::fs::remove_file(root.path(&file)) {
            Ok(()) => Ok(format!("removed {file}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(format!("{file} is not there"))
            }
            Err(e) => Err(e).with_context(|| format!("removing {file}")),
        };
    }
    let (profile, _) = profile::load(None)?;
    let layer = configset::parse(
        Some(root),
        &profile,
        configset::Folder::System,
        name,
        &file,
        Ok(content.to_string()),
    );
    if let Some(why) = layer.off {
        anyhow::bail!("{file}: {why}");
    }
    let path = root.path(&file);
    std::fs::create_dir_all(path.parent().context("no parent")?)?;
    std::fs::write(&path, content).with_context(|| format!("writing {file}"))?;
    Ok(format!("wrote {file}"))
}

/// The readings only root makes, with the eBPF sensor's histogram (or its error).
fn root_part(root: &SysRoot, runq: &Result<runq::Runq, String>) -> telemetry::RootSample {
    let mut part = telemetry::root_sample(root);
    match runq.as_ref().map(runq::Runq::per_cpu) {
        Ok(Ok(per_cpu)) => part.runq = Some(telemetry::by_class(root, &per_cpu)),
        Ok(Err(e)) => part.runq_error = Some(format!("reading the histogram: {e:#}")),
        Err(e) => part.runq_error = Some(e.clone()),
    }
    part
}

/// The caller's uid and primary gid, when it is not this process's user (the workload then runs as the caller).
async fn caller(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
) -> zbus::fdo::Result<Option<(u32, u32)>> {
    let uid = caller_uid(conn, header).await?;
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
    /// The hash of the interface this daemon was built from; the operator compares it with its own.
    #[zbus(property)]
    fn version(&self) -> String {
        self.version.clone()
    }

    /// Apply the power policy for `ac`, `battery` or `auto` (the current power source). Clears an Fn+Q choice.
    async fn apply_power(
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
            Self::changed(&emitter, "ApplyPower", &lines).await?;
        }
        Ok(lines)
    }

    /// Apply the configset layers again (after the user changed own layers): the same apply as after resume. The
    /// layers are the configured policy, so polkit is not asked.
    async fn reload(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        if self.tuning.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed(
                "a tune runs; the policy applies again after it".into(),
            ));
        }
        let lines = carry_out(&self.root, |m| react(m, Event::Reload)).map_err(|e| failed(&e))?;
        if !lines.is_empty() {
            Self::changed(&emitter, "Reload", &lines).await?;
        }
        Ok(lines)
    }

    /// Write the system layer `name` (`/etc/legionctl/configsets/<name>.jsonc`) with `content` (JSONC), or remove it
    /// (empty `content`), then apply the layers again. A layer that fails its checks is refused.
    async fn set_layer(
        &self,
        name: String,
        content: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.rules").await?;
        let line = write_layer(&self.root, &name, &content)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{e:#}")))?;
        self.layers_changed(&emitter, "SetLayer", line).await
    }

    /// Turn a layer on or off (`on`, `off`), or give it its default again (`default`: a generated layer is off, the
    /// others on); `name` is its id (`shipped/20-ac`), its file name, or its name without the order (`tuned`).
    async fn set_layer_state(
        &self,
        name: String,
        state: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.rules").await?;
        let enabled = match state.as_str() {
            "on" => Some(true),
            "off" => Some(false),
            "default" => None,
            other => {
                return Err(zbus::fdo::Error::InvalidArgs(format!(
                    "state must be on, off or default, not {other}"
                )));
            }
        };
        let line = on_machine(&self.root, |m| {
            let layer = configset::find(m.layers, &name).map_err(anyhow::Error::msg)?;
            m.ledger.set_layer(&layer.id(), enabled)?;
            Ok(format!("layer {}: {state}", layer.id()))
        })
        .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{e:#}")))?;
        self.layers_changed(&emitter, "SetLayerState", line).await
    }

    /// Hold knobs at values against the policy (path -> value; a `*` path holds every CPU of a per-CPU knob): writes
    /// them now; the policy leaves them alone until `Release`.
    async fn hold(
        &self,
        values: std::collections::BTreeMap<String, String>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.knobs.hold").await?;
        let lines = carry_out(&self.root, |m| knobs::hold_plan(m, &values))
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{e:#}")))?;
        Self::changed(&emitter, "Hold", &lines).await?;
        Ok(lines)
    }

    /// Release a held knob: the policy for the current power source takes it again.
    async fn release(
        &self,
        path: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.knobs.hold").await?;
        // the hold on the path, and the holds on the files it covers (one CPU's file of a per-CPU knob)
        let mut lines = carry_out(&self.root, |m| {
            Ok(m.ledger
                .holds()
                .into_keys()
                .filter(|held| *held == path || policy::held_by(&path, held))
                .map(|held| policy::Step::Hold {
                    path: held,
                    value: None,
                })
                .collect())
        })
        .map_err(|e| failed(&e))?;
        // the whole policy for the current power source, keeping a Fn+Q choice
        let again = Options {
            keep_override: true,
            ..Options::default()
        };
        lines.extend(
            carry_out(&self.root, |m| Ok(m.apply(None, again)?.1)).map_err(|e| failed(&e))?,
        );
        Self::changed(&emitter, "Release", &lines).await?;
        Ok(lines)
    }

    /// Give `devices` (one whole IOMMU group that the host does not use) to the caller's VM: bind them to vfio-pci,
    /// give the caller `/dev/vfio/<group>`, and let its process `pid` (the VM runner; 0: none) lock `memlock` bytes.
    async fn passthrough(
        &self,
        devices: Vec<String>,
        pid: u32,
        memlock: u64,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.vfio").await?;
        let uid = caller_uid(conn, &header).await?;
        let mut lines = vfio::bind(&self.root, &devices, uid)
            .map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))?;
        // pid 0: no VM runner to raise (a person binding by hand)
        match (pid != 0)
            .then(|| vfio::allow_memlock(&self.root, pid, uid, memlock))
            .transpose()
        {
            Ok(line) => lines.extend(line),
            Err(e) => {
                // the devices go back: the VM cannot start without the limit
                let _ = vfio::unbind(&self.root, &devices);
                return Err(zbus::fdo::Error::Failed(format!("{e:#}")));
            }
        }
        Self::changed(&emitter, "Passthrough", &lines).await?;
        Ok(lines)
    }

    /// Make the host ready for the caller's Windows VM (note 05): the guards, then the Windows disk on vfio-pci, the
    /// USB devices `usb` (vendor:product) and the memory lock of `pid`. Returns what it did, the devices it bound (for
    /// QEMU) and the firmware's `MSDM` table (the Windows license, root-only on the host) for the VM.
    async fn windows_vm_begin(
        &self,
        usb: Vec<String>,
        pid: u32,
        memlock: u64,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<(Vec<String>, Vec<String>, Vec<u8>)> {
        authorize(conn, &header, "org.legionctl.windows.vm").await?;
        let uid = caller_uid(conn, &header).await?;
        let ledger = Ledger::open(&self.root).map_err(|e| failed(&e))?;
        if ledger.get(Key::WindowsVm).is_some() {
            return Err(zbus::fdo::Error::Failed(
                "a Windows VM runs already (legionctl windows vm stop)".into(),
            ));
        }
        let msdm = winvm::msdm(&self.root).map_err(|e| failed(&e))?;
        let ask = winvm::Ask { usb, pid, memlock };
        let (lines, record) = winvm::begin(&self.root, uid, &ask).map_err(|e| failed(&e))?;
        let kept =
            serde_json::to_string(&record).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        if let Err(e) = ledger.set(Key::WindowsVm, Some(&kept)) {
            let undone = winvm::end(&self.root, &record);
            return Err(zbus::fdo::Error::Failed(format!(
                "{e:#}; undone: {}",
                undone.join("; ")
            )));
        }
        Self::changed(&emitter, "WindowsVmBegin", &lines).await?;
        Ok((lines, record.devices, msdm))
    }

    /// Let the caller start and end the Windows VM without a prompt (`on`: a polkit rule for this user and this
    /// action only, for autostart), or ask again (`off`).
    async fn set_windows_autostart(
        &self,
        on: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.windows.vm").await?;
        let uid = caller_uid(conn, &header).await?;
        let passwd = std::fs::read_to_string(self.root.path("/etc/passwd"))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let user = passwd
            .lines()
            .map(|l| l.split(':').collect::<Vec<_>>())
            .find(|f| f.get(2) == Some(&uid.to_string().as_str()))
            .and_then(|f| f.first().map(|n| (*n).to_string()))
            .ok_or_else(|| zbus::fdo::Error::Failed(format!("uid {uid} is not in /etc/passwd")))?;
        let line = winvm::set_autostart_rule(&self.root, on.then_some(user.as_str()))
            .map_err(|e| failed(&e))?;
        let lines = vec![line];
        Self::changed(&emitter, "SetWindowsAutostart", &lines).await?;
        Ok(lines)
    }

    /// Undo what the Windows VM's start changed on the host (after the VM ended, or after it failed).
    async fn windows_vm_end(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.windows.vm").await?;
        let ledger = Ledger::open(&self.root).map_err(|e| failed(&e))?;
        let Some(kept) = ledger.get(Key::WindowsVm) else {
            return Ok(vec!["no Windows VM ran".into()]);
        };
        let record: winvm::Record =
            serde_json::from_str(&kept).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let lines = winvm::end(&self.root, &record);
        ledger.set(Key::WindowsVm, None).map_err(|e| failed(&e))?;
        Self::changed(&emitter, "WindowsVmEnd", &lines).await?;
        Ok(lines)
    }

    /// Take the NVIDIA GPU from Linux (`gpu give nvidia none`): its services stopped, its group on vfio-pci for the
    /// caller; refused while the host uses it.
    async fn gpu_release(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.vfio").await?;
        let uid = caller_uid(conn, &header).await?;
        let lines = gpu_release(&self.root, uid).map_err(|e| failed(&e))?;
        Self::changed(&emitter, "GpuRelease", &lines).await?;
        Ok(lines)
    }

    /// Give the NVIDIA GPU back to Linux (`gpu give nvidia linux`).
    async fn gpu_return(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.vfio").await?;
        let lines = gpu_return(&self.root).map_err(|e| failed(&e))?;
        Self::changed(&emitter, "GpuReturn", &lines).await?;
        Ok(lines)
    }

    /// The boot takeover (todo-80): `install` (then the next start tries it once), `confirm`, `rebuild`, `remove`.
    async fn boot(
        &self,
        action: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.boot").await?;
        let root = self.root.clone();
        let lines = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            match action.as_str() {
                "install" => {
                    let mut lines = boot_steps(&root, &boot::install_plan(&root)?)?;
                    let once = boot::try_once(&boot::state(&root)?)?;
                    lines.extend(boot_steps(&root, &[once])?);
                    lines.push(
                        "the next start tries legionctl once; after it, run `legionctl boot confirm`".into(),
                    );
                    Ok(lines)
                }
                "confirm" => boot_steps(&root, &boot::confirm_plan(&boot::state(&root)?)?),
                "rebuild" => boot_rebuild(&root),
                "remove" => {
                    let state = boot::state(&root)?;
                    boot_steps(&root, &boot::remove_plan(&root, &state)?)
                }
                other => anyhow::bail!("boot action must be install, confirm, rebuild or remove, not {other}"),
            }
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(|e| failed(&e))?;
        Self::changed(&emitter, "Boot", &lines).await?;
        Ok(lines)
    }

    /// The greeter in place of GDM (todo-83): `install` or `remove`.
    async fn greeter(
        &self,
        action: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.greeter").await?;
        let root = self.root.clone();
        let lines = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let plan = match action.as_str() {
                "install" => greeter::install_plan(&root)?,
                "remove" => greeter::remove_plan(&root)?,
                other => anyhow::bail!("greeter action must be install or remove, not {other}"),
            };
            boot_steps(&root, &plan)
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(|e| failed(&e))?;
        Self::changed(&emitter, "Greeter", &lines).await?;
        Ok(lines)
    }

    /// A choice of the greeter before login for `user`: `vm-start`, `vm-stop`, `autostart-off|login|boot`,
    /// `disk-attach`, `disk-detach`, `gpu-linux`, `gpu-none`.
    async fn greeter_act(
        &self,
        user: String,
        action: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.greeter.act").await?;
        let root = self.root.clone();
        let lines = tokio::task::spawn_blocking(move || greeter_act(&root, &user, &action))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            .map_err(|e| failed(&e))?;
        Self::changed(&emitter, "GreeterAct", &lines).await?;
        Ok(lines)
    }

    /// Give `devices` back from vfio-pci to their own drivers, after the VM ended.
    async fn passthrough_end(
        &self,
        devices: Vec<String>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.vfio").await?;
        let lines = vfio::unbind(&self.root, &devices)
            .map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))?;
        Self::changed(&emitter, "PassthroughEnd", &lines).await?;
        Ok(lines)
    }

    /// Choose the GPU GNOME draws the desktop on: `nvidia`, `intel` or `auto`; takes effect at the next login.
    async fn set_primary_gpu(
        &self,
        choice: String,
        release_nvidia: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.gpu.primary").await?;
        let choice: gpu::DesktopGpu = choice
            .parse()
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{e}")))?;
        let lines = carry_out(&self.root, |m| gpu::set_desktop(m, choice, release_nvidia))
            .map_err(|e| failed(&e))?;
        if !lines.is_empty() {
            Self::changed(&emitter, "SetPrimaryGpu", &lines).await?;
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
        let runq = self.runq.clone();
        let report = tokio::task::spawn_blocking(move || {
            let report = tune::run(
                &root,
                &spec,
                &|argv, cancel| tune::run_workload(argv, user, cancel),
                &|| telemetry::sample(&root, Duration::from_secs(1), &|| root_part(&root, &runq)),
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

    /// Boot Windows once (`BootNext` to its firmware entry), then reboot. Nothing reboots when setting `BootNext`
    /// fails.
    async fn reboot_windows(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.windows.reboot").await?;
        let steps = windows::state(&self.root)
            .and_then(|state| windows::reboot_plan(&state))
            .map_err(|e| failed(&e))?;
        let ledger = Ledger::open(&self.root).map_err(|e| failed(&e))?;
        let (boot_next, reboot) = steps.split_at(1);
        let mut lines = policy::execute(&self.root, &ledger, boot_next);
        if let Some(warning) = lines.iter().find(|l| l.starts_with("warning")) {
            return Err(zbus::fdo::Error::Failed(format!(
                "not rebooting: {warning}"
            )));
        }
        lines.push(format!(
            "BootNext set: {}",
            boot_next
                .iter()
                .filter_map(policy::describe)
                .collect::<String>()
        ));
        // the signal goes out before the reboot takes the bus down
        Self::changed(&emitter, "RebootWindows", &lines).await?;
        lines.extend(policy::execute(&self.root, &ledger, reboot));
        Ok(lines)
    }

    /// The fan controls as JSON (`fan::FanState`): the firmware's fan table, the user's curve, fan max mode.
    /// Read-only, so polkit is not asked.
    async fn fan_status(&self) -> zbus::fdo::Result<String> {
        let root = self.root.clone();
        // off the bus thread: the firmware call waits while a sample's calls run
        let state = tokio::task::spawn_blocking(move || on_machine(&root, |m| Ok(fan::state(m))))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            .map_err(|e| failed(&e))?;
        serde_json::to_string(&state).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Write the fan table (10 levels 1-10; custom mode only), read it back, and keep it as the user's curve: it is
    /// written again after resume and at each entry to custom mode.
    async fn set_fan_curve(
        &self,
        levels: Vec<u8>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.fan").await?;
        machine_change(&self.root, &emitter, "SetFanCurve", |m| {
            let plan = fan::set_curve(m, &levels)?;
            fan::require_verified(m)?;
            Ok(plan)
        })
        .await
    }

    /// Forget the user's fan curve; the profile's default curve applies (now, in custom mode).
    async fn reset_fan_curve(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.fan").await?;
        machine_change(&self.root, &emitter, "ResetFanCurve", |m| {
            let plan = fan::reset_curve(m)?;
            fan::require_verified(m)?;
            Ok(plan)
        })
        .await
    }

    /// The battery's charge mode: standard, conservation (near 80 %) or rapid.
    async fn set_battery_mode(
        &self,
        mode: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.battery").await?;
        machine_change(&self.root, &emitter, "SetBatteryMode", |m| {
            battery::mode_plan(m.root, &mode)
        })
        .await
    }

    /// USB charging while the laptop is off, on or off.
    async fn set_usb_charging(
        &self,
        on: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.battery").await?;
        machine_change(&self.root, &emitter, "SetUsbCharging", |m| {
            battery::usb_charging_plan(m.root, on)
        })
        .await
    }

    /// Fan max mode on or off; set again after resume and at start while on.
    async fn set_fan_max(
        &self,
        on: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.fan").await?;
        machine_change(&self.root, &emitter, "SetFanMax", |m| fan::max(m, on)).await
    }

    /// Full fan speed on or off, for dust cleaning (custom mode only).
    async fn set_fan_full_speed(
        &self,
        on: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.fan").await?;
        machine_change(&self.root, &emitter, "SetFanFullSpeed", |m| {
            let plan = fan::clean(m, on)?;
            fan::require_verified(m)?;
            Ok(plan)
        })
        .await
    }

    /// Lock or release the EC fan controller (a raw EC write); refused unless the profile marks it verified.
    async fn set_fan_lock(
        &self,
        on: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.fan.lock").await?;
        machine_change(&self.root, &emitter, "SetFanLock", |m| fan::lock(m, on)).await
    }

    /// The readings only root can make (fan RPM, throttle reasons from the MSR), as JSON (`telemetry::RootSample`).
    /// Read-only, so polkit is not asked.
    async fn sample(&self) -> zbus::fdo::Result<String> {
        let (root, runq) = (self.root.clone(), self.runq.clone());
        let sample = tokio::task::spawn_blocking(move || root_part(&root, &runq))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        serde_json::to_string(&sample).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// The devices that can go to a VM (`vfio::Candidate` list as JSON): read-only, so polkit is not asked.
    async fn passthroughs(&self) -> zbus::fdo::Result<String> {
        let root = self.root.clone();
        let list = tokio::task::spawn_blocking(move || vfio::candidates(&root))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        serde_json::to_string(&list).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Send the starts of programs in the caller's own agent sessions (`SessionExec`) to the caller's connection, from
    /// now on. Only the caller's sessions, and only to it: arguments can hold secrets.
    async fn watch_sessions(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        if let Some(e) = &self.exec_error {
            return Err(zbus::fdo::Error::Failed(format!(
                "the eBPF session activity sensor does not run: {e}"
            )));
        }
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("caller has no bus name".into()))?;
        let uid = caller_uid(conn, &header).await?;
        self.watchers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(uid, sender.to_string());
        Ok(())
    }

    /// A process in agent session `session` started a program; sent only to the session owner's watcher.
    #[zbus(signal)]
    async fn session_exec(
        emitter: &SignalEmitter<'_>,
        session: &str,
        pid: u32,
        argv: &[String],
    ) -> zbus::Result<()>;

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

/// The session activity sensor's loop: each second it watches the scopes of the users who asked; each program start
/// goes to its session owner's watcher.
async fn session_activity(
    bus: zbus::Connection,
    watchers: Arc<Mutex<BTreeMap<u32, String>>>,
    mut ring: AsyncFd<aya::maps::RingBuf<aya::maps::MapData>>,
    mut watch: exec::Watch,
) {
    // new and ended session scopes reach the sensor within a second
    let mut scan = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = scan.tick() => {
                let uids: BTreeSet<u32> = watchers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .keys()
                    .copied()
                    .collect();
                if let Err(e) = watch.refresh(&uids) {
                    eprintln!("exec sensor: cannot watch the sessions: {e:#}");
                }
            }
            ready = ring.readable_mut() => {
                let Ok(mut guard) = ready else {
                    eprintln!("exec sensor: the ring buffer failed; stopped");
                    return;
                };
                let execs = watch.drain(guard.get_inner_mut());
                guard.clear_ready();
                for e in execs {
                    send_exec(&bus, &watchers, e).await;
                }
            }
        }
    }
}

/// Send one session's program start to its owner's watcher alone (a unicast signal).
async fn send_exec(bus: &zbus::Connection, watchers: &Mutex<BTreeMap<u32, String>>, e: exec::Exec) {
    let Some(name) = watchers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&e.uid)
        .cloned()
    else {
        return;
    };
    let sent = async {
        let to = SignalEmitter::new(bus, OBJECT_PATH)?
            .set_destination(zbus::names::BusName::try_from(name.as_str())?);
        System1::session_exec(&to, &e.session, e.pid, &e.argv).await
    }
    .await;
    if let Err(err) = sent {
        eprintln!("exec sensor: cannot send to {name}: {err}");
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

/// The eBPF sensors. One that cannot load is reported (Sample, doctor), and the daemon runs without it (note 04).
type Execs = Result<(AsyncFd<aya::maps::RingBuf<aya::maps::MapData>>, exec::Watch), String>;

fn sensors(root: &SysRoot) -> (Arc<Result<runq::Runq, String>>, Execs) {
    let runq = Arc::new(runq::Runq::start(root).map_err(|e| format!("{e:#}")));
    if let Err(e) = runq.as_ref() {
        eprintln!("runq sensor: not running: {e}");
    }
    let execs = exec::start(root).map_err(|e| format!("{e:#}"));
    if let Err(e) = &execs {
        eprintln!("exec sensor: not running: {e}");
    }
    (runq, execs)
}

/// The udev events of the power supply and the platform profile.
fn udev_monitor() -> Result<AsyncFd<udev::MonitorSocket>> {
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("power_supply")?
        .match_subsystem("platform-profile")?
        .listen()
        .context("listening for udev events")?;
    Ok(AsyncFd::new(monitor)?)
}

/// Own the name on the system bus and serve `system` there.
async fn serve(system: System1) -> Result<zbus::Connection> {
    zbus::connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, system)?
        .build()
        .await
        .with_context(|| format!("owning {BUS_NAME} on the system bus"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let root = SysRoot::from_env();
    if let Some(done) = one_shot(&root) {
        return done;
    }
    let tuning = Arc::new(AtomicBool::new(false));
    let (runq, execs) = sensors(&root);
    let watchers = Arc::new(Mutex::new(BTreeMap::new()));
    let monitor = udev_monitor()?;
    let bus = serve(System1 {
        root: root.clone(),
        tuning: tuning.clone(),
        runq,
        watchers: watchers.clone(),
        exec_error: execs.as_ref().err().cloned(),
        version: legionctl_machine::interface_version(),
    })
    .await?;
    let emitter = SignalEmitter::new(&bus, OBJECT_PATH)?;
    // the exec sensor runs beside the loop below; if it stops after it started, the daemon stops with an error and
    // systemd starts it again (DESIGN/02)
    let mut activity = execs
        .ok()
        .map(|(ring, watch)| tokio::spawn(session_activity(bus.clone(), watchers, ring, watch)));
    let mut sleep = Login1Proxy::new(&bus)
        .await?
        .receive_prepare_for_sleep()
        .await?;
    let (press_tx, mut presses) = tokio::sync::mpsc::unbounded_channel();
    watch_fn_keys(root.clone(), press_tx);
    let mut tick = tokio::time::interval(TICK);
    tick.tick().await;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut folders = folders::Folders::watch(&root)?;
    let mut workloads = workload::Sensor::start(&root)?;

    // a missing sensor is reported, and the daemon runs without it (note 01)
    let fn_lock = fn_lock_watch(&root)
        .inspect_err(|e| eprintln!("FnLock: not watched: {e:#}"))
        .ok();
    // announce later changes only; the session asks for the current state when it starts
    let mut announced = mode(&root);
    handle(&root, &emitter, Event::Start, &mut announced).await;
    notify(sd_notify::NotifyState::Ready);
    // running again: safe mode is over
    let _ = std::fs::remove_file(root.path(SAFE_MODE));

    loop {
        tokio::select! {
            ended = async { activity.as_mut().expect("guarded").await }, if activity.is_some() => {
                anyhow::bail!("the exec sensor stopped: {}", ended.err().map_or_else(|| "it ended".into(), |e| e.to_string()));
            }
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
            changed = folders.changed() => {
                changed?;
                if !tuning.load(Ordering::SeqCst) {
                    handle(&root, &emitter, Event::Reload, &mut announced).await;
                }
            }
            changed = workloads.changed(&root) => {
                changed?;
                if !tuning.load(Ordering::SeqCst) {
                    handle(&root, &emitter, Event::Workload, &mut announced).await;
                }
            }
            _ = term.recv() => {
                notify(sd_notify::NotifyState::Stopping);
                // a stop ends the sensors; a failure keeps them for the next start (D7)
                return pins::unpin(&root);
            }
        }
    }
}
