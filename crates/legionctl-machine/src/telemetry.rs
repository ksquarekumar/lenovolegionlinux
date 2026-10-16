//! One telemetry sample (note 16, `legionctl monitor`): package power from RAPL energy over a short window, CPU
//! temperature and clock, the NVIDIA GPU (only when it is already awake: a query would wake a suspended GPU), PSI,
//! every hwmon channel (a device that sleeps is not read: the read would wake it), the memory pressure of each agent
//! session (cgroup PSI), and the root-only part from `legionctl-system` (fan RPM, throttle reasons from the MSR, the
//! eBPF sensors).

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::firmware;
use crate::load::{Pressure, parse_pressure};
use crate::sysroot::SysRoot;

const RAPL_PACKAGE: &str = "/sys/class/powercap/intel-rapl:0/energy_uj";
const RAPL_RANGE: &str = "/sys/class/powercap/intel-rapl:0/max_energy_range_uj";
const NVIDIA_VENDOR: &str = "0x10de";
/// The processes in [`Sample::cpu_energy`].
const TOP: usize = 5;

/// What only root can read; `legionctl-system` answers it over D-Bus (`Sample`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RootSample {
    pub fan_rpm: Option<BTreeMap<String, u64>>,
    /// Throttle reasons active now.
    pub throttle_now: Option<Vec<String>>,
    /// Throttle reasons logged since boot (the MSR's sticky bits).
    pub throttle_logged: Option<Vec<String>>,
    /// How often each throttle reason started since the daemon started, per workload kind that was active then
    /// (`none`: no kind): reason -> kind -> starts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub throttle_episodes: BTreeMap<String, BTreeMap<String, u64>>,
    /// The eBPF sensor's cumulative run-queue wait histogram per CPU class (bucket i: 2^i to 2^(i+1) µs). A sample
    /// turns two of these into `runq_latency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runq: Option<BTreeMap<String, Vec<u64>>>,
    /// The eBPF power sensor's cumulative weight of each process (CPU time × clock). A sample turns two of these into
    /// `cpu_energy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<Vec<ProcessWeight>>,
    /// The eBPF block I/O sensor's cumulative latency histogram per disk (bucket i: 2^i to 2^(i+1) µs). A sample
    /// turns two of these into `io_latency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io: Option<BTreeMap<String, Vec<u64>>>,
    /// Thermal trip points crossed per thermal zone since the eBPF thermal sensor started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_trips: Option<BTreeMap<String, u64>>,
    /// Why each eBPF sensor that does not run does not (no permission, the verifier's message), by sensor.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sensor_errors: BTreeMap<String, String>,
}

/// One process's cumulative CPU time weighted by the clock of its CPU (µs × MHz), from the eBPF power sensor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessWeight {
    pub pid: u32,
    /// The process's name (`comm`).
    pub name: String,
    pub weight: u64,
}

/// A process's estimated share of the CPU energy in a sample window: its CPU time weighted by the clock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EnergyShare {
    pub pid: u32,
    pub name: String,
    pub share_percent: f64,
    /// The share of the package power (RAPL) in the window.
    pub watts: Option<f64>,
}

/// Run-queue latency over a sample window: how long woken tasks waited for a CPU. Log2 resolution: a percentile is
/// the upper bound of its bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Latency {
    pub count: u64,
    pub p50_us: u64,
    pub p99_us: u64,
}

/// CPU numbers in a kernel list (`0-15,24`).
fn cpu_list(text: &str) -> Vec<usize> {
    text.split(',')
        .filter_map(|part| {
            let (a, b) = part
                .trim()
                .split_once('-')
                .unwrap_or((part.trim(), part.trim()));
            Some(a.parse().ok()?..=b.parse().ok()?)
        })
        .flatten()
        .collect()
}

/// Per-CPU histograms summed per CPU class: `performance` and `efficiency` cores, or `all` on a CPU without classes.
///
/// # Errors
/// A CPU class list cannot be read.
pub fn by_class(root: &SysRoot, per_cpu: &[Vec<u64>]) -> Result<BTreeMap<String, Vec<u64>>> {
    let mut classes: Vec<(&str, Vec<usize>)> = Vec::new();
    for (name, dir) in [("performance", "cpu_core"), ("efficiency", "cpu_atom")] {
        if let Some(cpus) = root.read(format!("/sys/devices/{dir}/cpus"))? {
            classes.push((name, cpu_list(&cpus)));
        }
    }
    let mut out: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (cpu, hist) in per_cpu.iter().enumerate() {
        let class = classes
            .iter()
            .find(|(_, cpus)| cpus.contains(&cpu))
            .map_or("all", |(name, _)| name);
        let sum = out.entry(class.into()).or_default();
        sum.resize(sum.len().max(hist.len()), 0);
        for (bucket, n) in hist.iter().enumerate() {
            sum[bucket] += n;
        }
    }
    Ok(out)
}

fn percentile(hist: &[u64], count: u64, share: f64) -> u64 {
    // the rank of the percentile, rounded up (count is small enough for f64)
    let rank = (f64::from(u32::try_from(count).unwrap_or(u32::MAX)) * share).ceil();
    let mut seen = 0.0;
    for (bucket, n) in hist.iter().enumerate() {
        seen += f64::from(u32::try_from(*n).unwrap_or(u32::MAX));
        if seen >= rank {
            return 2u64 << bucket;
        }
    }
    2u64 << hist.len().saturating_sub(1)
}

/// The latency in the window between two cumulative readings.
#[must_use]
pub fn latency(
    before: &BTreeMap<String, Vec<u64>>,
    after: &BTreeMap<String, Vec<u64>>,
) -> BTreeMap<String, Latency> {
    after
        .iter()
        .filter_map(|(class, hist)| {
            let old = before.get(class);
            let delta: Vec<u64> = hist
                .iter()
                .enumerate()
                .map(|(i, n)| n.saturating_sub(old.and_then(|o| o.get(i)).copied().unwrap_or(0)))
                .collect();
            let count = delta.iter().sum();
            (count > 0).then(|| {
                (
                    class.clone(),
                    Latency {
                        count,
                        p50_us: percentile(&delta, count, 0.5),
                        p99_us: percentile(&delta, count, 0.99),
                    },
                )
            })
        })
        .collect()
}

/// The [`TOP`] processes by their share of the weight added between two cumulative readings, largest first;
/// `package_w` is the package power in the window.
#[must_use]
pub fn cpu_energy(
    before: &[ProcessWeight],
    after: &[ProcessWeight],
    package_w: Option<f64>,
) -> Vec<EnergyShare> {
    let old: HashMap<u32, u64> = before.iter().map(|p| (p.pid, p.weight)).collect();
    let mut added: Vec<(&ProcessWeight, u64)> = after
        .iter()
        .map(|p| {
            (
                p,
                p.weight
                    .saturating_sub(old.get(&p.pid).copied().unwrap_or(0)),
            )
        })
        .filter(|(_, w)| *w > 0)
        .collect();
    let total: u64 = added.iter().map(|(_, w)| w).sum();
    added.sort_by_key(|a| std::cmp::Reverse(a.1));
    added
        .into_iter()
        .take(TOP)
        .map(|(p, w)| {
            // in hundredths of a percent, so the weights (which pass u32) stay integers
            let hundredths = u128::from(w) * 10_000 / u128::from(total);
            let share = f64::from(u32::try_from(hundredths).unwrap_or(u32::MAX)) / 100.0;
            EnergyShare {
                pid: p.pid,
                name: p.name.clone(),
                share_percent: share,
                watts: package_w.map(|watts| watts * share / 100.0),
            }
        })
        .collect()
}

/// The agent session scopes of user `uid` (`legionctl-session-<id>.scope`) in the cgroup tree at `cgroup`
/// (`/sys/fs/cgroup`): their folders and session ids.
#[must_use]
pub fn session_scopes(cgroup: &Path, uid: u32) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if !entry.metadata().is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            match name
                .strip_prefix("legionctl-session-")
                .and_then(|n| n.strip_suffix(".scope"))
            {
                Some(id) => out.push((entry.path(), id.to_string())),
                None if depth > 0 => walk(&entry.path(), depth - 1, out),
                None => {}
            }
        }
    }
    let mut out = Vec::new();
    // the scopes sit in app.slice, or a slice below it
    walk(
        &cgroup.join(format!("user.slice/user-{uid}.slice/user@{uid}.service")),
        2,
        &mut out,
    );
    out
}

/// The memory pressure of each agent session of this process's user: its scope's `memory.pressure`.
fn session_memory_pressure(root: &SysRoot) -> BTreeMap<String, Pressure> {
    let Ok(me) = std::fs::metadata("/proc/self") else {
        return BTreeMap::new();
    };
    session_scopes(&root.path("/sys/fs/cgroup"), me.uid())
        .into_iter()
        .filter_map(|(dir, id)| {
            let text = std::fs::read_to_string(dir.join("memory.pressure")).ok()?;
            Some((id, parse_pressure(&text)?))
        })
        .collect()
}

/// Read the root-only values (in `legionctl-system`).
#[must_use]
pub fn root_sample(root: &SysRoot) -> RootSample {
    let names = |r: Vec<&str>| r.into_iter().map(String::from).collect();
    let fan_rpm = firmware::fan_rpm(root);
    let sensor_errors = fan_rpm
        .as_ref()
        .err()
        .map(|e| ("fan_rpm".to_string(), format!("{e:#}")))
        .into_iter()
        .collect();
    RootSample {
        fan_rpm: fan_rpm.ok(),
        throttle_now: firmware::current_throttle_reasons(root).map(names),
        throttle_logged: firmware::logged_throttle_reasons(root).map(names),
        sensor_errors,
        ..RootSample::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GpuSample {
    /// PCI runtime power state: active, suspended, ...; the values below only when active.
    pub state: String,
    pub power_w: Option<f64>,
    pub temp_c: Option<f64>,
    pub util_percent: Option<f64>,
    pub clock_mhz: Option<f64>,
}

/// One hwmon channel (`temp1_input` of `hwmon4`), in its unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Channel {
    /// The hwmon device and its name: `hwmon4 coretemp`.
    pub device: String,
    /// The channel's label, else its name (`temp1`).
    pub name: String,
    pub value: f64,
    /// °C, RPM, W, V, A or MHz.
    pub unit: String,
}

/// The hwmon channel kinds: file prefix, the divisor to the unit, the unit, and the `OpenMetrics` name.
const KINDS: [(&str, f64, &str, &str); 6] = [
    ("temp", 1e3, "°C", "celsius"),
    ("fan", 1.0, "RPM", "rpm"),
    ("power", 1e6, "W", "watts"),
    ("in", 1e3, "V", "volts"),
    ("curr", 1e3, "A", "amperes"),
    ("freq", 1e6, "MHz", "mhz"),
];

/// Every channel of every hwmon device, and the devices not read because they sleep: a read of a sleeping device's
/// sensor wakes it (the NVIDIA GPU's), its runtime power state alone does not.
fn hwmon(root: &SysRoot) -> Result<(Vec<Channel>, Vec<String>)> {
    let (mut channels, mut asleep) = (Vec::new(), Vec::new());
    for dir in root.children("/sys/class/hwmon", "hwmon")? {
        let id = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let device = format!("{id} {}", root.read(dir.join("name"))?.unwrap_or_default());
        if root
            .read(dir.join("device/power/runtime_status"))?
            .is_some_and(|s| s != "active" && s != "unsupported")
        {
            asleep.push(device);
            continue;
        }
        for file in root.children(&dir, "")? {
            let file = file
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let Some(stem) = file
                .strip_suffix("_input")
                .or_else(|| file.strip_suffix("_average"))
            else {
                continue;
            };
            let Some((_, divisor, unit, _)) = KINDS.iter().find(|(kind, ..)| {
                stem.strip_prefix(kind)
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            }) else {
                continue;
            };
            if let Some(raw) = root
                .read(dir.join(&file))?
                .and_then(|v| v.parse::<f64>().ok())
            {
                channels.push(Channel {
                    device: device.clone(),
                    name: root
                        .read(dir.join(format!("{stem}_label")))?
                        .unwrap_or_else(|| stem.into()),
                    value: raw / divisor,
                    unit: (*unit).into(),
                });
            }
        }
    }
    Ok((channels, asleep))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Sample {
    /// Milliseconds since the epoch at the end of the window.
    pub ts_ms: u64,
    /// Package power over the sample window (RAPL energy).
    pub package_w: Option<f64>,
    pub cpu_temp_c: Option<f64>,
    /// Mean current clock over all CPUs.
    pub cpu_mhz: Option<f64>,
    pub gpu: Option<GpuSample>,
    pub cpu_pressure: Option<Pressure>,
    pub memory_pressure: Option<Pressure>,
    pub io_pressure: Option<Pressure>,
    /// Run-queue latency in the window per CPU class (the daemon's eBPF sensor).
    pub runq_latency: Option<BTreeMap<String, Latency>>,
    /// The processes with the largest estimated CPU energy share in the window, largest first (the daemon's eBPF
    /// power sensor).
    pub cpu_energy: Option<Vec<EnergyShare>>,
    /// I/O latency in the window per disk (the daemon's eBPF block I/O sensor).
    pub io_latency: Option<BTreeMap<String, Latency>>,
    /// The memory pressure of each agent session of this user (its cgroup's PSI), by session id.
    pub session_memory_pressure: BTreeMap<String, Pressure>,
    /// Every hwmon channel.
    pub hwmon: Vec<Channel>,
    /// The hwmon devices not read: their device sleeps.
    pub hwmon_asleep: Vec<String>,
    #[serde(flatten)]
    pub root: RootSample,
}

fn read_f64(root: &SysRoot, path: &str) -> Result<Option<f64>> {
    Ok(root.read(path)?.and_then(|v| v.parse().ok()))
}

/// Joules per second from two RAPL readings, across a counter wrap.
fn watts(before: f64, after: f64, range: Option<f64>, window: Duration) -> Option<f64> {
    let delta = if after >= before {
        after - before
    } else {
        range? - before + after
    };
    Some(delta / 1e6 / window.as_secs_f64())
}

/// The package temperature (`coretemp`'s `temp1`), in °C.
///
/// # Errors
/// A hwmon device's files cannot be read.
pub fn cpu_temp(root: &SysRoot) -> Result<Option<f64>> {
    let Some(h) = firmware::hwmon(root, "coretemp")? else {
        return Ok(None);
    };
    Ok(read_f64(root, &h.join("temp1_input").to_string_lossy())?.map(|m| m / 1000.0))
}

fn cpu_mhz(root: &SysRoot) -> Result<Option<f64>> {
    let mut khz: Vec<f64> = Vec::new();
    for p in root.children("/sys/devices/system/cpu/cpufreq", "policy")? {
        khz.extend(read_f64(
            root,
            &p.join("scaling_cur_freq").to_string_lossy(),
        )?);
    }
    Ok((!khz.is_empty()).then(|| {
        khz.iter().sum::<f64>() / f64::from(u32::try_from(khz.len()).unwrap_or(u32::MAX)) / 1000.0
    }))
}

/// The NVIDIA GPU's runtime power state, and its readings when it is awake.
fn gpu(root: &SysRoot) -> Result<Option<GpuSample>> {
    let mut found = None;
    for d in root.children("/sys/bus/pci/devices", "")? {
        if root.read(d.join("vendor"))?.as_deref() == Some(NVIDIA_VENDOR)
            && root
                .read(d.join("class"))?
                .is_some_and(|c| c.starts_with("0x03"))
        {
            found = Some(d);
            break;
        }
    }
    let Some(device) = found else {
        return Ok(None);
    };
    let state = root
        .read(device.join("power/runtime_status"))?
        .unwrap_or_else(|| "unknown".into());
    let mut sample = GpuSample {
        state,
        power_w: None,
        temp_c: None,
        util_percent: None,
        clock_mhz: None,
    };
    if sample.state == "active"
        && let Some(line) = std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=power.draw,temperature.gpu,utilization.gpu,clocks.sm",
                "--format=csv,noheader,nounits",
            ])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        let values: Vec<Option<f64>> = line.split(',').map(|v| v.trim().parse().ok()).collect();
        if let [power, temp, util, clock] = values[..] {
            (
                sample.power_w,
                sample.temp_c,
                sample.util_percent,
                sample.clock_mhz,
            ) = (power, temp, util, clock);
        }
    }
    Ok(Some(sample))
}

/// One sample over `window` (the package power is the mean over it); `root_part` reads `legionctl-system`'s part,
/// at the start and at the end of the window.
///
/// # Errors
/// A machine file cannot be read.
pub fn sample(
    root: &SysRoot,
    window: Duration,
    root_part: &dyn Fn() -> RootSample,
) -> Result<Sample> {
    let first = root_part();
    let before = read_f64(root, RAPL_PACKAGE)?;
    std::thread::sleep(window);
    let after = read_f64(root, RAPL_PACKAGE)?;
    let mut last = root_part();
    let runq_latency = first
        .runq
        .zip(last.runq.take())
        .map(|(a, b)| latency(&a, &b));
    let io_latency = first.io.zip(last.io.take()).map(|(a, b)| latency(&a, &b));
    let range = read_f64(root, RAPL_RANGE)?;
    let package_w = before
        .zip(after)
        .and_then(|(b, a)| watts(b, a, range, window));
    let cpu_energy = first
        .processes
        .zip(last.processes.take())
        .map(|(a, b)| cpu_energy(&a, &b, package_w));
    let psi = |name: &str| -> Result<Option<Pressure>> {
        Ok(root
            .read(format!("/proc/pressure/{name}"))?
            .as_deref()
            .and_then(parse_pressure))
    };
    let (hwmon, hwmon_asleep) = hwmon(root)?;
    Ok(Sample {
        ts_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        package_w,
        cpu_temp_c: cpu_temp(root)?,
        cpu_mhz: cpu_mhz(root)?,
        gpu: gpu(root)?,
        cpu_pressure: psi("cpu")?,
        memory_pressure: psi("memory")?,
        io_pressure: psi("io")?,
        runq_latency,
        cpu_energy,
        io_latency,
        session_memory_pressure: session_memory_pressure(root),
        hwmon,
        hwmon_asleep,
        root: last,
    })
}

/// A label value in `OpenMetrics` text.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// One gauge with its values (labels, value) in `OpenMetrics` text; none without values.
fn gauge(lines: &mut Vec<String>, name: &str, help: &str, values: Vec<(String, String)>) {
    if !values.is_empty() {
        lines.push(format!("# TYPE legionctl_{name} gauge"));
        lines.push(format!("# HELP legionctl_{name} {help}"));
        lines.extend(
            values
                .into_iter()
                .map(|(labels, v)| format!("legionctl_{name}{labels} {v}")),
        );
    }
}

/// The eBPF sensors' readings and the sessions' memory pressure.
fn ebpf_gauges(lines: &mut Vec<String>, s: &Sample) {
    gauge(
        lines,
        "cpu_energy_share_percent",
        "A process's estimated share of the CPU energy over the sample window (the largest only)",
        s.cpu_energy
            .iter()
            .flatten()
            .map(|e| {
                (
                    format!("{{pid=\"{}\",name=\"{}\"}}", e.pid, escape(&e.name)),
                    e.share_percent.to_string(),
                )
            })
            .collect(),
    );
    gauge(
        lines,
        "io_latency_p99_microseconds",
        "Block I/O latency p99 over the sample window, per disk",
        s.io_latency
            .iter()
            .flatten()
            .map(|(disk, l)| {
                (
                    format!("{{disk=\"{}\"}}", escape(disk)),
                    l.p99_us.to_string(),
                )
            })
            .collect(),
    );
    gauge(
        lines,
        "thermal_trips",
        "Trip points crossed per thermal zone since the sensor started",
        s.root
            .thermal_trips
            .iter()
            .flatten()
            .map(|(zone, n)| (format!("{{zone=\"{}\"}}", escape(zone)), n.to_string()))
            .collect(),
    );
    gauge(
        lines,
        "throttle_episodes",
        "Starts of a throttle reason since the daemon started, per workload kind active then",
        s.root
            .throttle_episodes
            .iter()
            .flat_map(|(reason, kinds)| {
                kinds.iter().map(move |(kind, n)| {
                    (
                        format!(
                            "{{reason=\"{}\",workload=\"{}\"}}",
                            escape(reason),
                            escape(kind)
                        ),
                        n.to_string(),
                    )
                })
            })
            .collect(),
    );
    gauge(
        lines,
        "session_memory_pressure_some_percent",
        "Memory PSI some avg10 of each agent session's cgroup",
        s.session_memory_pressure
            .iter()
            .map(|(id, p)| {
                (
                    format!("{{session=\"{}\"}}", escape(id)),
                    p.some_avg10.to_string(),
                )
            })
            .collect(),
    );
}

/// Each hwmon channel, per unit, and each active throttle reason.
fn sensor_gauges(lines: &mut Vec<String>, s: &Sample) {
    for (_, _, unit, name) in KINDS {
        gauge(
            lines,
            &format!("hwmon_{name}"),
            &format!("Each hwmon channel in {unit}"),
            s.hwmon
                .iter()
                .filter(|c| c.unit == unit)
                .map(|c| {
                    (
                        format!(
                            "{{device=\"{}\",sensor=\"{}\"}}",
                            escape(&c.device),
                            escape(&c.name)
                        ),
                        c.value.to_string(),
                    )
                })
                .collect(),
        );
    }
    gauge(
        lines,
        "throttle_active",
        "A throttle reason that is active now (the MSR)",
        s.root
            .throttle_now
            .iter()
            .flatten()
            .map(|r| (format!("{{reason=\"{}\"}}", escape(r)), "1".into()))
            .collect(),
    );
}

/// The sample in `OpenMetrics` text (`GET /v1/metrics`), for Prometheus or Grafana.
#[must_use]
pub fn openmetrics(s: &Sample) -> String {
    let mut lines = Vec::new();
    let mut gauge = |name: &str, help: &str, values: Vec<(String, String)>| {
        gauge(&mut lines, name, help, values);
    };
    let one = |v: Option<f64>| {
        v.map(|v| (String::new(), v.to_string()))
            .into_iter()
            .collect::<Vec<_>>()
    };
    let gpu = s.gpu.as_ref();
    gauge(
        "package_watts",
        "CPU package power over the sample window",
        one(s.package_w),
    );
    gauge(
        "cpu_temperature_celsius",
        "CPU package temperature",
        one(s.cpu_temp_c),
    );
    gauge(
        "cpu_frequency_mhz",
        "Mean current CPU clock",
        one(s.cpu_mhz),
    );
    gauge(
        "gpu_watts",
        "NVIDIA GPU power, while it is awake",
        one(gpu.and_then(|g| g.power_w)),
    );
    gauge(
        "gpu_temperature_celsius",
        "NVIDIA GPU temperature",
        one(gpu.and_then(|g| g.temp_c)),
    );
    gauge(
        "gpu_utilization_percent",
        "NVIDIA GPU utilization",
        one(gpu.and_then(|g| g.util_percent)),
    );
    let psi = [
        ("cpu", &s.cpu_pressure),
        ("memory", &s.memory_pressure),
        ("io", &s.io_pressure),
    ];
    gauge(
        "pressure_some_percent",
        "PSI some avg10",
        psi.iter()
            .filter_map(|(r, p)| {
                Some((
                    format!("{{resource=\"{r}\"}}"),
                    p.as_ref()?.some_avg10.to_string(),
                ))
            })
            .collect(),
    );
    gauge(
        "fan_rpm",
        "Fan speed from the firmware",
        s.root
            .fan_rpm
            .iter()
            .flatten()
            .map(|(f, rpm)| (format!("{{fan=\"{f}\"}}"), rpm.to_string()))
            .collect(),
    );
    gauge(
        "runq_latency_p99_microseconds",
        "Run-queue latency p99 over the sample window, per CPU class",
        s.runq_latency
            .iter()
            .flatten()
            .map(|(class, l)| (format!("{{class=\"{class}\"}}"), l.p99_us.to_string()))
            .collect(),
    );
    sensor_gauges(&mut lines, s);
    ebpf_gauges(&mut lines, s);
    lines.push("# EOF".into());
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_power_survives_a_rapl_counter_wrap() {
        let window = Duration::from_millis(500);
        assert_eq!(watts(1_000_000.0, 11_000_000.0, None, window), Some(20.0));
        // the counter wrapped at max_energy_range_uj during the window
        assert_eq!(
            watts(
                262_143_000_000.0,
                9_000_000.0,
                Some(262_143_328_850.0),
                window
            )
            .map(f64::round),
            Some(19.0)
        );
        assert_eq!(
            watts(5.0, 1.0, None, window),
            None,
            "a wrap without the range is unknown, not negative"
        );
    }
}
