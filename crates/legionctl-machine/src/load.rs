//! System load as agents and humans should read it: one level plus the reasons behind it.
//!
//! Pressure stall information (PSI, `/proc/pressure/*`) says how much work is actually waiting on CPU, memory
//! or I/O, which tells "busy" apart from "many runnable threads" better than the load average alone.

use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::sysroot::SysRoot;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Pressure {
    /// Percent of the last 10 s in which at least one task stalled on this resource.
    pub some_avg10: f64,
    /// Percent of the last 10 s in which all non-idle tasks stalled (not reported for CPU on every kernel).
    pub full_avg10: Option<f64>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Idle,
    Normal,
    Busy,
    Overloaded,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Load {
    pub level: Level,
    pub reasons: Vec<String>,
    pub load1: f64,
    pub cpus: u32,
    pub cpu: Option<Pressure>,
    pub memory: Option<Pressure>,
    pub io: Option<Pressure>,
    pub mem_available_mib: u32,
    pub mem_total_mib: u32,
    /// With I/O pressure while no disk works: the cgroup that stalls most, and its pressure. Not counted in `level`.
    pub io_stall_source: Option<String>,
}

#[must_use]
pub fn parse_pressure(text: &str) -> Option<Pressure> {
    let (mut some, mut full) = (None, None);
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let kind = fields.next()?;
        let avg10: f64 = fields
            .find_map(|f| f.strip_prefix("avg10="))?
            .parse()
            .ok()?;
        match kind {
            "some" => some = Some(avg10),
            "full" => full = Some(avg10),
            _ => {}
        }
    }
    Some(Pressure {
        some_avg10: some?,
        full_avg10: full,
    })
}

fn meminfo_kib(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Thresholds are deliberately simple and documented in the reasons, so an agent can explain its choice.
#[must_use]
pub fn classify(
    load1: f64,
    cpus: u32,
    cpu: Option<&Pressure>,
    memory: Option<&Pressure>,
    io: Option<&Pressure>,
    disks_busy: f64,
    mem_available_fraction: f64,
) -> (Level, Vec<String>) {
    let cpus_f = f64::from(cpus.max(1));
    let cpu_some = cpu.map_or(0.0, |p| p.some_avg10);
    let mem_some = memory.map_or(0.0, |p| p.some_avg10);
    let mem_full = memory.and_then(|p| p.full_avg10).unwrap_or(0.0);
    let io_some = io.map_or(0.0, |p| p.some_avg10);
    let io_full = io.and_then(|p| p.full_avg10).unwrap_or(0.0);
    // Tasks can wait in iowait with the disks idle (seen here: a QEMU/KVM Windows VM in Docker, 77-91% "some" and
    // "full" in its cgroup, 0 disk I/O). I/O pressure counts only while a disk does I/O; the load average cannot
    // gate it, because it stays high for minutes after heavy CPU work ends.
    let disks_working = disks_busy >= DISK_WORKING;
    let working = load1 >= 0.10 * cpus_f;
    let heavily_working = load1 >= 0.25 * cpus_f;

    let overloaded = [
        (cpu_some >= 60.0, format!("CPU pressure {cpu_some:.0}%")),
        (
            heavily_working && mem_full >= 10.0,
            format!("memory stalls (full) {mem_full:.0}%"),
        ),
        (
            disks_working && io_full >= 30.0,
            format!("I/O stalls (full) {io_full:.0}%"),
        ),
        (
            mem_available_fraction < 0.05,
            format!(
                "only {:.0}% memory available",
                mem_available_fraction * 100.0
            ),
        ),
        (
            load1 >= 1.5 * cpus_f,
            format!("load {load1:.1} on {cpus} CPUs"),
        ),
    ];
    let busy = [
        (cpu_some >= 20.0, format!("CPU pressure {cpu_some:.0}%")),
        (
            working && mem_some >= 10.0,
            format!("memory pressure {mem_some:.0}%"),
        ),
        (
            disks_working && io_some >= 20.0,
            format!("I/O pressure {io_some:.0}%"),
        ),
        (
            mem_available_fraction < 0.15,
            format!(
                "only {:.0}% memory available",
                mem_available_fraction * 100.0
            ),
        ),
        (
            load1 >= 0.75 * cpus_f,
            format!("load {load1:.1} on {cpus} CPUs"),
        ),
    ];
    let hits = |rules: &[(bool, String)]| {
        rules
            .iter()
            .filter(|(hit, _)| *hit)
            .map(|(_, why)| why.clone())
            .collect::<Vec<_>>()
    };

    let reasons = hits(&overloaded);
    if !reasons.is_empty() {
        return (Level::Overloaded, reasons);
    }
    let reasons = hits(&busy);
    if !reasons.is_empty() {
        return (Level::Busy, reasons);
    }
    if load1 < 0.1 * cpus_f && cpu_some < 2.0 {
        return (Level::Idle, vec![]);
    }
    (Level::Normal, vec![])
}

/// One line an agent can act on: the level, why, and what to do about it.
#[must_use]
pub fn brief(load: &Load) -> String {
    let advice = match load.level {
        Level::Overloaded => " Avoid builds, test suites and parallel jobs until it settles.",
        Level::Busy => " Prefer light commands; limit parallelism (e.g. -j4).",
        _ => "",
    };
    let why = if load.reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", load.reasons.join(", "))
    };
    let stall = load.io_stall_source.as_ref().map_or(String::new(), |src| {
        format!(" I/O wait without disk I/O from {src}; not counted.")
    });
    format!(
        "System load: {:?}{why}; load {:.1} on {} CPUs, {} of {} free.{advice}{stall}",
        load.level,
        load.load1,
        load.cpus,
        crate::human_bytes(u64::from(load.mem_available_mib) << 20),
        crate::human_bytes(u64::from(load.mem_total_mib) << 20)
    )
}

/// Share of a short window in which a disk must do I/O before I/O pressure counts as load.
const DISK_WORKING: f64 = 0.10;
const CGROUPS: &str = "/sys/fs/cgroup";

/// The cgroup that explains the I/O stall, as `path (N%)`: from the root, go down to the child that stalls most while
/// it has at least 90% of its parent's pressure (values read moments apart differ a little). Container ids are cut
/// to 12 characters as `docker ps` shows them.
fn io_stall_source(root: &SysRoot) -> Option<String> {
    let pressure = |cgroup: &PathBuf| {
        root.read(cgroup.join("io.pressure"))
            .as_deref()
            .and_then(parse_pressure)
            .map(|p| p.some_avg10)
    };
    let mut path = PathBuf::from(CGROUPS);
    let mut some = pressure(&path)?;
    while let Some((child, child_some)) = root
        .children(&path, "")
        .into_iter()
        .filter_map(|c| pressure(&c).map(|p| (c, p)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, p)| *p >= 0.9 * some)
    {
        path = child;
        some = child_some;
    }
    let name: Vec<String> = path
        .strip_prefix(CGROUPS)
        .ok()?
        .iter()
        .map(|part| {
            let part = part.to_string_lossy();
            if part.len() == 64 && part.chars().all(|c| c.is_ascii_hexdigit()) {
                part[..12].to_string()
            } else {
                part.into_owned()
            }
        })
        .collect();
    Some(format!("{} ({some:.0}%)", name.join("/")))
}

/// Busiest whole disk's share of a short window, from `io_ticks` (ms spent doing I/O) in /proc/diskstats.
fn disks_busy(root: &SysRoot) -> f64 {
    let ticks = || -> Vec<(String, u64)> {
        root.read("/proc/diskstats")
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let name = *f.get(2)?;
                let whole = name.starts_with("nvme") && !name.contains('p')
                    || name.starts_with("sd") && !name.ends_with(|c: char| c.is_ascii_digit());
                whole.then(|| Some((name.to_string(), f.get(12)?.parse().ok()?)))?
            })
            .collect()
    };
    let window = Duration::from_millis(100);
    let before = ticks();
    std::thread::sleep(window);
    ticks()
        .iter()
        .filter_map(|(name, t)| {
            let (_, b) = before.iter().find(|(n, _)| n == name)?;
            Some(Duration::from_millis(t.saturating_sub(*b)).as_secs_f64() / window.as_secs_f64())
        })
        .fold(0.0, f64::max)
}

pub fn read_load(root: &SysRoot) -> Load {
    let load1 = root
        .read("/proc/loadavg")
        .and_then(|t| t.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0);
    let cpus = std::thread::available_parallelism()
        .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX));
    let cpu = root
        .read("/proc/pressure/cpu")
        .as_deref()
        .and_then(parse_pressure);
    let memory = root
        .read("/proc/pressure/memory")
        .as_deref()
        .and_then(parse_pressure);
    let io = root
        .read("/proc/pressure/io")
        .as_deref()
        .and_then(parse_pressure);
    let meminfo = root.read("/proc/meminfo").unwrap_or_default();
    let mib = |key| {
        meminfo_kib(&meminfo, key).map_or(0, |kib| u32::try_from(kib >> 10).unwrap_or(u32::MAX))
    };
    let total = mib("MemTotal");
    let available = mib("MemAvailable");
    let fraction = if total > 0 {
        f64::from(available) / f64::from(total)
    } else {
        1.0
    };
    let io_some = io.as_ref().map_or(0.0, |p| p.some_avg10);
    // sample the disks only when I/O pressure could matter, so the usual call stays instant
    let disks = if io_some >= 20.0 {
        disks_busy(root)
    } else {
        0.0
    };
    let (level, reasons) = classify(
        load1,
        cpus,
        cpu.as_ref(),
        memory.as_ref(),
        io.as_ref(),
        disks,
        fraction,
    );
    Load {
        level,
        reasons,
        load1,
        cpus,
        cpu,
        memory,
        io,
        mem_available_mib: available,
        mem_total_mib: total,
        io_stall_source: (io_some >= 20.0 && disks < DISK_WORKING)
            .then(|| io_stall_source(root))
            .flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iowait_without_disk_io_is_not_load_and_names_its_cgroup() {
        // measured on this machine: I/O some/full ~77% from a QEMU/KVM VM in Docker, disks 0 ms busy
        let phantom = Pressure {
            some_avg10: 77.0,
            full_avg10: Some(65.0),
        };
        let quiet = Pressure {
            some_avg10: 1.0,
            full_avg10: None,
        };
        let level =
            |load1, disks| classify(load1, 32, Some(&quiet), None, Some(&phantom), disks, 0.3);
        assert_eq!(level(2.3, 0.0).0, Level::Idle);
        // just after a full CPU + GPU load test: load average still 10, disks idle (2026-10-12 false "Overloaded")
        assert_eq!(level(10.0, 0.0).0, Level::Normal);
        // the same pressure while a disk really works is reported
        let (busy, why) = level(2.3, 0.6);
        assert_eq!(busy, Level::Overloaded);
        assert_eq!(why, vec!["I/O stalls (full) 65%".to_string()]);

        // the cgroup tree of that measurement: the container stalls, the rest of the system does not
        let dir = std::env::temp_dir().join(format!("legionctl-io-source-{}", std::process::id()));
        let id = "09bd9c9baa5f59137a2440fe5cc44399965edc74eb94f4a04559353cd7d099ef";
        for (cgroup, some) in [
            ("", 28.6),
            ("docker", 77.3),
            (&format!("docker/{id}") as &str, 77.3),
            ("user.slice", 0.4),
            ("user.slice/user-1000.slice", 0.4),
        ] {
            let path = dir.join("sys/fs/cgroup").join(cgroup);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("io.pressure"),
                format!("some avg10={some} avg60=0 avg300=0 total=0\nfull avg10={some} avg60=0 avg300=0 total=0\n"),
            )
            .unwrap();
        }
        let source = io_stall_source(&SysRoot::new(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(source.as_deref(), Some("docker/09bd9c9baa5f (77%)"));
    }
}
