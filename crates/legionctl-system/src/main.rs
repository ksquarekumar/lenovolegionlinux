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
//! | throttle trigger | a CPU's clock dropped or a thermal interrupt came (the eBPF power sensor); each second while throttled |
//!
//! While a tune runs (`Tune`), events wait: the sweep owns the firmware limits, and the policy applies again after.
//!
//! It serves `org.legionctl.System1` on the system bus: methods change the machine after polkit authorizes the
//! caller, and signals tell the session what changed (`Changed`), the firmware mode after a change (`ModeChanged`),
//! Fn lock (`FnLock`) and which Fn key was pressed (`Key`). After each event and each change, the state that other
//! tools follow (firmware mode, power source, workload, throttling, fan max mode) is compared with the last one
//! announced; each difference is a signal and a structured journal entry (note 16, "For other tools"); each start of a
//! throttle reason is counted with the workload kinds active then. Decisions come from `legionctl_machine`; this file
//! only wires event sources and the bus to it.

mod block;
mod exec;
mod folders;
mod pins;
mod power;
mod runq;
mod thermal;
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
use legionctl_machine::ledger::{FanKey, Key, Ledger};
use legionctl_machine::policy::{self, Machine, Options, Source, Step};
use legionctl_machine::react::{Event, react};
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::{
    battery, boot, cleanup, configset, doctor, fan, firmware, gpu, greeter, knobs, profile, sets,
    telemetry, tune, vfio, windows, winvm,
};
use tokio::io::unix::AsyncFd;
use tokio::io::{Interest, Ready};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;

const TICK: Duration = Duration::from_secs(30);
/// Written by `legionctl-system --safe` (legionctl-safe.service, after repeated failures); `doctor` reports it.
const SAFE_MODE: &str = "/run/legionctl/safe-mode";

/// Safe mode (note 13): the daemon failed again and again, so nothing enforces the policy. Hand the machine to the
/// firmware's balanced mode and leave the reason where `doctor` reads it. systemd gives the daemon's result
/// (`MONITOR_SERVICE_RESULT`): with `RestartMode=direct` only a result that is no restart reaches this unit, and only
/// `start-limit-hit` is repeated failure; another one (a stop that timed out during an upgrade) is not (todo-125).
fn safe(root: &SysRoot) -> Result<()> {
    if let Ok(result) = std::env::var("MONITOR_SERVICE_RESULT")
        && result != "start-limit-hit"
    {
        eprintln!(
            "safe mode: not entered: legionctl-system ended with result {result}, not repeated failures"
        );
        return Ok(());
    }
    let (profile, _) = profile::load(None)?;
    let choices = root
        .read(&profile.paths.platform_profile_choices)?
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
    let layers = configset::load(root, &profile, &ledger)?;
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
    on_machine(root, |m| policy::execute(root, m.ledger, &plan(m)?))
}

/// What the session shows about the firmware mode: the mode, whether it is the user's Fn+Q choice, and the power
/// source whose default it is otherwise.
#[derive(Debug, Clone, PartialEq)]
struct ModeState {
    mode: String,
    fn_q_choice: bool,
    source: &'static str,
}

/// The firmware mode now; `None` without `platform_profile`.
fn mode(root: &SysRoot) -> Result<Option<ModeState>> {
    let (profile, _) = profile::load(None)?;
    let Some(mode) = root.read(&profile.paths.platform_profile)? else {
        return Ok(None);
    };
    Ok(Some(ModeState {
        fn_q_choice: Ledger::open(root)?.get(Key::Override)?.as_deref() == Some(mode.as_str()),
        source: if root.read(&profile.paths.ac_adapter)?.as_deref() == Some("1") {
            "ac"
        } else {
            "battery"
        },
        mode,
    }))
}

/// The state other tools follow, as last announced.
#[derive(Debug, Clone, PartialEq)]
struct Observed {
    mode: Option<ModeState>,
    workload: BTreeSet<String>,
    /// Throttle reasons active now; `None` without the msr module.
    throttle: Option<Vec<String>>,
    fan_max: bool,
}

impl Observed {
    /// The workload kinds active, or `none`.
    fn kinds(&self) -> Vec<String> {
        if self.workload.is_empty() {
            vec!["none".into()]
        } else {
            self.workload.iter().cloned().collect()
        }
    }
}

fn observe(root: &SysRoot) -> Result<Observed> {
    Ok(Observed {
        mode: mode(root)?,
        workload: configset::workload(root)?,
        throttle: firmware::current_throttle_reasons(root)
            .map(|r| r.into_iter().map(String::from).collect()),
        fan_max: Ledger::open(root)?.fan(FanKey::Max)?.is_some(),
    })
}

/// What other tools heard last, and each throttle reason's starts per workload kind active then (reason -> kind ->
/// starts; `none`: no kind). The journal hears a throttle change only after it held (todo-126): `journaled` is the
/// throttle of its last entry, `changes` the throttle changes since then, `changed` the time of the last one.
struct Announced {
    last: Observed,
    episodes: BTreeMap<String, BTreeMap<String, u64>>,
    journaled: Option<Vec<String>>,
    changes: u64,
    changed: Instant,
}

/// How long a throttle change holds before the journal hears it; a start after no throttling is written at once.
const THROTTLE_HOLD: Duration = Duration::from_secs(5);

impl Announced {
    fn new(last: Observed) -> Announced {
        Announced {
            journaled: last.throttle.clone(),
            last,
            episodes: BTreeMap::new(),
            changes: 0,
            changed: Instant::now(),
        }
    }

    /// `now` is announced: the state before it, and each throttle reason that starts counts once for each workload
    /// kind active now.
    fn update(&mut self, now: &Observed) -> Observed {
        let before = std::mem::replace(&mut self.last, now.clone());
        if now.throttle != before.throttle {
            self.changes += 1;
            self.changed = Instant::now();
        }
        let was = before.throttle.clone().unwrap_or_default();
        for reason in now.throttle.iter().flatten().filter(|r| !was.contains(r)) {
            for kind in now.kinds() {
                *self
                    .episodes
                    .entry(reason.clone())
                    .or_default()
                    .entry(kind)
                    .or_default() += 1;
            }
        }
        before
    }

    /// The throttle reasons for a journal entry, and the changes since the last entry: when the throttle differs from
    /// the last entry and starts after none, or held for [`THROTTLE_HOLD`].
    fn throttle_entry(&mut self) -> Option<(Vec<String>, u64)> {
        let reasons = self.last.throttle.clone()?;
        if self.journaled.as_ref() == Some(&reasons) {
            return None;
        }
        let start = !reasons.is_empty() && self.journaled.as_ref().is_none_or(Vec::is_empty);
        if !start && self.changed.elapsed() < THROTTLE_HOLD {
            return None;
        }
        self.journaled = Some(reasons.clone());
        Some((reasons, std::mem::take(&mut self.changes)))
    }

    /// Look again each second: a throttle reason is active now (as last announced), or a journal entry waits for its
    /// change to hold.
    fn watching(announced: &Mutex<Announced>) -> bool {
        let a = announced.lock().unwrap_or_else(PoisonError::into_inner);
        a.last.throttle.as_ref().is_some_and(|r| !r.is_empty()) || a.journaled != a.last.throttle
    }
}

/// The journal's native socket.
const JOURNAL: &str = "/run/systemd/journal/socket";

/// One structured journal entry: `MESSAGE`, `LEGIONCTL_EVENT=<event>` and the event's fields as `LEGIONCTL_<KEY>`
/// (`journalctl -t legionctl-system LEGIONCTL_EVENT=<event>`). The values have no line breaks.
fn journal(root: &SysRoot, event: &str, message: &str, fields: &[(&str, String)]) {
    let entry: String = fields.iter().fold(
        format!(
            "MESSAGE={message}\nPRIORITY=6\nSYSLOG_IDENTIFIER=legionctl-system\nLEGIONCTL_EVENT={event}\n"
        ),
        |entry, (key, value)| entry + "LEGIONCTL_" + key + "=" + value + "\n",
    );
    let sent = std::os::unix::net::UnixDatagram::unbound()
        .and_then(|socket| socket.send_to(entry.as_bytes(), root.path(JOURNAL)));
    if let Err(e) = sent {
        eprintln!("{message} (not in the journal: {e})");
    }
}

fn sent(what: &str, result: zbus::Result<()>) {
    if let Err(e) = result {
        eprintln!("cannot send {what}: {e}");
    }
}

/// The power source of `o`.
fn source(o: &Observed) -> Option<&'static str> {
    o.mode.as_ref().map(|m| m.source)
}

/// The state to announce, after the policy of its power source: when the source differs from the last one announced
/// and the power source plan did not run just now (`source_planned`), it runs first. Without it, a look before the
/// late (or lost) udev event, or a source change during another event's plan, announces the old source's mode with the
/// new source. While a tune runs, no plan runs. A source that cannot be read now or was never announced is no change:
/// a plan for it would clear the user's Fn+Q choice.
async fn settled(
    root: &SysRoot,
    emitter: &SignalEmitter<'_>,
    announced: &Mutex<Announced>,
    tuning: &AtomicBool,
    source_planned: bool,
) -> Option<Observed> {
    let now = observed(root)?;
    let last = source(
        &announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last,
    );
    let changed = matches!((source(&now), last), (Some(a), Some(b)) if a != b);
    if source_planned || tuning.load(Ordering::SeqCst) || !changed {
        return Some(now);
    }
    plan(root, emitter, Event::PowerSource).await;
    observed(root)
}

/// The state that other tools follow cannot be read: a background loop error (user decision Q1), so a journal entry
/// and an open `doctor` finding until a read works again.
fn observe_failed(root: &SysRoot, e: &anyhow::Error) {
    let reason = format!("{e:#}");
    journal(
        root,
        "observe-error",
        &format!("the machine state cannot be read: {reason}"),
        &[("REASON", reason.replace('\n', " "))],
    );
    let marker = root.path(doctor::OBSERVE_ERROR);
    if let Err(e) = marker
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&marker, &reason))
    {
        eprintln!("cannot write {}: {e}", doctor::OBSERVE_ERROR);
    }
}

/// The state now; when it cannot be read, [`observe_failed`] reports why. A read that works closes the `doctor`
/// finding.
fn observed(root: &SysRoot) -> Option<Observed> {
    match observe(root) {
        Ok(now) => {
            match std::fs::remove_file(root.path(doctor::OBSERVE_ERROR)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    eprintln!("cannot remove {}: {e}", doctor::OBSERVE_ERROR);
                }
                _ => {}
            }
            Some(now)
        }
        Err(e) => {
            observe_failed(root, &e);
            None
        }
    }
}

/// Compare the state other tools follow with the last one announced; each difference is one signal and one journal
/// entry. A throttle reason that starts counts once for each workload kind active now (the correlation).
async fn announce(
    root: &SysRoot,
    emitter: &SignalEmitter<'_>,
    announced: &Mutex<Announced>,
    tuning: &AtomicBool,
    source_planned: bool,
) {
    let Some(now) = settled(root, emitter, announced, tuning, source_planned).await else {
        return;
    };
    let (before, throttle_entry) = {
        let mut a = announced.lock().unwrap_or_else(PoisonError::into_inner);
        (a.update(&now), a.throttle_entry())
    };
    if now.mode != before.mode
        && let Some(m) = &now.mode
    {
        let why = if m.fn_q_choice {
            "Fn+Q choice".to_string()
        } else {
            format!("default on {}", m.source)
        };
        journal(
            root,
            "mode",
            &format!("mode {} ({why})", m.mode),
            &[
                ("MODE", m.mode.clone()),
                ("FN_Q_CHOICE", u8::from(m.fn_q_choice).to_string()),
                ("SOURCE", m.source.into()),
            ],
        );
        sent(
            "ModeChanged",
            System1::mode_changed(emitter, &m.mode, m.fn_q_choice, m.source).await,
        );
    }
    if let Some(s) = source(&now).filter(|s| Some(*s) != source(&before)) {
        journal(
            root,
            "power-source",
            &format!("power source {s}"),
            &[("SOURCE", s.into())],
        );
        sent(
            "PowerSourceChanged",
            System1::power_source_changed(emitter, s).await,
        );
    }
    if now.workload != before.workload {
        let kinds: Vec<String> = now.workload.iter().cloned().collect();
        let shown = if kinds.is_empty() {
            "none".into()
        } else {
            kinds.join(",")
        };
        journal(
            root,
            "workload",
            &format!("workload {shown}"),
            &[("WORKLOAD", shown.clone())],
        );
        sent(
            "WorkloadChanged",
            System1::workload_changed(emitter, &kinds).await,
        );
    }
    if let Some((reasons, changes)) = throttle_entry {
        let message = if reasons.is_empty() {
            "throttling stopped".to_string()
        } else {
            format!("throttling: {}", reasons.join(", "))
        };
        journal(
            root,
            "throttle",
            &format!("{message} (changes since the last entry: {changes})"),
            &[
                ("REASONS", reasons.join(",")),
                ("WORKLOAD", now.kinds().join(",")),
                ("CHANGES", changes.to_string()),
            ],
        );
    }
    if now.throttle != before.throttle
        && let Some(reasons) = &now.throttle
    {
        sent(
            "ThrottleChanged",
            System1::throttle_changed(emitter, reasons).await,
        );
    }
    if now.fan_max != before.fan_max {
        let on = if now.fan_max { "on" } else { "off" };
        journal(
            root,
            "fan-mode",
            &format!("fan max mode {on}"),
            &[("FAN_MAX", on.into())],
        );
        sent(
            "FanModeChanged",
            System1::fan_mode_changed(emitter, now.fan_max).await,
        );
    }
}

/// One event: its plan, then the announcement of what other tools follow when it differs from the last one announced.
async fn handle(
    root: &SysRoot,
    emitter: &SignalEmitter<'_>,
    event: Event,
    announced: &Mutex<Announced>,
    tuning: &AtomicBool,
) {
    plan(root, emitter, event).await;
    announce(
        root,
        emitter,
        announced,
        tuning,
        event == Event::PowerSource,
    )
    .await;
}

/// Carry out the plan of `event`: log what happened (every event but the tick leaves a line) and tell the session if
/// anything changed. The plan runs beside the event loop, so a SIGTERM during a slow step ends the daemon without a wait
/// for the step or the steps after it (todo-125).
async fn plan(root: &SysRoot, emitter: &SignalEmitter<'_>, event: Event) {
    let owned = root.clone();
    let done = tokio::task::spawn_blocking(move || carry_out(&owned, |m| react(m, event)))
        .await
        .unwrap_or_else(|e| Err(e.into()));
    match done {
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

/// Carry out a plan that does not need the policy's machine (boot, greeter, disk): it stops at the first failure, so a
/// later step (a firmware entry) never follows a failed one (an unsigned loader).
fn boot_steps(root: &SysRoot, steps: &[Step]) -> Result<Vec<String>> {
    policy::execute(root, &Ledger::open(root)?, steps)
}

/// A mode that runs once and ends, instead of the daemon: `--safe` (the safe-mode unit), `--boot-rebuild` (the pacman
/// hook after a kernel or initramfs change), `--remove-check` (the pacman hook before the package's removal; it fails
/// to stop the removal), `--greeter-remove` (the package's removal: GDM back, so the computer keeps a login screen).
fn one_shot(root: &SysRoot) -> Option<Result<()>> {
    let print = |lines: Vec<String>| {
        for line in lines {
            println!("{line}");
        }
    };
    match std::env::args().nth(1).as_deref() {
        Some("--safe") => Some(safe(root)),
        Some("--boot-rebuild") => Some(boot_rebuild(root).map(print)),
        Some("--remove-check") => Some(remove_check(root)),
        Some("--greeter-remove") => Some(greeter::state(root).and_then(|state| {
            if state.installed {
                greeter::remove_plan(root)
                    .and_then(|plan| boot_steps(root, &plan))
                    .map(print)
            } else {
                Ok(())
            }
        })),
        _ => None,
    }
}

/// Refuse the package's removal while it leaves a bad state: the boot takeover first in the firmware with images that
/// no hook makes again, or greetd with a greeter that `pre_remove` cannot take back (GDM gone). Each reason names the
/// command to run first.
fn remove_check(root: &SysRoot) -> Result<()> {
    let mut reasons = Vec::new();
    let state = boot::state(root)?;
    if state.ours.is_some() || boot::default_kernel(root)?.is_some() {
        reasons.push(
            "the boot takeover is installed: without legionctl its kernel images are not made again after a kernel \
             update. Run `legionctl boot remove` first"
                .to_string(),
        );
    }
    if greeter::state(root)?.installed
        && let Err(e) = greeter::remove_plan(root)
    {
        reasons.push(format!(
            "legionctl's greeter is the login screen and the removal cannot put the old one back ({e:#}). Make \
             `legionctl greeter remove` work, and run it first"
        ));
    }
    anyhow::ensure!(
        reasons.is_empty(),
        "legionctl is not removed:\n- {}",
        reasons.join("\n- ")
    );
    Ok(())
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
        .get(Key::GpuServices)?
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
    match action {
        "disk-attach" | "disk-detach" => windows_disk(root, action == "disk-attach"),
        "gpu-none" => gpu_release(root, uid),
        "gpu-linux" => gpu_return(root),
        _ => boot_steps(root, &greeter::vm_plan(root, user, uid, action)?),
    }
}

/// The Windows disk's partitions to Linux (`attach`: their automounts start) or away (their automounts and mounts
/// stop); refused while the Windows VM has the disk.
fn windows_disk(root: &SysRoot, attach: bool) -> Result<Vec<String>> {
    anyhow::ensure!(
        Ledger::open(root)?.get(Key::WindowsVm)?.is_none(),
        "the Windows VM has the disk now"
    );
    boot_steps(root, &winvm::disk_plan(root, attach)?)
}

/// Keep the selection now (layer states and holds) as the set `name`; `boot`: each kernel's image gets an entry for
/// it, which needs the boot takeover. The images are made again when the set has, or had, boot entries.
fn save_set(root: &SysRoot, name: &str, boot: bool) -> Result<Vec<String>> {
    sets::check_name(name)?;
    if boot {
        boot::takeover(&boot::state(root)?)?;
    }
    let ledger = Ledger::open(root)?;
    let had_boot = ledger.sets()?.get(name).is_some_and(|s| s.boot);
    let selection = sets::Selection {
        boot,
        ..ledger.selection()?
    };
    ledger.save_set(name, Some(&selection))?;
    let mut lines = vec![format!(
        "set {name}: {} layer states, {} holds{}",
        selection.layers.len(),
        selection.holds.len(),
        if boot { ", with boot entries" } else { "" }
    )];
    if boot || had_boot {
        lines.extend(boot_rebuild(root)?);
    }
    Ok(lines)
}

/// Remove the set `name`, and its boot entries.
fn remove_set(root: &SysRoot, name: &str) -> Result<Vec<String>> {
    let ledger = Ledger::open(root)?;
    let set = ledger
        .sets()?
        .remove(name)
        .with_context(|| format!("no saved set {name}"))?;
    ledger.save_set(name, None)?;
    let mut lines = vec![format!("set {name}: removed")];
    if set.boot {
        lines.extend(boot_rebuild(root)?);
    }
    Ok(lines)
}

/// Switch to the set `name` as one change: the holds it does not have are released, the layer states become its own,
/// its holds are written, and the layers apply. The selection before is kept as `previous` (the undo).
fn switch_set(root: &SysRoot, name: &str) -> Result<Vec<String>> {
    let ledger = Ledger::open(root)?;
    let to = ledger.sets()?.remove(name).with_context(|| {
        format!("no saved set {name} (legionctl resources list shows the sets)")
    })?;
    // its holds are checked before anything changes
    on_machine(root, |m| knobs::hold_plan(m, &to.holds))?;
    let now = ledger.selection()?;
    ledger.save_set(sets::PREVIOUS, Some(&now))?;
    let mut lines = sets::changes(&now, &to);
    lines.extend(carry_out(root, |m| {
        Ok(m.ledger
            .holds()?
            .into_keys()
            .filter(|path| !to.holds.contains_key(path))
            .map(|path| Step::Hold { path, value: None })
            .collect())
    })?);
    ledger.set_layers(&to)?;
    lines.extend(carry_out(root, |m| knobs::hold_plan(m, &to.holds))?);
    lines.extend(carry_out(root, |m| react(m, Event::Reload))?);
    Ok(lines)
}

/// The set of this start's boot entry (`legionctl.set=<name>`, todo-85), for this start only: the selection before
/// is kept as `before-boot` (once, so a second start through the menu keeps the first), and a start without the
/// parameter goes back to it.
fn boot_set(root: &SysRoot) -> Result<Vec<String>> {
    let ledger = Ledger::open(root)?;
    let line = root.read("/proc/cmdline")?.unwrap_or_default();
    let saved = ledger.sets()?;
    match sets::chosen(&line) {
        Some(name) => {
            anyhow::ensure!(
                saved.contains_key(name),
                "no saved set {name}: the selection stays"
            );
            if !saved.contains_key(sets::BEFORE_BOOT) {
                ledger.save_set(sets::BEFORE_BOOT, Some(&ledger.selection()?))?;
            }
            switch_set(root, name)
        }
        None if saved.contains_key(sets::BEFORE_BOOT) => {
            let lines = switch_set(root, sets::BEFORE_BOOT)?;
            ledger.save_set(sets::BEFORE_BOOT, None)?;
            Ok(lines)
        }
        None => Ok(Vec::new()),
    }
}

/// [`boot_set`] before the first apply; a failure leaves the selection as it is.
fn start_set(root: &SysRoot) {
    match boot_set(root) {
        Ok(lines) => {
            for line in lines {
                eprintln!("Start: set: {line}");
            }
        }
        Err(e) => eprintln!("Start: the boot entry's set: {e:#}"),
    }
}

/// Every kernel's UKI again (the pacman hook); nothing when legionctl does not own the boot. Each warning (a kernel
/// whose image is not made again) also goes to the journal: the hook's output is gone after pacman ends.
/// The plan of the boot action `action` (for `install`: before its `BootNext` step, which needs the new entry): for
/// its run and its dry run (`Plan`).
fn boot_plan(root: &SysRoot, action: &str) -> Result<Vec<Step>> {
    if action == "install" {
        return boot::install_plan(root);
    }
    let state = boot::state(root)?;
    match action {
        "confirm" => boot::confirm_plan(&state),
        "rebuild" if state.ours.is_none() => Ok(vec![Step::Info(
            "no legionctl boot entry: nothing to rebuild".into(),
        )]),
        "rebuild" => boot::rebuild_plan(root, &state),
        "remove" => boot::remove_plan(root, &state),
        other => {
            anyhow::bail!("boot action must be install, confirm, rebuild or remove, not {other}")
        }
    }
}

/// The plan of the greeter action `action`, for its run and its dry run (`Plan`).
fn greeter_plan(root: &SysRoot, action: &str) -> Result<Vec<Step>> {
    match action {
        "install" => greeter::install_plan(root),
        "remove" => greeter::remove_plan(root),
        other => anyhow::bail!("greeter action must be install or remove, not {other}"),
    }
}

fn boot_rebuild(root: &SysRoot) -> Result<Vec<String>> {
    let plan = boot_plan(root, "rebuild")?;
    for step in &plan {
        if let Step::Warn(text) = step {
            let logged = std::process::Command::new("logger")
                .args(["-t", "legionctl-system", "-p", "warning", "--", text])
                .status();
            if !logged.is_ok_and(|s| s.success()) {
                eprintln!("warning: not in the journal: {text}");
            }
        }
    }
    boot_steps(root, &plan)
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
    /// The eBPF sensors that samples read.
    meters: Arc<Meters>,
    /// Who gets the session activity: uid -> its connection (`WatchSessions`; the newest call of a user wins).
    watchers: Arc<Mutex<BTreeMap<u32, String>>>,
    /// The hash of the interface this build serves (`Version`).
    version: String,
    /// What other tools heard last (shared with the event loop).
    announced: Arc<Mutex<Announced>>,
}

impl System1 {
    /// Tell the session what changed, then announce what other tools follow.
    async fn tell(
        &self,
        emitter: &SignalEmitter<'_>,
        cause: &str,
        lines: &[String],
    ) -> zbus::Result<()> {
        Self::changed(emitter, cause, lines).await?;
        announce(&self.root, emitter, &self.announced, &self.tuning, false).await;
        Ok(())
    }

    /// A fan or battery change: a refused plan is the caller's error; the session hears what changed.
    async fn machine_change(
        &self,
        emitter: &SignalEmitter<'_>,
        cause: &str,
        plan: impl FnOnce(&Machine) -> Result<Vec<Step>>,
    ) -> zbus::fdo::Result<Vec<String>> {
        let lines = carry_out(&self.root, plan)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{e:#}")))?;
        self.tell(emitter, cause, &lines).await?;
        Ok(lines)
    }

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
        self.tell(emitter, cause, &lines).await?;
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

/// The eBPF sensors that samples read. One that cannot load is its reason (Sample, doctor), and the daemon runs
/// without it (note 04).
struct Meters {
    runq: Result<runq::Runq, String>,
    power: Result<power::Power, String>,
    block: Result<block::Block, String>,
    thermal: Result<thermal::Thermal, String>,
    /// Why the session activity sensor does not run.
    exec: Option<String>,
}

/// What `sensor` (`name`) reads now, or why it cannot, in `errors`.
fn read<S, T>(
    name: &str,
    sensor: &Result<S, String>,
    read: impl FnOnce(&S) -> Result<T>,
    errors: &mut BTreeMap<String, String>,
) -> Option<T> {
    match sensor.as_ref().map(read) {
        Ok(Ok(value)) => Some(value),
        Ok(Err(e)) => {
            errors.insert(name.into(), format!("reading its map: {e:#}"));
            None
        }
        Err(e) => {
            errors.insert(name.into(), e.clone());
            None
        }
    }
}

/// The readings only root makes, with the eBPF sensors' (or why each does not run) and the throttle starts per
/// workload kind.
fn root_part(
    root: &SysRoot,
    meters: &Meters,
    announced: &Mutex<Announced>,
) -> telemetry::RootSample {
    let mut part = telemetry::root_sample(root);
    part.throttle_episodes = announced
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .episodes
        .clone();
    let errors = &mut part.sensor_errors;
    part.runq = read("run-queue", &meters.runq, runq::Runq::per_cpu, errors).and_then(|per_cpu| {
        telemetry::by_class(root, &per_cpu)
            .map_err(|e| errors.insert("run-queue".into(), format!("{e:#}")))
            .ok()
    });
    part.processes = read("power", &meters.power, power::Power::processes, errors);
    part.io = read("block I/O", &meters.block, block::Block::per_disk, errors);
    part.thermal_trips = read(
        "thermal",
        &meters.thermal,
        thermal::Thermal::per_zone,
        errors,
    );
    if let Some(e) = &meters.exec {
        errors.insert("session activity".into(), e.clone());
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
            self.tell(&emitter, "ApplyPower", &lines).await?;
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
            self.tell(&emitter, "Reload", &lines).await?;
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
        self.tell(&emitter, "Hold", &lines).await?;
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
                .holds()?
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
        self.tell(&emitter, "Release", &lines).await?;
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
        self.tell(&emitter, "Passthrough", &lines).await?;
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
        if ledger
            .get(Key::WindowsVm)
            .map_err(|e| failed(&e))?
            .is_some()
        {
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
        self.tell(&emitter, "WindowsVmBegin", &lines).await?;
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
        let line = winvm::set_autostart_rule(&self.root, &user, on).map_err(|e| failed(&e))?;
        let lines = vec![line];
        self.tell(&emitter, "SetWindowsAutostart", &lines).await?;
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
        let Some(kept) = ledger.get(Key::WindowsVm).map_err(|e| failed(&e))? else {
            return Ok(vec!["no Windows VM ran".into()]);
        };
        let record: winvm::Record =
            serde_json::from_str(&kept).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let lines = winvm::end(&self.root, &record);
        ledger.set(Key::WindowsVm, None).map_err(|e| failed(&e))?;
        self.tell(&emitter, "WindowsVmEnd", &lines).await?;
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
        self.tell(&emitter, "GpuRelease", &lines).await?;
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
        self.tell(&emitter, "GpuReturn", &lines).await?;
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
                    let mut lines = boot_steps(&root, &boot_plan(&root, "install")?)?;
                    let once = boot::try_once(&boot::state(&root)?)?;
                    lines.extend(boot_steps(&root, &[once])?);
                    lines.push(
                        "the next start tries legionctl once; after it, run `legionctl boot confirm`".into(),
                    );
                    Ok(lines)
                }
                "rebuild" => boot_rebuild(&root),
                _ => boot_steps(&root, &boot_plan(&root, &action)?),
            }
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(|e| failed(&e))?;
        self.tell(&emitter, "Boot", &lines).await?;
        Ok(lines)
    }

    /// The kernels of the boot takeover (todo-84), for the kernel `name`: `default`, `try` (then the restart),
    /// `cmdline` (`line` as its command line), `undo` (its line before the last change) or `install` (the packages
    /// that `kernel build` made in the folder `line`).
    async fn kernel(
        &self,
        action: String,
        name: String,
        line: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.boot").await?;
        let root = self.root.clone();
        let (lines, restart) =
            tokio::task::spawn_blocking(move || -> Result<(Vec<String>, Option<Step>)> {
                let state = boot::state(&root)?;
                let mut plan = match action.as_str() {
                    "default" => boot::default_plan(&state, &name)?,
                    "try" => boot::try_plan(&state, &name)?,
                    "cmdline" => boot::cmdline_plan(&root, &state, &name, &line)?,
                    "undo" => boot::undo_plan(&root, &state, &name)?,
                    "install" => boot::package_plan(&root, &name, std::path::Path::new(&line))?,
                    other => anyhow::bail!(
                        "kernel action must be default, try, cmdline, undo or install, not {other}"
                    ),
                };
                // the restart only after each step before it worked
                let restart = if action == "try" { plan.pop() } else { None };
                Ok((boot_steps(&root, &plan)?, restart))
            })
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            .map_err(|e| failed(&e))?;
        // the signal goes out before the restart takes the bus down
        self.tell(&emitter, "Kernel", &lines).await?;
        let Some(restart) = restart else {
            return Ok(lines);
        };
        let ledger = Ledger::open(&self.root).map_err(|e| failed(&e))?;
        let restarted = policy::execute(&self.root, &ledger, &[restart]).map_err(|e| failed(&e))?;
        Ok([lines, restarted].concat())
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
            boot_steps(&root, &greeter_plan(&root, &action)?)
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(|e| failed(&e))?;
        self.tell(&emitter, "Greeter", &lines).await?;
        Ok(lines)
    }

    /// The plan of a `boot` or `greeter` action as lines, with no change: the dry run (`boot install --dry-run`), and
    /// the greeter's refusal before the polkit dialog. The daemon plans as root, so a root-only file (the MOK key in
    /// /etc/refind.d/keys) is seen, and the user sees what the run would do. Read-only, so polkit is not asked.
    async fn plan(&self, kind: String, action: String) -> zbus::fdo::Result<Vec<String>> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let mut lines = policy::grouped(&match kind.as_str() {
                "boot" => boot_plan(&root, &action)?,
                "greeter" => greeter_plan(&root, &action)?,
                other => anyhow::bail!("plan kind must be boot or greeter, not {other}"),
            });
            if kind == "boot" && action == "install" {
                lines.push("then the next start tries legionctl's entry once (BootNext)".into());
            }
            Ok(lines)
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(|e| failed(&e))
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
        self.tell(&emitter, "GreeterAct", &lines).await?;
        Ok(lines)
    }

    /// The Windows disk's partitions to Linux (`attach`) or away from it, after login (the greeter's `disk-attach`
    /// and `disk-detach` before it); refused while the Windows VM runs.
    async fn windows_disk(
        &self,
        attach: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.windows.vm").await?;
        let root = self.root.clone();
        let lines = tokio::task::spawn_blocking(move || windows_disk(&root, attach))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            .map_err(|e| failed(&e))?;
        self.tell(&emitter, "WindowsDisk", &lines).await?;
        Ok(lines)
    }

    /// Saved sets (todo-85): `save` the layer states and holds of now as `name` (`boot`: with an entry in each
    /// kernel's image), `switch` to `name` as one change, or `remove` it. polkit `org.legionctl.rules`, and
    /// `org.legionctl.boot` when the images change.
    async fn sets(
        &self,
        action: String,
        name: String,
        boot: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.rules").await?;
        let had_boot = Ledger::open(&self.root)
            .and_then(|l| l.sets())
            .map_err(|e| failed(&e))?
            .get(&name)
            .is_some_and(|s| s.boot);
        if (action == "save" && boot) || (action != "switch" && had_boot) {
            authorize(conn, &header, "org.legionctl.boot").await?;
        }
        if self.tuning.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed(
                "a tune runs; change the sets after it".into(),
            ));
        }
        let root = self.root.clone();
        let switch = action == "switch";
        let lines = tokio::task::spawn_blocking(move || match action.as_str() {
            "save" => save_set(&root, &name, boot),
            "switch" => switch_set(&root, &name),
            "remove" => remove_set(&root, &name),
            other => anyhow::bail!("sets action must be save, switch or remove, not {other}"),
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{e:#}")))?;
        self.tell(&emitter, if switch { "SwitchSet" } else { "Sets" }, &lines)
            .await?;
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
        self.tell(&emitter, "PassthroughEnd", &lines).await?;
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
            self.tell(&emitter, "SetPrimaryGpu", &lines).await?;
        }
        Ok(lines)
    }

    /// The NVIDIA GPU's runtime power management: with `sleep` it goes to D3cold when no program uses it (refused
    /// while one does), else it stays powered.
    async fn set_gpu_power(
        &self,
        sleep: bool,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        authorize(conn, &header, "org.legionctl.gpu.power").await?;
        let lines = carry_out(&self.root, |m| gpu::set_nvidia_power(m, sleep))
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{e:#}")))?;
        self.tell(&emitter, "SetGpuPower", &lines).await?;
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
            self.tell(&emitter, "Cleanup", &lines).await?;
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
        let (meters, announced) = (self.meters.clone(), self.announced.clone());
        let report = tokio::task::spawn_blocking(move || {
            let report = tune::run(
                &root,
                &spec,
                &|argv, cancel| tune::run_workload(argv, user, cancel),
                &|| {
                    telemetry::sample(&root, Duration::from_secs(1), &|| {
                        root_part(&root, &meters, &announced)
                    })
                },
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
        self.tell(&emitter, "Tune", &lines).await?;
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
        let mut lines = policy::execute(&self.root, &ledger, boot_next)
            .map_err(|e| zbus::fdo::Error::Failed(format!("not rebooting: {e:#}")))?;
        lines.push(format!(
            "BootNext set: {}",
            boot_next
                .iter()
                .filter_map(policy::describe)
                .collect::<String>()
        ));
        // the signal goes out before the reboot takes the bus down
        self.tell(&emitter, "RebootWindows", &lines).await?;
        lines.extend(policy::execute(&self.root, &ledger, reboot).map_err(|e| failed(&e))?);
        Ok(lines)
    }

    /// The fan controls as JSON (`fan::FanState`): the firmware's fan table, the user's curve, fan max mode.
    /// Read-only, so polkit is not asked.
    async fn fan_status(&self) -> zbus::fdo::Result<String> {
        let root = self.root.clone();
        // off the bus thread: the firmware call waits while a sample's calls run
        let state = tokio::task::spawn_blocking(move || on_machine(&root, fan::state))
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
        self.machine_change(&emitter, "SetFanCurve", |m| {
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
        self.machine_change(&emitter, "ResetFanCurve", |m| {
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
        self.machine_change(&emitter, "SetBatteryMode", |m| {
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
        self.machine_change(&emitter, "SetUsbCharging", |m| {
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
        self.machine_change(&emitter, "SetFanMax", |m| fan::max(m, on))
            .await
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
        self.machine_change(&emitter, "SetFanFullSpeed", |m| {
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
        self.machine_change(&emitter, "SetFanLock", |m| fan::lock(m, on))
            .await
    }

    /// The readings only root can make (fan RPM, throttle reasons from the MSR, the eBPF sensors), as JSON
    /// (`telemetry::RootSample`). Read-only, so polkit is not asked.
    async fn sample(&self) -> zbus::fdo::Result<String> {
        let (root, meters, announced) = (
            self.root.clone(),
            self.meters.clone(),
            self.announced.clone(),
        );
        let sample = tokio::task::spawn_blocking(move || root_part(&root, &meters, &announced))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        serde_json::to_string(&sample).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// The devices that can go to a VM (`vfio::Candidate` list as JSON): read-only, so polkit is not asked.
    async fn passthroughs(&self) -> zbus::fdo::Result<String> {
        let root = self.root.clone();
        let list = tokio::task::spawn_blocking(move || vfio::candidates(&root))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            .map_err(|e| failed(&e))?;
        serde_json::to_string(&list).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Send the starts of programs (`SessionExec`) and the files opened (`SessionOpen`) in the caller's own agent
    /// sessions to the caller's connection, from now on. Only the caller's sessions, and only to it: arguments and
    /// paths can hold secrets.
    async fn watch_sessions(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        if let Some(e) = &self.meters.exec {
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

    /// A process in agent session `session` opened `path` (the first open of each path); sent only to the session
    /// owner's watcher.
    #[zbus(signal)]
    async fn session_open(
        emitter: &SignalEmitter<'_>,
        session: &str,
        pid: u32,
        path: &str,
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

    /// The power source changed: `ac` or `battery`.
    #[zbus(signal)]
    async fn power_source_changed(emitter: &SignalEmitter<'_>, source: &str) -> zbus::Result<()>;

    /// The workloads that run changed (`game`, `build`, `vm`; empty: none), from the eBPF workload sensor.
    #[zbus(signal)]
    async fn workload_changed(emitter: &SignalEmitter<'_>, kinds: &[String]) -> zbus::Result<()>;

    /// The throttle reasons active now changed (the MSR, read at each event, at each eBPF trigger and each second
    /// while throttled; empty: throttling stopped).
    #[zbus(signal)]
    async fn throttle_changed(emitter: &SignalEmitter<'_>, reasons: &[String]) -> zbus::Result<()>;

    /// Fan max mode was turned on or off.
    #[zbus(signal)]
    async fn fan_mode_changed(emitter: &SignalEmitter<'_>, max: bool) -> zbus::Result<()>;
}

/// Read the Fn-key device on a thread (blocking reads) and send one message per debounced Fn+R press. The device is
/// looked up by name and opened again when it goes away.
fn watch_fn_keys(root: SysRoot, presses: tokio::sync::mpsc::UnboundedSender<()>) {
    std::thread::spawn(move || {
        let mut debounce = Debounce::default();
        loop {
            let device = match keys::find_device(&root, FN_KEYS_DEVICE) {
                Ok(Some(device)) => device,
                Ok(None) => {
                    std::thread::sleep(REOPEN);
                    continue;
                }
                Err(e) => {
                    eprintln!("Fn keys: cannot look for {FN_KEYS_DEVICE}: {e:#}");
                    std::thread::sleep(REOPEN);
                    continue;
                }
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

/// Opened paths kept per session to send only the first open of each; a session past this starts again.
const SEEN: usize = 4096;

/// The session activity sensor's loop: each second it watches the scopes of the users who asked; each program start
/// and the first open of each file go to its session owner's watcher.
async fn session_activity(
    bus: zbus::Connection,
    watchers: Arc<Mutex<BTreeMap<u32, String>>>,
    mut rings: exec::Rings,
    mut watch: exec::Watch,
) {
    // new and ended session scopes reach the sensor within a second
    let mut scan = tokio::time::interval(Duration::from_secs(1));
    // session -> the paths sent; shortcut: a session that opens more than SEEN files sends some again; keep a
    // bounded LRU when that floods the event log
    let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    loop {
        let activity = tokio::select! {
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
                seen.retain(|session, _| watch.watches(session));
                continue;
            }
            ready = rings.execs.readable_mut() => {
                let Ok(mut guard) = ready else {
                    eprintln!("exec sensor: the ring buffer failed; stopped");
                    return;
                };
                let execs = watch.drain(guard.get_inner_mut(), false);
                guard.clear_ready();
                execs
            }
            ready = rings.opens.readable_mut() => {
                let Ok(mut guard) = ready else {
                    eprintln!("exec sensor: the ring buffer of opened files failed; stopped");
                    return;
                };
                let opens = watch.drain(guard.get_inner_mut(), true);
                guard.clear_ready();
                opens
            }
        };
        for a in activity {
            if let exec::Did::Open(path) = &a.did {
                let paths = seen.entry(a.session.clone()).or_default();
                if paths.len() >= SEEN {
                    paths.clear();
                }
                if !paths.insert(path.clone()) {
                    continue;
                }
            }
            send_activity(&bus, &watchers, a).await;
        }
    }
}

/// Send one thing a session did to its owner's watcher alone (a unicast signal).
async fn send_activity(
    bus: &zbus::Connection,
    watchers: &Mutex<BTreeMap<u32, String>>,
    a: exec::Activity,
) {
    let Some(name) = watchers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&a.uid)
        .cloned()
    else {
        return;
    };
    let sent = async {
        let to = SignalEmitter::new(bus, OBJECT_PATH)?
            .set_destination(zbus::names::BusName::try_from(name.as_str())?);
        match &a.did {
            exec::Did::Exec(argv) => System1::session_exec(&to, &a.session, a.pid, argv).await,
            exec::Did::Open(path) => System1::session_open(&to, &a.session, a.pid, path).await,
        }
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

/// Start the eBPF sensors; each one that cannot load says why, and the daemon runs without it (note 04). Also the
/// session activity's rings and watch, and the power sensor's triggers.
fn sensors(root: &SysRoot) -> (Meters, Option<(exec::Rings, exec::Watch)>, power::Triggers) {
    fn started<T>(name: &str, sensor: Result<T>) -> Result<T, String> {
        sensor.map_err(|e| {
            let why = format!("{e:#}");
            eprintln!("{name} sensor: not running: {why}");
            why
        })
    }
    let (power, triggers) = match started("power", power::Power::start(root)) {
        Ok((power, triggers)) => (Ok(power), Some(triggers)),
        Err(e) => (Err(e), None),
    };
    let (execs, exec) = match started("exec", exec::start(root)) {
        Ok(execs) => (Some(execs), None),
        Err(e) => (None, Some(e)),
    };
    let meters = Meters {
        runq: started("runq", runq::Runq::start(root)),
        power,
        block: started("block I/O", block::Block::start(root)),
        thermal: started("thermal", thermal::Thermal::start(root)),
        exec,
    };
    (meters, execs, power::Triggers::new(triggers))
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

fn main() -> Result<()> {
    let root = SysRoot::from_env();
    if let Some(done) = one_shot(&root) {
        return done;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let ended = runtime.block_on(async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            ended = run(root.clone()) => ended,
            _ = term.recv() => {
                notify(sd_notify::NotifyState::Stopping);
                // a stop ends the sensors; a failure keeps them for the next start (D7)
                pins::unpin(&root)
            }
        }
    });
    // a stop waits for no step that runs (an event's plan, a method's tune step or package unit): systemd sent its
    // processes the same SIGTERM, and no step starts after it (todo-125)
    runtime.shutdown_background();
    ended
}

/// The daemon until an event source fails; SIGTERM drops it at its next wait ([`main`]).
async fn run(root: SysRoot) -> Result<()> {
    let tuning = Arc::new(AtomicBool::new(false));
    let (meters, execs, mut triggers) = sensors(&root);
    let watchers = Arc::new(Mutex::new(BTreeMap::new()));
    let monitor = udev_monitor()?;
    // announce later changes only; the session asks for the current state when it starts
    // nothing announced yet when the state cannot be read: the first read that works announces each state
    let first = observed(&root).unwrap_or(Observed {
        mode: None,
        workload: BTreeSet::new(),
        throttle: None,
        fan_max: false,
    });
    let announced = Arc::new(Mutex::new(Announced::new(first)));
    let bus = serve(System1 {
        root: root.clone(),
        tuning: tuning.clone(),
        meters: Arc::new(meters),
        watchers: watchers.clone(),
        version: legionctl_machine::interface_version(),
        announced: announced.clone(),
    })
    .await?;
    let emitter = SignalEmitter::new(&bus, OBJECT_PATH)?;
    // the exec sensor runs beside the loop below; if it stops after it started, the daemon stops with an error and
    // systemd starts it again (DESIGN/02)
    let mut activity = execs
        .map(|(rings, watch)| tokio::spawn(session_activity(bus.clone(), watchers, rings, watch)));
    let mut sleep = Login1Proxy::new(&bus)
        .await?
        .receive_prepare_for_sleep()
        .await?;
    let (press_tx, mut presses) = tokio::sync::mpsc::unbounded_channel();
    watch_fn_keys(root.clone(), press_tx);
    let mut tick = tokio::time::interval(TICK);
    tick.tick().await;
    let mut folders = folders::Folders::watch(&root)?;
    let mut workloads = workload::Sensor::start(&root)?;

    // a missing sensor is reported, and the daemon runs without it (note 01)
    let fn_lock = fn_lock_watch(&root)
        .inspect_err(|e| eprintln!("FnLock: not watched: {e:#}"))
        .ok();
    start_set(&root);
    handle(&root, &emitter, Event::Start, &announced, &tuning).await;
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
                    handle(&root, &emitter, event, &announced, &tuning).await;
                }
            }
            Some(signal) = sleep.next() => {
                if !signal.args()?.start && !tuning.load(Ordering::SeqCst) {
                    handle(&root, &emitter, Event::Resume, &announced, &tuning).await;
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
                handle(&root, &emitter, Event::Tick, &announced, &tuning).await;
            },
            changed = folders.changed() => {
                changed?;
                if !tuning.load(Ordering::SeqCst) {
                    handle(&root, &emitter, Event::Reload, &announced, &tuning).await;
                }
            }
            () = triggers.next(Announced::watching(&announced)) => announce(&root, &emitter, &announced, &tuning, false).await,
            changed = workloads.changed(&root) => {
                changed?;
                if !tuning.load(Ordering::SeqCst) {
                    handle(&root, &emitter, Event::Workload, &announced, &tuning).await;
                }
            }
        }
    }
}
