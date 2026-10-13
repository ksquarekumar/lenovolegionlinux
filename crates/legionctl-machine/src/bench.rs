//! Repeatable workloads with telemetry (note 16, `legionctl bench`): stress-ng on every CPU, gpu-burn on the NVIDIA
//! GPU, or both. A run keeps its scores, a summary of the samples taken during it, and the knob values it ran with,
//! so that `bench compare` shows what changed between two runs (two power limits, two `sched_ext` schedulers).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::telemetry::Sample;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scenario {
    Cpu,
    Gpu,
    Mixed,
}

impl Scenario {
    /// The workload commands for `seconds`.
    #[must_use]
    pub fn commands(self, seconds: u32) -> Vec<Vec<String>> {
        let cpu = ["stress-ng", "--cpu", "0", "--metrics-brief", "--timeout"]
            .map(String::from)
            .into_iter()
            .chain([format!("{seconds}s")])
            .collect();
        let gpu = vec!["gpu-burn".into(), seconds.to_string()];
        match self {
            Scenario::Cpu => vec![cpu],
            Scenario::Gpu => vec![gpu],
            Scenario::Mixed => vec![cpu, gpu],
        }
    }
}

/// The samples of a run, summed up.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    pub samples: usize,
    pub package_w_mean: Option<f64>,
    pub package_w_max: Option<f64>,
    pub cpu_temp_max: Option<f64>,
    pub cpu_mhz_mean: Option<f64>,
    pub gpu_w_mean: Option<f64>,
    pub gpu_temp_max: Option<f64>,
    pub fan_rpm_max: Option<u64>,
    /// Throttle reasons active in any sample.
    pub throttle: Vec<String>,
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| {
        values.iter().sum::<f64>() / f64::from(u32::try_from(values.len()).unwrap_or(u32::MAX))
    })
}

fn max(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(f64::max)
}

#[must_use]
pub fn summarize(samples: &[Sample]) -> Summary {
    let all = |f: fn(&Sample) -> Option<f64>| samples.iter().filter_map(f).collect::<Vec<f64>>();
    let package = all(|s| s.package_w);
    let gpu_w = all(|s| s.gpu.as_ref()?.power_w);
    Summary {
        samples: samples.len(),
        package_w_mean: mean(&package),
        package_w_max: max(&package),
        cpu_temp_max: max(&all(|s| s.cpu_temp_c)),
        cpu_mhz_mean: mean(&all(|s| s.cpu_mhz)),
        gpu_w_mean: mean(&gpu_w),
        gpu_temp_max: max(&all(|s| s.gpu.as_ref()?.temp_c)),
        fan_rpm_max: samples
            .iter()
            .filter_map(|s| s.root.fan_rpm.as_ref()?.values().max().copied())
            .max(),
        throttle: samples
            .iter()
            .filter_map(|s| s.root.throttle_now.clone())
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}

/// What a run stores in the event log (`bench.result`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BenchResult {
    pub scenario: Scenario,
    pub label: Option<String>,
    pub seconds: u32,
    pub scores: BTreeMap<String, f64>,
    /// Failed stressors, GPU computation errors, a workload that exited with an error.
    pub problems: Vec<String>,
    pub summary: Summary,
    /// Every knob's value when the run started.
    pub knobs: BTreeMap<String, Option<String>>,
}

/// Scores and problems from the workloads' output.
#[derive(Debug, Default, PartialEq)]
pub struct Scores {
    pub scores: BTreeMap<String, f64>,
    pub problems: Vec<String>,
}

/// stress-ng `--metrics-brief`: the `bogo ops/s (real time)` of each stressor, and `failed: N`.
pub fn stress_ng(output: &str, into: &mut Scores) {
    for line in output.lines() {
        let Some((_, rest)) = line.split_once("] ") else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if line.contains(" metrc: ")
            && let [stressor, _, _, _, _, per_second, _] = fields[..]
            && let Ok(v) = per_second.parse()
        {
            into.scores.insert(format!("{stressor} bogo ops/s"), v);
        }
        if let ["failed:", n] = fields[..]
            && n != "0"
        {
            into.problems
                .push(format!("stress-ng: {n} stressors failed"));
        }
    }
}

/// gpu-burn: the last progress line (`100.0%  proc'd: 68 (21427 Gflop/s)   errors: 0`, one group per GPU).
pub fn gpu_burn(output: &str, into: &mut Scores) {
    let Some(last) = output.split(['\r', '\n']).rfind(|l| l.contains("Gflop/s)")) else {
        into.problems.push("gpu-burn: no progress line".into());
        return;
    };
    let gflops: f64 = last
        .split('(')
        .skip(1)
        .filter_map(|g| g.split_once(" Gflop/s)")?.0.trim().parse::<f64>().ok())
        .sum();
    into.scores.insert("gpu Gflop/s".into(), gflops);
    let errors = last
        .split("errors:")
        .skip(1)
        .filter_map(|e| e.split_whitespace().next()?.parse::<u64>().ok())
        .sum::<u64>();
    if errors > 0 {
        into.problems.push(format!(
            "gpu-burn: {errors} computation errors (the GPU computes wrong results)"
        ));
    }
}
