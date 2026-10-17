//! System load as agents and humans should read it: one level plus the reasons behind it.
//!
//! Pressure stall information (PSI, `/proc/pressure/*`) says how much work is actually waiting on CPU, memory
//! or I/O, which tells "busy" apart from "many runnable threads" better than the load average alone.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
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
    pub mem_available_bytes: u64,
    pub mem_total_bytes: u64,
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

/// A `/proc/meminfo` value in KiB.
#[must_use]
pub fn meminfo_kib(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// When load counts as busy or overloaded. The values come from the config (`load`); see its defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    /// Share of a short window (%) in which a disk must do I/O before I/O pressure counts as load.
    pub disk_busy_percent: f64,
    pub busy: Limits,
    pub overloaded: Limits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// CPU pressure, "some" (%).
    pub cpu_pressure: f64,
    /// Memory pressure (%): "some" for busy, "full" for overloaded; counts only while the CPUs work.
    pub memory_pressure: f64,
    /// I/O pressure (%): "some" for busy, "full" for overloaded; counts only while a disk works.
    pub io_pressure: f64,
    /// Available memory below this share of the total (%).
    pub memory_available_percent: f64,
    /// Load average per CPU.
    pub load_per_cpu: f64,
}

/// One reading of the inputs the classifier uses.
#[derive(Debug, Clone)]
pub struct Sample {
    pub load1: f64,
    pub cpus: u32,
    pub cpu: Option<Pressure>,
    pub memory: Option<Pressure>,
    pub io: Option<Pressure>,
    /// Busiest whole disk's share of a short window (0-1).
    pub disks_busy: f64,
    pub mem_available_fraction: f64,
}

/// The level and the reasons, each reason naming the threshold it crossed so an agent can explain its choice.
#[must_use]
pub fn classify(s: &Sample, t: &Thresholds) -> (Level, Vec<String>) {
    let cpus = s.cpus;
    let load1 = s.load1;
    let cpus_f = f64::from(cpus.max(1));
    let cpu_some = s.cpu.as_ref().map_or(0.0, |p| p.some_avg10);
    let mem_some = s.memory.as_ref().map_or(0.0, |p| p.some_avg10);
    let mem_full = s.memory.as_ref().and_then(|p| p.full_avg10).unwrap_or(0.0);
    let io_some = s.io.as_ref().map_or(0.0, |p| p.some_avg10);
    let io_full = s.io.as_ref().and_then(|p| p.full_avg10).unwrap_or(0.0);
    let available = s.mem_available_fraction * 100.0;
    // Tasks can wait in iowait with the disks idle (seen here: a QEMU/KVM Windows VM in Docker, 77-91% "some" and
    // "full" in its cgroup, 0 disk I/O). I/O pressure counts only while a disk does I/O; the load average cannot
    // gate it, because it stays high for minutes after heavy CPU work ends.
    let disks_working = s.disks_busy * 100.0 >= t.disk_busy_percent;
    // memory stalls matter only while the CPUs have work: 10% and 25% of them busy
    let working = load1 >= 0.10 * cpus_f;
    let heavily_working = load1 >= 0.25 * cpus_f;
    let (b, o) = (&t.busy, &t.overloaded);

    let overloaded = [
        (
            cpu_some >= o.cpu_pressure,
            format!("CPU pressure {cpu_some:.0}%"),
        ),
        (
            heavily_working && mem_full >= o.memory_pressure,
            format!("memory stalls (full) {mem_full:.0}%"),
        ),
        (
            disks_working && io_full >= o.io_pressure,
            format!("I/O stalls (full) {io_full:.0}%"),
        ),
        (
            available < o.memory_available_percent,
            format!("only {available:.0}% memory available"),
        ),
        (
            load1 >= o.load_per_cpu * cpus_f,
            format!("load {load1:.1} on {cpus} CPUs"),
        ),
    ];
    let busy = [
        (
            cpu_some >= b.cpu_pressure,
            format!("CPU pressure {cpu_some:.0}%"),
        ),
        (
            working && mem_some >= b.memory_pressure,
            format!("memory pressure {mem_some:.0}%"),
        ),
        (
            disks_working && io_some >= b.io_pressure,
            format!("I/O pressure {io_some:.0}%"),
        ),
        (
            available < b.memory_available_percent,
            format!("only {available:.0}% memory available"),
        ),
        (
            load1 >= b.load_per_cpu * cpus_f,
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
    brief_of(
        load.level,
        &load.reasons,
        (load.load1, load.cpus),
        (load.mem_available_bytes, load.mem_total_bytes),
        load.io_stall_source.as_deref(),
    )
}

/// The one-line load text from its parts (the operator's wire type gives the same parts).
#[must_use]
pub fn brief_of(
    level: Level,
    reasons: &[String],
    (load1, cpus): (f64, u32),
    (available, total): (u64, u64),
    stall: Option<&str>,
) -> String {
    let advice = match level {
        Level::Overloaded => " Avoid builds, test suites and parallel jobs until it settles.",
        Level::Busy => " Prefer light commands; limit parallelism (e.g. -j4).",
        _ => "",
    };
    // the load figure follows in the line itself, so a load reason does not show twice
    let reasons: Vec<&str> = reasons
        .iter()
        .map(String::as_str)
        .filter(|r| !r.starts_with("load "))
        .collect();
    let why = if reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", reasons.join(", "))
    };
    let stall = stall.map_or(String::new(), |src| {
        format!(" I/O wait without disk I/O from {src}; not counted.")
    });
    format!(
        "System load: {level:?}{why}; load {load1:.1} on {cpus} CPUs, {} of {} memory free.{advice}{stall}",
        crate::human_bytes(available),
        crate::human_bytes(total)
    )
}

const CGROUPS: &str = "/sys/fs/cgroup";

/// The cgroup that explains the I/O stall and its "some" pressure: from the root, go down to the child that stalls
/// most while it has at least 90% of its parent's pressure (values read moments apart differ a little). `None` when
/// no single cgroup below the root explains it.
fn stall_cgroup(root: &SysRoot) -> anyhow::Result<Option<(PathBuf, f64)>> {
    let pressure = |cgroup: &PathBuf| -> anyhow::Result<Option<f64>> {
        Ok(root
            .read(cgroup.join("io.pressure"))?
            .as_deref()
            .and_then(parse_pressure)
            .map(|p| p.some_avg10))
    };
    let mut path = PathBuf::from(CGROUPS);
    let Some(mut some) = pressure(&path)? else {
        return Ok(None);
    };
    loop {
        let mut best: Option<(PathBuf, f64)> = None;
        for c in root.children(&path, "")? {
            if let Some(p) = pressure(&c)?
                && best.as_ref().is_none_or(|(_, b)| p > *b)
            {
                best = Some((c, p));
            }
        }
        match best.filter(|(_, p)| *p >= 0.9 * some) {
            Some((child, child_some)) => {
                path = child;
                some = child_some;
            }
            None => break,
        }
    }
    Ok((path != Path::new(CGROUPS)).then_some((path, some)))
}

/// `docker/09bd9c9baa5f (77%)`: the path below the cgroup root, container ids cut to 12 characters as `docker ps`
/// shows them.
fn cgroup_name(path: &Path, some: f64) -> String {
    let name: Vec<String> = path
        .strip_prefix(CGROUPS)
        .unwrap_or(path)
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
    format!("{} ({some:.0}%)", name.join("/"))
}

/// Read and write operations a cgroup has done on every device, from its `io.stat`.
fn cgroup_ios(root: &SysRoot, cgroup: Option<&Path>) -> anyhow::Result<Option<u64>> {
    let Some(cgroup) = cgroup else {
        return Ok(None);
    };
    Ok(root.read(cgroup.join("io.stat"))?.map(|stat| {
        stat.split_whitespace()
            .filter_map(|f| f.strip_prefix("rios=").or_else(|| f.strip_prefix("wios=")))
            .filter_map(|n| n.parse::<u64>().ok())
            .sum()
    }))
}

/// Over one short window: the busiest whole disk's share of it (from `io_ticks`, ms spent doing I/O, in
/// /proc/diskstats), and how many I/O operations `cgroup` did in it.
fn sample_io(root: &SysRoot, cgroup: Option<&Path>) -> anyhow::Result<(f64, Option<u64>)> {
    let ticks = || -> anyhow::Result<Vec<(String, u64)>> {
        Ok(root
            .read("/proc/diskstats")?
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let name = *f.get(2)?;
                let whole = name.starts_with("nvme") && !name.contains('p')
                    || name.starts_with("sd") && !name.ends_with(|c: char| c.is_ascii_digit());
                whole.then(|| Some((name.to_string(), f.get(12)?.parse().ok()?)))?
            })
            .collect())
    };
    let window = Duration::from_millis(100);
    let before = (ticks()?, cgroup_ios(root, cgroup)?);
    std::thread::sleep(window);
    let busy = ticks()?
        .iter()
        .filter_map(|(name, t)| {
            let (_, b) = before.0.iter().find(|(n, _)| n == name)?;
            Some(Duration::from_millis(t.saturating_sub(*b)).as_secs_f64() / window.as_secs_f64())
        })
        .fold(0.0, f64::max);
    let ios = before
        .1
        .zip(cgroup_ios(root, cgroup)?)
        .map(|(b, a)| a.saturating_sub(b));
    Ok((busy, ios))
}

/// The load now, with its level against `thresholds`.
///
/// # Errors
/// A `/proc` or cgroup file cannot be read.
pub fn read_load(root: &SysRoot, thresholds: &Thresholds) -> anyhow::Result<Load> {
    let loadavg = root
        .read("/proc/loadavg")?
        .context("/proc/loadavg is missing")?;
    let load1: f64 = loadavg
        .split_whitespace()
        .next()
        .and_then(|l| l.parse().ok())
        .with_context(|| format!("/proc/loadavg has no load: {loadavg}"))?;
    // the machine's CPUs, not this process's share of them: the load average counts the whole machine
    let online = root
        .read("/sys/devices/system/cpu/online")?
        .context("/sys/devices/system/cpu/online is missing")?;
    let cpus = u32::try_from(crate::telemetry::cpu_list(&online).len())?;
    anyhow::ensure!(
        cpus > 0,
        "/sys/devices/system/cpu/online lists no CPU: {online}"
    );
    let cpu = root
        .read("/proc/pressure/cpu")?
        .as_deref()
        .and_then(parse_pressure);
    let memory = root
        .read("/proc/pressure/memory")?
        .as_deref()
        .and_then(parse_pressure);
    let io = root
        .read("/proc/pressure/io")?
        .as_deref()
        .and_then(parse_pressure);
    let meminfo = root
        .read("/proc/meminfo")?
        .context("/proc/meminfo is missing")?;
    let bytes = |key| -> anyhow::Result<u64> {
        meminfo_kib(&meminfo, key)
            .map(|kib| kib << 10)
            .with_context(|| format!("/proc/meminfo has no {key}"))
    };
    let total = bytes("MemTotal")?;
    let available = bytes("MemAvailable")?;
    // whole MiB keep the ratio exact in an f64
    let fraction = f64::from(u32::try_from(available >> 20).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total >> 20).unwrap_or(u32::MAX).max(1));
    // sample the disks only when I/O pressure could count, so the usual call stays instant ("full" <= "some")
    let io_some = io.as_ref().map_or(0.0, |p| p.some_avg10);
    let io_counts = io_some
        >= thresholds
            .busy
            .io_pressure
            .min(thresholds.overloaded.io_pressure);
    // I/O pressure that one cgroup explains while it does no I/O itself is iowait without disk work (a VM's
    // threads, seen here: WinBoat); it does not count even when another process uses a disk at that moment
    let source = if io_counts { stall_cgroup(root)? } else { None };
    let (busy, source_ios) = if io_counts {
        sample_io(root, source.as_ref().map(|s| s.0.as_path()))?
    } else {
        (0.0, None)
    };
    let phantom = source_ios == Some(0);
    let disks = if phantom { 0.0 } else { busy };
    let sample = Sample {
        load1,
        cpus,
        cpu: cpu.clone(),
        memory: memory.clone(),
        io: io.clone(),
        disks_busy: disks,
        mem_available_fraction: fraction,
    };
    let (level, mut reasons) = classify(&sample, thresholds);
    if cpu.is_none() && memory.is_none() && io.is_none() {
        // a kernel without PSI: the level comes from the load and memory only
        reasons.push("PSI not available (no /proc/pressure): stalls are not counted".into());
    }
    Ok(Load {
        level,
        reasons,
        load1,
        cpus,
        cpu,
        memory,
        io,
        mem_available_bytes: available,
        mem_total_bytes: total,
        io_stall_source: source
            .filter(|_| phantom || disks * 100.0 < thresholds.disk_busy_percent)
            .map(|(path, some)| cgroup_name(&path, some)),
    })
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
        let defaults: serde_json::Value =
            serde_json::from_reader(json_comments::StripComments::new(
                include_str!("../../../config/defaults.jsonc").as_bytes(),
            ))
            .unwrap();
        let thresholds: Thresholds = serde_json::from_value(defaults["load"].clone()).unwrap();
        let level = |load1, disks_busy| {
            let sample = Sample {
                load1,
                cpus: 32,
                cpu: Some(quiet.clone()),
                memory: None,
                io: Some(phantom.clone()),
                disks_busy,
                mem_available_fraction: 0.3,
            };
            classify(&sample, &thresholds)
        };
        assert_eq!(level(2.3, 0.0).0, Level::Idle);
        // just after a full CPU + GPU load test: load average still 10, disks idle (2026-10-12 false "Overloaded")
        assert_eq!(level(10.0, 0.0).0, Level::Normal);
        // the same pressure while a disk really works is reported
        let (busy, why) = level(2.3, 0.6);
        assert_eq!(busy, Level::Overloaded);
        assert_eq!(why, vec!["I/O stalls (full) 65%".to_string()]);

        // the cgroup tree of that measurement: the container stalls, the rest of the system does not
        let dir = crate::scratch::Scratch::new(
            std::env::temp_dir().join(format!("legionctl-io-source-{}", std::process::id())),
        );
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
        let root = SysRoot::new(&dir);
        let (path, some) = stall_cgroup(&root).unwrap().unwrap();
        assert_eq!(cgroup_name(&path, some), "docker/09bd9c9baa5f (77%)");
        // the container did no I/O in the window: its stall is not load, whatever the disks do
        std::fs::write(
            dir.join("sys/fs/cgroup/docker").join(id).join("io.stat"),
            "259:6 rios=44630 wios=13264\n",
        )
        .unwrap();
        assert_eq!(sample_io(&root, Some(&path)).unwrap().1, Some(0));
    }
}
