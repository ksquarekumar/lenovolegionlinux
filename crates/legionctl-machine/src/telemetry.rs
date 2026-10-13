//! One telemetry sample (note 16, `legionctl monitor`): package power from RAPL energy over a short window, CPU
//! temperature and clock, the NVIDIA GPU (only when it is already awake: a query would wake a suspended GPU), PSI,
//! and the root-only part from `legionctl-system` (fan RPM, throttle reasons from the MSR).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::firmware;
use crate::load::{Pressure, parse_pressure};
use crate::sysroot::SysRoot;

const RAPL_PACKAGE: &str = "/sys/class/powercap/intel-rapl:0/energy_uj";
const RAPL_RANGE: &str = "/sys/class/powercap/intel-rapl:0/max_energy_range_uj";
const NVIDIA_VENDOR: &str = "0x10de";

/// What only root can read; `legionctl-system` answers it over D-Bus (`Sample`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RootSample {
    pub fan_rpm: Option<BTreeMap<String, u64>>,
    /// Throttle reasons active now.
    pub throttle_now: Option<Vec<String>>,
    /// Throttle reasons logged since boot (the MSR's sticky bits).
    pub throttle_logged: Option<Vec<String>>,
}

/// Read the root-only values (in `legionctl-system`).
#[must_use]
pub fn root_sample(root: &SysRoot) -> RootSample {
    let names = |r: Vec<&str>| r.into_iter().map(String::from).collect();
    RootSample {
        fan_rpm: firmware::fan_rpm(root)
            .map(|f| f.iter().map(|(n, v)| ((*n).to_string(), *v)).collect()),
        throttle_now: firmware::current_throttle_reasons(root).map(names),
        throttle_logged: firmware::logged_throttle_reasons(root).map(names),
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
    #[serde(flatten)]
    pub root: RootSample,
}

fn read_f64(root: &SysRoot, path: &str) -> Option<f64> {
    root.read(path)?.parse().ok()
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

fn cpu_temp(root: &SysRoot) -> Option<f64> {
    root.children("/sys/class/hwmon", "hwmon")
        .into_iter()
        .find(|h| root.read(h.join("name")).as_deref() == Some("coretemp"))
        .and_then(|h| root.read(h.join("temp1_input")))
        .and_then(|t| t.parse::<f64>().ok())
        .map(|m| m / 1000.0)
}

fn cpu_mhz(root: &SysRoot) -> Option<f64> {
    let khz: Vec<f64> = root
        .children("/sys/devices/system/cpu/cpufreq", "policy")
        .iter()
        .filter_map(|p| root.read(p.join("scaling_cur_freq"))?.parse().ok())
        .collect();
    (!khz.is_empty()).then(|| {
        khz.iter().sum::<f64>() / f64::from(u32::try_from(khz.len()).unwrap_or(u32::MAX)) / 1000.0
    })
}

/// The NVIDIA GPU's runtime power state, and its readings when it is awake.
fn gpu(root: &SysRoot) -> Option<GpuSample> {
    let device = root
        .children("/sys/bus/pci/devices", "")
        .into_iter()
        .find(|d| {
            root.read(d.join("vendor")).as_deref() == Some(NVIDIA_VENDOR)
                && root
                    .read(d.join("class"))
                    .is_some_and(|c| c.starts_with("0x03"))
        })?;
    let state = root
        .read(device.join("power/runtime_status"))
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
    Some(sample)
}

/// One sample over `window` (the package power is the mean over it); `root_part` comes from `legionctl-system`.
#[must_use]
pub fn sample(root: &SysRoot, window: Duration, root_part: RootSample) -> Sample {
    let before = read_f64(root, RAPL_PACKAGE);
    std::thread::sleep(window);
    let after = read_f64(root, RAPL_PACKAGE);
    let psi = |name: &str| {
        root.read(format!("/proc/pressure/{name}"))
            .as_deref()
            .and_then(parse_pressure)
    };
    Sample {
        ts_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        package_w: before
            .zip(after)
            .and_then(|(b, a)| watts(b, a, read_f64(root, RAPL_RANGE), window)),
        cpu_temp_c: cpu_temp(root),
        cpu_mhz: cpu_mhz(root),
        gpu: gpu(root),
        cpu_pressure: psi("cpu"),
        memory_pressure: psi("memory"),
        io_pressure: psi("io"),
        root: root_part,
    }
}

/// The sample in `OpenMetrics` text (`GET /v1/metrics`), for Prometheus or Grafana.
#[must_use]
pub fn openmetrics(s: &Sample) -> String {
    let mut lines = Vec::new();
    let mut gauge = |name: &str, help: &str, values: Vec<(String, String)>| {
        if !values.is_empty() {
            lines.push(format!("# TYPE legionctl_{name} gauge"));
            lines.push(format!("# HELP legionctl_{name} {help}"));
            lines.extend(
                values
                    .into_iter()
                    .map(|(labels, v)| format!("legionctl_{name}{labels} {v}")),
            );
        }
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
