//! The tuning engine (note 06): step one firmware limit through a list of values, run the same workload at each,
//! and recommend the knee: the smallest value within 2 % of the best score, without thermal throttling in the steady
//! half of the step (and, under mixed load, with the GPU within 2 % of its best). `legionctl-system` runs it as root;
//! the original value comes back on every exit.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bench::{self, Scenario, Scores, Summary};
use crate::policy::{self, Step};
use crate::sysroot::SysRoot;
use crate::telemetry::Sample;

const ATTRIBUTES: &str = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes";
const AC: &str = "/sys/class/power_supply/ADP0/online";
/// The limits a sweep may step (note 06).
pub const TUNABLE: [&str; 5] = [
    "ppt_pl1_spl",
    "ppt_pl2_sppt",
    "ppt_cpu_cl",
    "cpu_temp",
    "gpu_nv_ac_offset",
];
/// Within this share of the best score counts as the same.
const KNEE: f64 = 0.98;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TuneSpec {
    /// A firmware attribute from [`TUNABLE`].
    pub attribute: String,
    /// The values to step through, each within the firmware's range.
    pub values: Vec<u32>,
    pub scenario: Scenario,
    /// Duration of each step (1-600).
    pub seconds: u32,
    /// Pause between steps, so that each starts from a similar temperature (0-600).
    pub cooldown_s: u32,
    /// The workload commands, resolved by the caller; they run as the caller.
    pub workloads: Vec<Vec<String>>,
    /// The recommendation goes into a layer for the running kernel only (`50-tuned-<package>`).
    pub per_kernel: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TuneStep {
    pub value: u32,
    pub scores: BTreeMap<String, f64>,
    pub problems: Vec<String>,
    pub summary: Summary,
    /// Throttle reasons in the second half of the step.
    pub steady_throttle: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TuneReport {
    pub attribute: String,
    pub scenario: Scenario,
    /// The value before the sweep; it is written back at the end.
    pub original: String,
    pub steps: Vec<TuneStep>,
    pub recommended: Option<u32>,
    /// Why this value, or why none.
    pub reason: String,
    /// Why the sweep stopped early.
    pub aborted: Option<String>,
    /// The results file (machine path).
    pub file: String,
}

fn attribute_file(attribute: &str, file: &str) -> String {
    format!("{ATTRIBUTES}/{attribute}/{file}")
}

/// Refuse a spec that the machine cannot run safely: unknown attribute, values outside the firmware range, on
/// battery.
///
/// # Errors
/// The reason the spec is refused.
pub fn check(root: &SysRoot, spec: &TuneSpec) -> Result<()> {
    ensure!(
        TUNABLE.contains(&spec.attribute.as_str()),
        "attribute must be one of {}",
        TUNABLE.join(", ")
    );
    ensure!(
        !spec.values.is_empty() && spec.values.len() <= 20,
        "give 1-20 values"
    );
    ensure!((1..=600).contains(&spec.seconds), "seconds must be 1-600");
    ensure!(spec.cooldown_s <= 600, "cooldown_s must be 0-600");
    ensure!(!spec.workloads.is_empty(), "no workload");
    let bound = |f: &str| -> Result<u32> {
        root.read(attribute_file(&spec.attribute, f))
            .with_context(|| format!("{} has no {f}", spec.attribute))?
            .parse()
            .with_context(|| format!("{} {f} is not a number", spec.attribute))
    };
    let (min, max) = (bound("min_value")?, bound("max_value")?);
    // the firmware reports 0..0 for a value it gives no range for: then it is not safe to step
    ensure!(
        min < max,
        "{} has no firmware range ({min}..{max})",
        spec.attribute
    );
    if let Some(v) = spec.values.iter().find(|v| !(min..=max).contains(*v)) {
        bail!(
            "{v} is outside the firmware range of {} ({min}..{max})",
            spec.attribute
        );
    }
    ensure!(
        root.read(AC).as_deref() == Some("1"),
        "tuning runs on AC only"
    );
    Ok(())
}

/// Why a step must stop now: the power source changed, or the CPU is hotter than the firmware's own limit.
fn danger(root: &SysRoot, sample: &Sample) -> Option<String> {
    if root.read(AC).as_deref() != Some("1") {
        return Some("the machine left AC".into());
    }
    let limit: f64 = root
        .read(attribute_file("cpu_temp", "current_value"))?
        .parse()
        .ok()?;
    let temp = sample.cpu_temp_c?;
    (temp > limit)
        .then(|| format!("CPU at {temp:.0} °C, above the firmware limit of {limit:.0} °C"))
}

fn primary(scenario: Scenario) -> &'static str {
    match scenario {
        Scenario::Gpu => "gpu Gflop/s",
        Scenario::Cpu | Scenario::Mixed => "cpu bogo ops/s",
    }
}

/// The knee and why; `None` when no step qualifies.
#[must_use]
pub fn recommend(scenario: Scenario, steps: &[TuneStep]) -> (Option<u32>, String) {
    let key = primary(scenario);
    let best_gpu = steps
        .iter()
        .filter_map(|s| s.scores.get("gpu Gflop/s").copied())
        .reduce(f64::max);
    let clean: Vec<&TuneStep> = steps
        .iter()
        .filter(|s| s.problems.is_empty() && s.scores.contains_key(key))
        .collect();
    if clean.is_empty() {
        return (None, format!("no step has a {key} score without problems"));
    }
    let cool: Vec<&TuneStep> = clean
        .iter()
        .copied()
        .filter(|s| !s.steady_throttle.iter().any(|r| r == "thermal"))
        .filter(
            |s| match (scenario, best_gpu, s.scores.get("gpu Gflop/s")) {
                (Scenario::Mixed, Some(best), Some(g)) => *g >= best * KNEE,
                _ => true,
            },
        )
        .collect();
    if cool.is_empty() {
        return (
            None,
            "every step throttles thermally in its steady half: heat limits this machine, not the power limit".into(),
        );
    }
    let best = cool.iter().map(|s| s.scores[key]).fold(f64::MIN, f64::max);
    let knee = cool
        .iter()
        .filter(|s| s.scores[key] >= best * KNEE)
        .min_by_key(|s| s.value)
        .map(|s| s.value);
    (
        knee,
        format!(
            "the smallest value within 2 % of the best {key} ({best:.0}) without thermal throttling"
        ),
    )
}

/// Runs one workload command and returns whether it succeeded, with its output.
pub type Workload<'a> = dyn Fn(&[String], &AtomicBool) -> Result<(bool, String)> + Sync + 'a;

/// Run `argv` in its own process group, as `user` (uid, gid) when given, and kill the group when `cancel` is set.
///
/// # Errors
/// The command does not start.
pub fn run_workload(
    argv: &[String],
    user: Option<(u32, u32)>,
    cancel: &AtomicBool,
) -> Result<(bool, String)> {
    let mut cmd = if let Some((uid, gid)) = user {
        let mut c = Command::new("setpriv");
        c.args([
            format!("--reuid={uid}"),
            format!("--regid={gid}"),
            "--init-groups".into(),
            "--reset-env".into(),
        ])
        .arg("--")
        .args(argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
    };
    let mut child = cmd
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", argv[0]))?;
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            text
        })
    };
    let out = drain(Box::new(child.stdout.take().context("no stdout")?));
    let err = drain(Box::new(child.stderr.take().context("no stderr")?));
    let mut killed = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel.load(Ordering::Relaxed) && !killed {
            // the whole group: stress-ng forks a worker per CPU, and a worker holds the pipes open
            let group = rustix::process::Pid::from_raw(i32::try_from(child.id())?)
                .context("no process id")?;
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
            killed = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let text = out.join().unwrap_or_default() + &err.join().unwrap_or_default();
    Ok((status.success() && !killed, text))
}

/// Run the sweep. `workload` runs one command ([`run_workload`]); `sample` takes one telemetry sample over a
/// second. The original value is written back whatever happens.
///
/// # Errors
/// The spec is refused, or the results file cannot be written.
pub fn run(
    root: &SysRoot,
    spec: &TuneSpec,
    workload: &Workload,
    sample: &dyn Fn() -> Sample,
) -> Result<TuneReport> {
    check(root, spec)?;
    let kernel = if spec.per_kernel {
        Some(crate::kernel::running_package(root).context(
            "the running kernel has no package (/usr/lib/modules/<release>/pkgbase): no layer for it",
        )?)
    } else {
        None
    };
    let path = attribute_file(&spec.attribute, "current_value");
    let original = root.read(&path).context("reading the current value")?;
    let ledger = crate::ledger::Ledger::open(root)?;
    let set = |value: &str| {
        policy::execute(
            root,
            &ledger,
            &[Step::Write {
                path: path.clone(),
                value: value.into(),
            }],
        )
    };
    let mut steps = Vec::new();
    let mut aborted = None;
    for (i, value) in spec.values.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(Duration::from_secs(u64::from(spec.cooldown_s)));
        }
        let lines = set(&value.to_string());
        if let Some(warning) = lines.iter().find(|l| l.starts_with("warning")) {
            aborted = Some(format!("writing {value}: {warning}"));
            break;
        }
        let (done, stop) = step(root, spec, *value, workload, sample);
        steps.push(done);
        if stop.is_some() {
            aborted = stop;
            break;
        }
    }
    set(&original);
    let (recommended, reason) = recommend(spec.scenario, &steps);
    let file = format!(
        "/var/lib/legionctl/tune/{}-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        spec.attribute
    );
    let report = TuneReport {
        attribute: spec.attribute.clone(),
        scenario: spec.scenario,
        original,
        steps,
        recommended,
        reason,
        aborted,
        file: file.clone(),
    };
    let target = root.path(&file);
    std::fs::create_dir_all(target.parent().context("no parent")?)?;
    std::fs::write(&target, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", target.display()))?;
    if let Some(value) = recommended {
        tuned(root, &spec.attribute, value, kernel.as_deref())?;
    }
    Ok(report)
}

/// Put a recommended value into the generated layer `tuned` (`/var/lib/legionctl/configsets/50-tuned.jsonc`), for AC
/// (the sweep runs on AC only), beside the values of earlier sweeps; with `kernel`, into that kernel's own layer
/// `tuned-<kernel>` (`50-tuned-<kernel>.jsonc`, `"when": {"kernel": ...}` too). The layer is off until `legionctl
/// rules enable`.
fn tuned(root: &SysRoot, attribute: &str, value: u32, kernel: Option<&str>) -> Result<()> {
    let name = kernel.map_or_else(|| "tuned".to_string(), |k| format!("tuned-{k}"));
    let file = root.path(format!("{}/50-{name}.jsonc", crate::configset::GENERATED));
    let mut set = std::fs::read(&file)
        .ok()
        .and_then(|bytes| {
            serde_json::from_reader::<_, serde_json::Value>(json_comments::StripComments::new(
                bytes.as_slice(),
            ))
            .ok()
        })
        .and_then(|layer| layer.get("set")?.as_object().cloned())
        .unwrap_or_default();
    set.insert(format!("firmware.{attribute}"), value.into());
    let mut when = serde_json::json!({ "power": "ac" });
    if let Some(kernel) = kernel {
        when["kernel"] = kernel.into();
    }
    let layer = serde_json::json!({ "when": when, "set": set });
    std::fs::create_dir_all(file.parent().context("no parent")?)?;
    std::fs::write(
        &file,
        format!(
            "// Made by legionctl tune: the recommended firmware limits on AC{}. Off until `legionctl rules enable {name}`.\n{}\n",
            kernel.map(|k| format!(" with {k}")).unwrap_or_default(),
            serde_json::to_string_pretty(&layer)?
        ),
    )
    .with_context(|| format!("writing {}", file.display()))
}

/// One step: the workloads in threads, a sample each second until they end (or a danger stops the step).
fn step(
    root: &SysRoot,
    spec: &TuneSpec,
    value: u32,
    workload: &Workload,
    sample: &dyn Fn() -> Sample,
) -> (TuneStep, Option<String>) {
    let mut samples = Vec::new();
    let mut stop = None;
    let mut scores = Scores::default();
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let cancel = &cancel;
        let runs: Vec<_> = spec
            .workloads
            .iter()
            .map(|argv| (argv, scope.spawn(move || workload(argv, cancel))))
            .collect();
        while runs.iter().any(|(_, r)| !r.is_finished()) {
            let s = sample();
            if stop.is_none() {
                stop = danger(root, &s);
                cancel.store(stop.is_some(), Ordering::Relaxed);
            }
            samples.push(s);
        }
        for (argv, run) in runs {
            let program = argv[0].rsplit('/').next().unwrap_or_default();
            match run.join() {
                Ok(Ok((ok, text))) => {
                    if program == "gpu-burn" {
                        bench::gpu_burn(&text, &mut scores);
                    } else {
                        bench::stress_ng(&text, &mut scores);
                    }
                    if !ok {
                        scores.problems.push(format!("{program} failed"));
                    }
                }
                Ok(Err(e)) => scores.problems.push(format!("{program}: {e:#}")),
                Err(_) => scores.problems.push(format!("{program} panicked")),
            }
        }
    });
    let steady = bench::summarize(&samples[samples.len() / 2..]);
    (
        TuneStep {
            value,
            scores: scores.scores,
            problems: scores.problems,
            summary: bench::summarize(&samples),
            steady_throttle: steady.throttle,
        },
        stop,
    )
}
