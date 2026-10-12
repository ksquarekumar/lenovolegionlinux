//! System load as agents and humans should read it: one level plus the reasons behind it.
//!
//! Pressure stall information (PSI, `/proc/pressure/*`) says how much work is actually waiting on CPU, memory
//! or I/O, which tells "busy" apart from "many runnable threads" better than the load average alone.

use schemars::JsonSchema;
use serde::Serialize;

use crate::read;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Pressure {
    /// Percent of the last 10 s in which at least one task stalled on this resource.
    pub some_avg10: f64,
    /// Percent of the last 10 s in which all non-idle tasks stalled (not reported for CPU on every kernel).
    pub full_avg10: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Idle,
    Normal,
    Busy,
    Overloaded,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Load {
    pub level: Level,
    pub reasons: Vec<String>,
    pub load1: f64,
    pub cpus: usize,
    pub cpu: Option<Pressure>,
    pub memory: Option<Pressure>,
    pub io: Option<Pressure>,
    pub mem_available_mib: u64,
    pub mem_total_mib: u64,
}

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
pub fn classify(
    load1: f64,
    cpus: usize,
    cpu: Option<&Pressure>,
    memory: Option<&Pressure>,
    io: Option<&Pressure>,
    mem_available_fraction: f64,
) -> (Level, Vec<String>) {
    let cpus_f = cpus.max(1) as f64;
    let cpu_some = cpu.map_or(0.0, |p| p.some_avg10);
    let mem_some = memory.map_or(0.0, |p| p.some_avg10);
    let mem_full = memory.and_then(|p| p.full_avg10).unwrap_or(0.0);
    let io_some = io.map_or(0.0, |p| p.some_avg10);
    let io_full = io.and_then(|p| p.full_avg10).unwrap_or(0.0);
    // Kernel workers stuck in uninterruptible sleep (seen here: RCU grace-period kworkers) count as I/O stalls with
    // the disks idle, and a single stalled task maxes out "full" on a quiet machine. Memory and I/O pressure only
    // mean the machine is loaded once it is actually doing work.
    // shortcut: gates on load average; upgrade to sampling disk busy time (/proc/diskstats io_ticks) in the daemon
    let working = load1 >= 0.10 * cpus_f;
    let heavily_working = load1 >= 0.25 * cpus_f;

    let overloaded = [
        (cpu_some >= 60.0, format!("CPU pressure {cpu_some:.0}%")),
        (
            heavily_working && mem_full >= 10.0,
            format!("memory stalls (full) {mem_full:.0}%"),
        ),
        (
            heavily_working && io_full >= 30.0,
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
            working && io_some >= 20.0,
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
    format!(
        "System load: {:?}{why}; load {:.1} on {} CPUs, {} of {} free.{advice}",
        load.level,
        load.load1,
        load.cpus,
        crate::human_bytes(load.mem_available_mib * 1024 * 1024),
        crate::human_bytes(load.mem_total_mib * 1024 * 1024)
    )
}

pub fn read_load() -> Load {
    let load1 = read("/proc/loadavg")
        .and_then(|t| t.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0);
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let cpu = read("/proc/pressure/cpu")
        .as_deref()
        .and_then(parse_pressure);
    let memory = read("/proc/pressure/memory")
        .as_deref()
        .and_then(parse_pressure);
    let io = read("/proc/pressure/io")
        .as_deref()
        .and_then(parse_pressure);
    let meminfo = read("/proc/meminfo").unwrap_or_default();
    let total = meminfo_kib(&meminfo, "MemTotal").unwrap_or(0);
    let available = meminfo_kib(&meminfo, "MemAvailable").unwrap_or(0);
    let fraction = if total > 0 {
        available as f64 / total as f64
    } else {
        1.0
    };
    let (level, reasons) = classify(
        load1,
        cpus,
        cpu.as_ref(),
        memory.as_ref(),
        io.as_ref(),
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
        mem_available_mib: available / 1024,
        mem_total_mib: total / 1024,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PSI: &str = "some avg10=34.50 avg60=20.00 avg300=5.00 total=1\nfull avg10=2.00 avg60=1.00 avg300=0.50 total=1";

    #[test]
    fn parses_pressure_lines() {
        let p = parse_pressure(PSI).unwrap();
        assert_eq!(p.some_avg10, 34.5);
        assert_eq!(p.full_avg10, Some(2.0));
    }

    #[test]
    fn levels_follow_thresholds() {
        let quiet = Pressure {
            some_avg10: 0.5,
            full_avg10: None,
        };
        assert_eq!(
            classify(0.4, 32, Some(&quiet), None, None, 0.6).0,
            Level::Idle
        );
        assert_eq!(
            classify(8.0, 32, Some(&quiet), None, None, 0.6).0,
            Level::Normal
        );
        let busy_cpu = parse_pressure(PSI).unwrap();
        let (level, why) = classify(8.0, 32, Some(&busy_cpu), None, None, 0.6);
        assert_eq!(level, Level::Busy);
        assert_eq!(why, vec!["CPU pressure 34%".to_string()]);
        assert_eq!(
            classify(60.0, 32, Some(&quiet), None, None, 0.6).0,
            Level::Overloaded
        );
        assert_eq!(
            classify(1.0, 32, None, None, None, 0.03).0,
            Level::Overloaded
        );
    }

    #[test]
    fn stuck_kernel_workers_on_an_idle_machine_are_not_load() {
        // measured on this machine: I/O some/full ~77% from D-state RCU kworkers, disks 0.4% busy, load 2.3/32
        let phantom = Pressure {
            some_avg10: 77.0,
            full_avg10: Some(65.0),
        };
        let quiet = Pressure {
            some_avg10: 1.0,
            full_avg10: None,
        };
        assert_eq!(
            classify(2.3, 32, Some(&quiet), None, Some(&phantom), 0.3).0,
            Level::Idle
        );
        // the same pressure while really working is reported
        let (level, why) = classify(10.0, 32, Some(&quiet), None, Some(&phantom), 0.3);
        assert_eq!(level, Level::Overloaded);
        assert_eq!(why, vec!["I/O stalls (full) 65%".to_string()]);
    }

    #[test]
    fn reads_meminfo_keys() {
        let text = "MemTotal:       32000000 kB\nMemAvailable:   16000000 kB";
        assert_eq!(meminfo_kib(text, "MemAvailable"), Some(16_000_000));
        assert_eq!(meminfo_kib(text, "SwapTotal"), None);
    }
}
