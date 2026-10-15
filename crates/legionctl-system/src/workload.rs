//! The workload sensor (note 04): loads `bpf/workload.bpf.c`, classifies each program that starts by its name (game,
//! build, vm), keeps the processes it classified until their end comes through the same ring buffer, and gives the
//! configsets' `workload` condition: a kind is active while one of its processes lives, and for a short time after
//! the last one ended (a build starts many short compilers one after another).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use aya::maps::{MapData, RingBuf};
use legionctl_machine::configset;
use legionctl_machine::sysroot::SysRoot;
use tokio::io::unix::AsyncFd;

/// `struct event` of the program: process id, ended, then the name.
const EVENT: usize = 24;
/// A kind stays active this long after its last process ended.
pub const LINGER: Duration = Duration::from_secs(5);

/// The kind of workload a program is, by its name (the kernel's `comm`: at most 15 bytes).
#[must_use]
pub fn classify(comm: &str) -> Option<&'static str> {
    const GAME: [&str; 7] = [
        // Steam starts each game under its reaper; the Steam client alone is not a game
        "reaper",
        "gamescope",
        "wine",
        "wine64",
        "wineserver",
        "wine-preloader",
        "wine64-preloade",
    ];
    const BUILD: [&str; 14] = [
        "cc1", "cc1plus", "rustc", "cargo", "clang", "clang++", "gcc", "g++", "ld", "ld.lld",
        "ld.mold", "mold", "make", "ninja",
    ];
    const VM: [&str; 3] = ["VirtualBoxVM", "VBoxHeadless", "VirtualBox"];
    // shortcut: the name only, not the cgroup (a Proton game runs as its .exe name under wine-preloader); add the
    // cgroup when a game is missed
    if GAME.contains(&comm) {
        Some("game")
    } else if BUILD.contains(&comm) {
        Some("build")
    } else if VM.contains(&comm) || comm.starts_with("qemu-system-") {
        Some("vm")
    } else {
        None
    }
}

/// The processes of each kind that run now.
pub struct Tracker {
    /// process id -> kind.
    live: BTreeMap<u32, &'static str>,
    /// When each kind last had a process.
    seen: BTreeMap<&'static str, Instant>,
}

/// The pinned program of this build, else load, attach and pin it; its ring buffer, ready for the event loop, and the
/// tracker with the matching processes that run already.
///
/// # Errors
/// No BPF permission, no BTF, the verifier refuses a program (its message is in the error), or a pin fails.
fn start(root: &SysRoot) -> Result<(AsyncFd<RingBuf<MapData>>, Tracker)> {
    let object = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/workload.bpf.o"));
    let [ring] = crate::pins::attach(
        root,
        "workload",
        object,
        &[
            ("exec", "sched_process_exec"),
            ("ended", "sched_process_exit"),
        ],
        &["events"],
    )?
    .try_into()
    .ok()
    .context("one map")?;
    let mut tracker = Tracker {
        live: BTreeMap::new(),
        seen: BTreeMap::new(),
    };
    // the programs that started before the sensor was attached: one look, after it was attached, so an end that comes
    // later is in the ring
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
            tracker.started(pid, comm.trim());
        }
    }
    Ok((AsyncFd::new(RingBuf::try_from(ring)?)?, tracker))
}

/// The sensor in the daemon's loop: its ring buffer and tracker (none when it cannot load), and the kinds active now.
pub struct Sensor {
    running: Option<(AsyncFd<RingBuf<MapData>>, Tracker)>,
    active: BTreeSet<String>,
    look: tokio::time::Interval,
}

impl Sensor {
    /// Start the sensor and write the active kinds; a sensor that cannot load leaves its reason instead (`doctor`), and
    /// the workload condition is never true.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn start(root: &SysRoot) -> Result<Sensor> {
        let mut sensor = Sensor {
            running: None,
            active: BTreeSet::new(),
            look: tokio::time::interval(Duration::from_secs(1)),
        };
        match start(root) {
            Ok((ring, mut tracker)) => {
                sensor.active = tracker.active(Instant::now());
                publish(root, Ok(&sensor.active))?;
                sensor.running = Some((ring, tracker));
            }
            Err(e) => {
                let why = format!("{e:#}");
                eprintln!("workload sensor: not running: {why}");
                publish(root, Err(&why))?;
            }
        }
        Ok(sensor)
    }

    /// Wait until the active kinds change, and write them; never, when the sensor does not run.
    ///
    /// # Errors
    /// The ring buffer fails, or the file cannot be written.
    pub async fn changed(&mut self, root: &SysRoot) -> Result<()> {
        let Some((ring, tracker)) = self.running.as_mut() else {
            return std::future::pending().await;
        };
        loop {
            tokio::select! {
                ready = ring.readable_mut() => {
                    let mut guard = ready?;
                    tracker.drain(guard.get_inner_mut());
                    guard.clear_ready();
                }
                _ = self.look.tick() => {
                    let now = tracker.active(Instant::now());
                    if now != self.active {
                        self.active = now;
                        publish(root, Ok(&self.active))?;
                        eprintln!("Workload: {}", self.active.iter().cloned().collect::<Vec<_>>().join(" "));
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Write the active kinds where every planner reads them ([`configset::WORKLOAD`]), or why the sensor does not run
/// ([`configset::WORKLOAD_ERROR`]); the other file goes.
///
/// # Errors
/// The file cannot be written.
fn publish(root: &SysRoot, active: Result<&BTreeSet<String>, &str>) -> Result<()> {
    let (write, remove, text) = match active {
        Ok(kinds) => (
            configset::WORKLOAD,
            configset::WORKLOAD_ERROR,
            kinds.iter().cloned().collect::<Vec<_>>().join("\n"),
        ),
        Err(why) => (
            configset::WORKLOAD_ERROR,
            configset::WORKLOAD,
            format!("{why}\n"),
        ),
    };
    let path = root.path(write);
    std::fs::create_dir_all(path.parent().context("no parent")?)?;
    std::fs::write(&path, text).with_context(|| format!("writing {write}"))?;
    match std::fs::remove_file(root.path(remove)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {remove}"))
        }
        _ => Ok(()),
    }
}

impl Tracker {
    /// Process `pid` started the program `comm` (a process that was a workload can start another program).
    fn started(&mut self, pid: u32, comm: &str) {
        match classify(comm) {
            Some(kind) => {
                self.live.insert(pid, kind);
                // a compiler can end before the next look: its kind lingers from now
                self.seen.insert(kind, Instant::now());
            }
            None => {
                self.live.remove(&pid);
            }
        }
    }

    /// The events in `ring` now, in order.
    pub fn drain(&mut self, ring: &mut RingBuf<MapData>) {
        while let Some(item) = ring.next() {
            let bytes: &[u8] = &item;
            if bytes.len() < EVENT {
                continue;
            }
            let word = |at: usize| {
                u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
            };
            let (pid, ended) = (word(0), word(4) != 0);
            if ended {
                self.live.remove(&pid);
            } else {
                let name = &bytes[8..EVENT];
                let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
                self.started(pid, &String::from_utf8_lossy(name));
            }
        }
    }

    /// The kinds active at `now`: with a process, or seen within [`LINGER`].
    pub fn active(&mut self, now: Instant) -> BTreeSet<String> {
        for kind in self.live.values() {
            self.seen.insert(kind, now);
        }
        self.seen.retain(|_, at| now.duration_since(*at) < LINGER);
        self.seen.keys().map(|k| (*k).to_string()).collect()
    }
}
