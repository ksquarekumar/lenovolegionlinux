//! One sensors view (`legionctl sensors`, note 16): every reading of a telemetry sample as a row with its unit and
//! source, the state that goes with them (fan controls, firmware mode, workload, power source), and each reading's
//! history from the earlier samples in the operator's telemetry ring. It takes no reading of its own.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::fan::{FanState, text};
use crate::firmware::LEGIONCTL_WMI;
use crate::sysroot::SysRoot;
use crate::telemetry::Sample;
use crate::{battery, configset};

/// One row: a number with its unit, or a state as text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reading {
    /// Where it comes from: a hwmon device by what it is (`CPU`, `NVMe <model>`, `memory DIMM 0`, `battery`), `RAPL`,
    /// `nvidia-smi`, `firmware acpi_call`, `eBPF runq`, ...
    pub source: String,
    pub name: String,
    pub value: Option<f64>,
    /// A state (`suspended`, `thermal, power limit`); a number that cannot be read is `unknown` and why.
    pub text: Option<String>,
    pub unit: String,
    /// The values of this reading in the earlier samples of the telemetry ring, oldest first, then this one.
    pub history: Vec<f64>,
}

fn float(n: u64) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The numbers of one sample: (source, name, value, unit).
fn numbers(s: &Sample) -> Vec<(String, String, f64, String)> {
    let mut out: Vec<(String, String, f64, String)> = s
        .hwmon
        .iter()
        .map(|c| (c.device.clone(), c.name.clone(), c.value, c.unit.clone()))
        .collect();
    let mut add = |source: &str, name: &str, value: Option<f64>, unit: &str| {
        if let Some(v) = value {
            out.push((source.into(), name.into(), v, unit.into()));
        }
    };
    add("RAPL", "CPU package power", s.package_w, "W");
    add("cpufreq", "CPU clock, mean", s.cpu_mhz, "MHz");
    if let Some(g) = &s.gpu {
        add("nvidia-smi", "GPU power", g.power_w, "W");
        add("nvidia-smi", "GPU temperature", g.temp_c, "°C");
        add("nvidia-smi", "GPU utilization", g.util_percent, "%");
        add("nvidia-smi", "GPU clock", g.clock_mhz, "MHz");
    }
    for (name, p) in [
        ("CPU", &s.cpu_pressure),
        ("memory", &s.memory_pressure),
        ("I/O", &s.io_pressure),
    ] {
        add(
            "PSI",
            &format!("{name} pressure, some avg10"),
            p.as_ref().map(|p| p.some_avg10),
            "%",
        );
    }
    for (class, l) in s.runq_latency.iter().flatten() {
        add(
            "eBPF runq",
            &format!("run-queue latency p99, {class}"),
            Some(float(l.p99_us)),
            "µs",
        );
    }
    for e in s.cpu_energy.iter().flatten() {
        add(
            "eBPF power",
            &format!("CPU energy share, {} ({})", e.name, e.pid),
            Some(e.share_percent),
            "%",
        );
    }
    for (disk, l) in s.io_latency.iter().flatten() {
        add(
            "eBPF block I/O",
            &format!("I/O latency p99, {disk}"),
            Some(float(l.p99_us)),
            "µs",
        );
    }
    for (zone, n) in s.root.thermal_trips.iter().flatten() {
        add(
            "eBPF thermal",
            &format!("trip points crossed, {zone}"),
            Some(float(*n)),
            "",
        );
    }
    for (reason, kinds) in &s.root.throttle_episodes {
        for (kind, n) in kinds {
            add(
                "MSR, eBPF workload",
                &format!("throttle {reason} starts, workload {kind}"),
                Some(float(*n)),
                "",
            );
        }
    }
    for (session, p) in &s.session_memory_pressure {
        add(
            "cgroup PSI",
            &format!("memory pressure of session {session}, some avg10"),
            Some(p.some_avg10),
            "%",
        );
    }
    // the legionctl-wmi module's hwmon has the fan RPM in place of acpi_call (it is in the hwmon rows above)
    if !s
        .hwmon
        .iter()
        .any(|c| c.device.ends_with(&format!(" {LEGIONCTL_WMI}")))
    {
        for (fan, rpm) in s.root.fan_rpm.iter().flatten() {
            add("firmware acpi_call", fan, Some(float(*rpm)), "RPM");
        }
    }
    out
}

/// The view of `now`: its numbers with their history in `earlier` (oldest first), then the states. `fan` is the
/// daemon's fan state (`None` without the daemon); `ac_adapter` is the profile's AC adapter file.
///
/// # Errors
/// A machine file cannot be read.
pub fn view(
    root: &SysRoot,
    now: &Sample,
    earlier: &[Sample],
    fan: Option<&FanState>,
    ac_adapter: &str,
) -> Result<Vec<Reading>> {
    let past: Vec<Vec<(String, String, f64, String)>> = earlier.iter().map(numbers).collect();
    let mut rows: Vec<Reading> = numbers(now)
        .into_iter()
        .map(|(source, name, value, unit)| {
            let mut history: Vec<f64> = past
                .iter()
                .filter_map(|n| n.iter().find(|(s, m, ..)| *s == source && *m == name))
                .map(|(_, _, v, _)| *v)
                .collect();
            history.push(value);
            Reading {
                source,
                name,
                value: Some(value),
                text: None,
                unit,
                history,
            }
        })
        .collect();
    rows.extend(states(root, now, fan, ac_adapter)?);
    for r in &mut rows {
        r.source = device_name(root, &r.source)?;
    }
    Ok(rows)
}

/// A hwmon device (`hwmon2 nvme`) by what it is: CPU, `NVMe` and its model, memory and its DIMM slot, battery, ...;
/// another source as it is.
fn device_name(root: &SysRoot, source: &str) -> Result<String> {
    let Some((id, name)) = source
        .split_once(' ')
        .filter(|(id, _)| id.starts_with("hwmon"))
    else {
        return Ok(source.into());
    };
    let dir = std::path::Path::new("/sys/class/hwmon").join(id);
    // the device's own name in sysfs: nvme1, 15-0052
    let node = root
        .link(dir.join("device"))?
        .and_then(|l| Some(l.file_name()?.to_string_lossy().into_owned()));
    Ok(match name {
        "coretemp" | "k10temp" | "zenpower" => "CPU".into(),
        // shortcut: two drives of one model get one name; add the slot when a machine has that
        "nvme" => match root.read(dir.join("device/model"))? {
            Some(model) => format!("NVMe {}", model.replace('_', " ")),
            None => format!("NVMe {}", node.as_deref().unwrap_or(id)),
        },
        // SPD hub addresses 0x50-0x57 are the DIMM slots 0-7
        "spd5118" | "jc42" => node
            .as_deref()
            .and_then(|n| u8::from_str_radix(n.rsplit_once('-')?.1, 16).ok())
            .and_then(|addr| addr.checked_sub(0x50))
            .map_or_else(|| "memory".into(), |slot| format!("memory DIMM {slot}")),
        "legionctl_wmi" => "fans".into(),
        "nouveau" | "nvidia" | "amdgpu" | "i915" | "xe" => format!("GPU ({name})"),
        "acpitz" => "ACPI thermal zone".into(),
        _ if name.starts_with("BAT") => "battery".into(),
        _ if name.starts_with("ADP") || name.starts_with("AC") => "AC adapter".into(),
        _ if name.starts_with("iwlwifi") => "Wi-Fi".into(),
        _ if name.starts_with("ucsi_source_psy") => "USB-C".into(),
        _ => source.into(),
    })
}

/// The states that go with the numbers: sleeping devices, the GPU's power state, the root-only readings, the fan
/// controls, the workload and the power source.
fn states(
    root: &SysRoot,
    now: &Sample,
    fan: Option<&FanState>,
    ac_adapter: &str,
) -> Result<Vec<Reading>> {
    let mut rows = Vec::new();
    let mut state = |source: &str, name: &str, text: String| rows.push(state(source, name, text));
    for device in &now.hwmon_asleep {
        state(
            device,
            "device",
            "asleep, not read (a read wakes it)".into(),
        );
    }
    if let Some(g) = &now.gpu {
        state("PCI runtime PM", "NVIDIA GPU", g.state.clone());
    }
    for (sensor, e) in &now.root.sensor_errors {
        state(
            &format!("eBPF {sensor}"),
            "sensor",
            format!("not running: {e}"),
        );
    }
    let root_only = |v: &Option<Vec<String>>| {
        v.as_ref().map_or_else(
            || "unknown (needs legionctl-system)".into(),
            |r| {
                if r.is_empty() {
                    "none".into()
                } else {
                    r.join(", ")
                }
            },
        )
    };
    state(
        "MSR",
        "throttle reasons now",
        root_only(&now.root.throttle_now),
    );
    state(
        "MSR",
        "throttle reasons since boot",
        root_only(&now.root.throttle_logged),
    );
    rows.extend(machine_states(root, fan, ac_adapter)?);
    Ok(rows)
}

/// A state row.
fn state(source: &str, name: &str, text: String) -> Reading {
    Reading {
        source: source.into(),
        name: name.into(),
        value: None,
        text: Some(text),
        unit: String::new(),
        history: Vec::new(),
    }
}

/// The fan controls, the workload, the power source and the battery.
fn machine_states(
    root: &SysRoot,
    fan: Option<&FanState>,
    ac_adapter: &str,
) -> Result<Vec<Reading>> {
    let mut rows = Vec::new();
    let mut state = |source: &str, name: &str, text: String| rows.push(state(source, name, text));
    match fan {
        Some(f) => {
            state(
                "legionctl-system",
                "firmware mode",
                f.mode.clone().unwrap_or_else(|| "unknown".into()),
            );
            state(
                "legionctl-system",
                "fan max mode",
                if f.max { "on" } else { "off" }.into(),
            );
            state(
                "legionctl-system",
                "fan table",
                f.curve.as_deref().map_or_else(
                    || {
                        format!(
                            "unknown: {}",
                            f.curve_error.as_deref().unwrap_or("no reply")
                        )
                    },
                    text,
                ),
            );
        }
        None => state(
            "legionctl-system",
            "fan controls",
            "unknown (needs legionctl-system)".into(),
        ),
    }
    let kinds = configset::workload(root)?;
    state(
        "eBPF workload",
        "workload",
        if kinds.is_empty() {
            "none".into()
        } else {
            kinds.into_iter().collect::<Vec<_>>().join(", ")
        },
    );
    state(
        "power_supply",
        "power source",
        match root.read(ac_adapter)?.as_deref() {
            Some("1") => "ac".into(),
            Some(_) => "battery".into(),
            None => "unknown".into(),
        },
    );
    let b = battery::state(root)?;
    if let Some(pct) = b.capacity_pct {
        state(
            "power_supply BAT0",
            "battery",
            format!("{pct} %, {}", b.status.as_deref().unwrap_or("unknown")),
        );
    }
    Ok(rows)
}

/// The key readings of `rows` in a few short lines, for the vitals overlay: CPU and GPU temperature and power (or the
/// GPU asleep), the fans, the firmware mode and the power source. A reading that is not there shows `?`.
#[must_use]
pub fn vitals(rows: &[Reading]) -> Vec<String> {
    let number = |name: &str, unit: &str| {
        rows.iter()
            .find(|r| r.name == name && r.unit == unit)
            .and_then(|r| r.value)
            .map_or_else(|| format!("? {unit}"), |v| format!("{v:.0} {unit}"))
    };
    let text = |name: &str| {
        rows.iter()
            .find(|r| r.name == name && r.value.is_none())
            .and_then(|r| r.text.clone())
            .unwrap_or_else(|| "?".into())
    };
    let gpu = match text("NVIDIA GPU").as_str() {
        "suspended" => "GPU  asleep (D3cold)".to_string(),
        _ => format!(
            "GPU  {}  {}",
            number("GPU temperature", "°C"),
            number("GPU power", "W")
        ),
    };
    let fans: Vec<String> = rows
        .iter()
        .filter(|r| r.unit == "RPM")
        .filter_map(|r| r.value)
        .map(|v| format!("{v:.0}"))
        .collect();
    vec![
        format!(
            "CPU  {}  {}",
            number("Package id 0", "°C"),
            number("CPU package power", "W")
        ),
        gpu,
        format!(
            "Fans {} RPM",
            if fans.is_empty() {
                "?".into()
            } else {
                fans.join(" / ")
            }
        ),
        format!("{} on {}", text("firmware mode"), text("power source")),
    ]
}
