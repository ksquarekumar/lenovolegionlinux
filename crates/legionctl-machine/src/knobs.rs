//! The knob catalog (note 16, `legionctl knobs`): every value legionctl reads or writes on this machine, with its
//! current value, the value the configsets want now and which layers set it, the firmware's range, and the policy:
//! `enforce` (legionctl restores it), `follow` (a firmware or user change is accepted, Fn+Q) or `observe` (only read).

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::configset::{Link, Merged};
use crate::ledger::Key;
use crate::policy::Machine;
use crate::policy::Step;
use crate::profile::{Profile, text};
use crate::sysroot::SysRoot;

const FIRMWARE_ATTRIBUTES: &str = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes";
/// The `sched_ext` scheduler that runs now (`scx_lavd`, ...); missing when the kernel scheduler runs.
const SCHED_EXT: &str = "/sys/kernel/sched_ext/root/ops";
const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
const PPD_PROFILES: [&str; 3] = ["performance", "balanced", "power-saver"];
/// The knobs that are actions, not files: their only values.
const IDLE_STATES: [&str; 1] = ["enabled"];
const NVIDIA_SERVICES: [&str; 2] = ["start", "unchanged"];
const RUNTIME_PM: [&str; 2] = ["auto", "on"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KnobPolicy {
    Enforce,
    Follow,
    Observe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Knob {
    /// `platform_profile`, `cpu.governor`, `sysctl.vm.swappiness`, `firmware.ppt_pl1_spl`, ...
    pub id: String,
    /// The file; `*` for a per-CPU knob (the value sums the CPUs up).
    pub path: String,
    pub value: Option<String>,
    /// What the configsets want now; `None` for a knob legionctl does not set.
    pub desired: Option<String>,
    pub min: Option<String>,
    pub max: Option<String>,
    pub choices: Vec<String>,
    pub policy: KnobPolicy,
    /// The value the user holds it at (`knobs hold`, `undo`); the policy leaves it alone until released.
    pub held: Option<String>,
    /// Every layer that sets it now, in merge order; the last one wins (unless it is held).
    pub chain: Vec<Link>,
}

impl Knob {
    fn new(
        id: impl Into<String>,
        path: impl Into<String>,
        value: Option<String>,
        policy: KnobPolicy,
    ) -> Knob {
        Knob {
            id: id.into(),
            path: path.into(),
            value,
            desired: None,
            min: None,
            max: None,
            choices: Vec::new(),
            policy,
            held: None,
            chain: Vec::new(),
        }
    }

    fn wants(mut self, wanted: &Merged) -> Knob {
        if let Some(desired) = wanted.text(&self.id) {
            self.desired = Some(desired);
            self.policy = KnobPolicy::Enforce;
            self.chain = wanted.chain.get(&self.id).cloned().unwrap_or_default();
        }
        self
    }

    fn choices(mut self, choices: &[&str]) -> Knob {
        self.choices = choices.iter().map(|c| (*c).to_string()).collect();
        self
    }
}

/// A per-CPU value as one: the value when every CPU agrees, else `a×n b×m`.
fn per_cpu(m: &Machine, file: &str) -> anyhow::Result<(String, Option<String>)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for policy in m.root.children(CPUFREQ, "policy")? {
        if let Some(v) = m.root.read(policy.join(file))? {
            *counts.entry(v).or_default() += 1;
        }
    }
    let path = format!("{CPUFREQ}/policy*/{file}");
    let value = match counts.len() {
        0 => None,
        1 => counts.into_keys().next(),
        _ => Some(
            counts
                .iter()
                .map(|(v, n)| format!("{v}×{n}"))
                .collect::<Vec<_>>()
                .join(" "),
        ),
    };
    Ok((path, value))
}

/// The values the machine offers in a whitespace-separated list file; empty when it does not publish one.
fn offered(root: &SysRoot, path: impl AsRef<Path>) -> anyhow::Result<Vec<String>> {
    Ok(root
        .read(path)?
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect())
}

/// The firmware's tunables (`lenovo-wmi-other`), with their range.
fn firmware_attributes(m: &Machine, wanted: &Merged) -> anyhow::Result<Vec<Knob>> {
    let mut knobs = Vec::new();
    for attribute in m.root.children(FIRMWARE_ATTRIBUTES, "")? {
        let Some(name) = attribute
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            continue;
        };
        let read = |f: &str| m.root.read(attribute.join(f));
        let mut knob = Knob::new(
            format!("firmware.{name}"),
            attribute.join("current_value").display().to_string(),
            read("current_value")?,
            KnobPolicy::Observe,
        )
        .wants(wanted);
        knob.min = read("min_value")?;
        knob.max = read("max_value")?;
        knob.choices = read("possible_values")?
            .map(|v| v.split(';').map(String::from).collect())
            .unwrap_or_default();
        knobs.push(knob);
    }
    Ok(knobs)
}

/// The CPU's power knobs.
fn cpu(m: &Machine, wanted: &Merged) -> anyhow::Result<Vec<Knob>> {
    let mut knobs = Vec::new();
    for (id, file, choices) in [
        (
            "cpu.governor",
            "scaling_governor",
            "scaling_available_governors",
        ),
        (
            "cpu.epp",
            "energy_performance_preference",
            "energy_performance_available_preferences",
        ),
    ] {
        let (path, value) = per_cpu(m, file)?;
        let mut knob = Knob::new(id, path, value, KnobPolicy::Observe).wants(wanted);
        knob.choices = offered(m.root, format!("{CPUFREQ}/policy0/{choices}"))?;
        knobs.push(knob);
    }
    for (id, path) in [
        (
            "cpu.no_turbo",
            "/sys/devices/system/cpu/intel_pstate/no_turbo",
        ),
        (
            "cpu.max_perf_pct",
            "/sys/devices/system/cpu/intel_pstate/max_perf_pct",
        ),
        (
            "cpu.ignore_ppc",
            "/sys/module/processor/parameters/ignore_ppc",
        ),
        (
            "cpu.energy_perf_bias",
            "/sys/devices/system/cpu/cpu0/power/energy_perf_bias",
        ),
    ] {
        knobs.push(Knob::new(id, path, m.root.read(path)?, KnobPolicy::Observe).wants(wanted));
    }
    Ok(knobs)
}

/// The knobs of `gpu power` (so that `undo` may hold it) and of the keyboard light.
fn parity_knobs(m: &Machine, wanted: &Merged) -> anyhow::Result<Vec<Knob>> {
    let mut knobs = Vec::new();
    if let Some(device) = crate::vfio::nvidia(m.root)?.and_then(|g| g.into_iter().next()) {
        let path = crate::gpu::control_path(&device);
        knobs.push(
            Knob::new(
                "nvidia.runtime_pm",
                &path,
                m.root.read(&path)?,
                KnobPolicy::Observe,
            )
            .choices(&RUNTIME_PM),
        );
    }
    knobs.push(
        Knob::new(
            "rgb.profile",
            format!("OpenRGB at {}", crate::rgb::address()),
            m.ledger.get(Key::RgbProfile)?,
            KnobPolicy::Observe,
        )
        .wants(wanted),
    );
    Ok(knobs)
}

/// Every knob, read now.
///
/// # Errors
/// A knob's file or the ledger cannot be read.
pub fn catalog(m: &Machine) -> anyhow::Result<Vec<Knob>> {
    let paths = &m.profile.paths;
    let fn_q = m.ledger.get(Key::Override)?;
    let wanted = m.desired(m.source()?, fn_q.as_deref())?;
    let mut knobs = Vec::new();

    let mut profile = Knob::new(
        "platform_profile",
        &paths.platform_profile,
        m.root.read(&paths.platform_profile)?,
        KnobPolicy::Follow,
    );
    profile.choices = offered(m.root, &paths.platform_profile_choices)?;
    profile.desired = match fn_q.clone() {
        Some(mode) => Some(mode),
        None => m.platform_choice(&wanted)?,
    };
    profile.chain = wanted
        .chain
        .get("platform_profile")
        .cloned()
        .unwrap_or_default();
    knobs.push(profile);

    knobs.extend(cpu(m, &wanted)?);
    knobs.push(Knob::new(
        "sched_ext",
        SCHED_EXT,
        m.root.read(SCHED_EXT)?,
        KnobPolicy::Observe,
    ));
    knobs.push(
        Knob::new(
            "ppd",
            "power-profiles-daemon",
            m.ppd_current.clone(),
            KnobPolicy::Observe,
        )
        .choices(&PPD_PROFILES)
        .wants(&wanted),
    );

    for (key, _) in wanted.under("sysctl.") {
        let path = format!("/proc/sys/{}", key.replace('.', "/"));
        knobs.push(
            Knob::new(
                format!("sysctl.{key}"),
                &path,
                m.root.read(&path)?,
                KnobPolicy::Observe,
            )
            .wants(&wanted),
        );
    }

    knobs.extend(firmware_attributes(m, &wanted)?);
    if let Some(fan) = &paths.fan_mode {
        knobs.push(
            Knob::new(
                "firmware.fan_mode",
                fan,
                m.root.read(fan)?,
                KnobPolicy::Observe,
            )
            .wants(&wanted),
        );
    }
    // actions, not files: nothing to read back
    knobs.push(
        Knob::new(
            "cpu.idle_states",
            "cpupower idle-set -E",
            None,
            KnobPolicy::Observe,
        )
        .choices(&IDLE_STATES)
        .wants(&wanted),
    );
    knobs.push(
        Knob::new(
            "nvidia.services",
            "nvidia-persistenced, nvidia-powerd, nvidia-smi -pm 1",
            None,
            KnobPolicy::Observe,
        )
        .choices(&NVIDIA_SERVICES)
        .wants(&wanted),
    );
    knobs.extend(parity_knobs(m, &wanted)?);
    let holds = m.ledger.holds()?;
    for knob in &mut knobs {
        // a hold on the knob's path, or on one CPU's file of a per-CPU knob
        knob.held = holds
            .iter()
            .find(|(path, _)| crate::policy::held_by(&knob.path, path))
            .map(|(_, value)| value.clone());
    }
    Ok(knobs)
}

fn one_of(id: &str, scalar: &str, choices: &[&str]) -> Result<(), String> {
    if choices.contains(&scalar) {
        Ok(())
    } else {
        Err(format!(
            "{id}: {scalar} is not one of {}",
            choices.join(", ")
        ))
    }
}

fn within(id: &str, scalar: &str, low: i64, high: i64) -> Result<(), String> {
    match scalar.parse::<i64>() {
        Ok(n) if (low..=high).contains(&n) => Ok(()),
        _ => Err(format!("{id}: {scalar} is not a number in {low}..{high}")),
    }
}

/// A firmware attribute (`firmware.<name>`): it exists, and the value is within its range or choices.
fn check_firmware(root: Option<&SysRoot>, id: &str, scalar: &str) -> Result<(), String> {
    let Some(name) = id
        .strip_prefix("firmware.")
        .filter(|n| !n.is_empty() && !n.contains('/'))
    else {
        return Err(format!("unknown knob {id} (see legionctl knobs list)"));
    };
    let Some(r) = root else {
        return Ok(());
    };
    let base = format!("{FIRMWARE_ATTRIBUTES}/{name}");
    let unreadable = |e: anyhow::Error| format!("{id}: {e:#}");
    if !r
        .exists(format!("{base}/current_value"))
        .map_err(unreadable)?
    {
        return Err(format!("{id}: the firmware has no attribute {name}"));
    }
    let bound = |f: &str| -> Result<Option<i64>, String> {
        Ok(r.read(format!("{base}/{f}"))
            .map_err(unreadable)?
            .and_then(|v| v.parse::<i64>().ok()))
    };
    match (bound("min_value")?, bound("max_value")?) {
        // min=max=0: the firmware gives no range
        (Some(low), Some(high)) if high != 0 => within(id, scalar, low, high),
        _ => match r
            .read(format!("{base}/possible_values"))
            .map_err(unreadable)?
        {
            Some(values) => one_of(id, scalar, &values.split(';').collect::<Vec<_>>()),
            None => Ok(()),
        },
    }
}

/// Whether a layer may set knob `id` to `value` on this machine: a knob of the catalog, a value of the right kind and
/// within the knob's choices or the firmware's range. Without `root` (`validate`), only what needs no machine.
///
/// # Errors
/// What is wrong, as a sentence for `doctor` and `validate`.
pub fn check(
    root: Option<&SysRoot>,
    profile: &Profile,
    id: &str,
    value: &Value,
) -> Result<(), String> {
    let unreadable = |e: anyhow::Error| format!("{id}: {e:#}");
    let scalar = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Array(list)
            if id == "platform_profile"
                && !list.is_empty()
                && list
                    .iter()
                    .all(|v| v.as_str().is_some_and(|s| !s.is_empty())) =>
        {
            let offered = root
                .map(|r| offered(r, &profile.paths.platform_profile_choices))
                .transpose()
                .map_err(unreadable)?
                .unwrap_or_default();
            let any = list
                .iter()
                .any(|v| offered.iter().any(|c| Some(c.as_str()) == v.as_str()));
            return if offered.is_empty() || any {
                Ok(())
            } else {
                Err(format!(
                    "platform_profile: none of {} is offered here ({})",
                    text(value),
                    offered.join(", ")
                ))
            };
        }
        _ => return Err(format!("{id}: {value} is not a text or a number")),
    };
    let published = |file: String| -> Result<(), String> {
        let o = root
            .map(|r| offered(r, file))
            .transpose()
            .map_err(unreadable)?
            .unwrap_or_default();
        if o.is_empty() || o.contains(&scalar) {
            Ok(())
        } else {
            Err(format!("{id}: {scalar} is not one of {}", o.join(", ")))
        }
    };
    match id {
        // one mode as text is a list of one
        "platform_profile" => check(root, profile, id, &Value::Array(vec![value.clone()])),
        "cpu.governor" => published(format!("{CPUFREQ}/policy0/scaling_available_governors")),
        "cpu.epp" => published(format!(
            "{CPUFREQ}/policy0/energy_performance_available_preferences"
        )),
        "cpu.no_turbo" | "cpu.ignore_ppc" => one_of(id, &scalar, &["0", "1"]),
        "cpu.max_perf_pct" => within(id, &scalar, 1, 100),
        "cpu.energy_perf_bias" => within(id, &scalar, 0, 15),
        "cpu.idle_states" => one_of(id, &scalar, &IDLE_STATES),
        "nvidia.services" => one_of(id, &scalar, &NVIDIA_SERVICES),
        "ppd" => one_of(id, &scalar, &PPD_PROFILES),
        // the profiles are OpenRGB's; a missing one is a warning when the policy loads it
        "rgb.profile" if !scalar.is_empty() => Ok(()),
        "firmware.fan_mode" if profile.paths.fan_mode.is_some() => Ok(()),
        _ => match id.strip_prefix("sysctl.") {
            Some(key) => {
                let path = format!("/proc/sys/{}", key.replace('.', "/"));
                let there = match root {
                    Some(r) => r.exists(&path).map_err(unreadable)?,
                    None => true,
                };
                if !key.is_empty() && !key.contains('/') && there {
                    Ok(())
                } else {
                    Err(format!("{id}: there is no {path}"))
                }
            }
            None => check_firmware(root, id, &scalar),
        },
    }
}

/// Hold each path (a knob's path, or one of the files of a per-CPU knob) at its value against the policy (D1): the
/// writes now, then the holds. A value outside the knob's choices or range is refused.
///
/// # Errors
/// A path is no knob of this machine, or a value is out of range.
pub fn hold_plan(
    m: &Machine,
    values: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<Vec<Step>> {
    let knobs = catalog(m)?;
    // besides the knobs, the files the policy itself writes for either power source: only these may be held, since
    // the daemon writes them as root
    let mut policy_files = std::collections::BTreeSet::new();
    let all = crate::policy::Options {
        force: true,
        ..crate::policy::Options::default()
    };
    for source in [crate::policy::Source::Ac, crate::policy::Source::Battery] {
        for step in m.apply(Some(source), all)?.1 {
            if let Step::Write { path, .. } = step {
                policy_files.insert(path);
            }
        }
    }
    let mut steps = Vec::new();
    for (path, value) in values {
        let Some(knob) = knobs.iter().find(|k| crate::policy::held_by(&k.path, path)) else {
            anyhow::ensure!(
                policy_files.contains(path),
                "{path} is neither a knob of this machine nor a file its policy writes (see legionctl knobs list)"
            );
            steps.push(Step::Write {
                path: path.clone(),
                value: value.clone(),
            });
            steps.push(Step::Hold {
                path: path.clone(),
                value: Some(value.clone()),
            });
            continue;
        };
        anyhow::ensure!(
            knob.choices.is_empty() || knob.choices.contains(value),
            "{}: {value} is not one of {}",
            knob.id,
            knob.choices.join(", ")
        );
        let number = value.parse::<f64>().ok();
        for (bound, below) in [(&knob.min, true), (&knob.max, false)] {
            if let (Some(bound), Some(n)) =
                (bound.as_ref().and_then(|b| b.parse::<f64>().ok()), number)
            {
                anyhow::ensure!(
                    if below { n >= bound } else { n <= bound },
                    "{}: {value} is outside {}..{}",
                    knob.id,
                    knob.min.as_deref().unwrap_or("?"),
                    knob.max.as_deref().unwrap_or("?")
                );
            }
        }
        // a per-CPU knob: each CPU policy's file
        if let Some((head, tail)) = path.split_once('*') {
            for policy in m.root.children(
                head.trim_end_matches("policy").trim_end_matches('/'),
                "policy",
            )? {
                let file = format!("{}{}", policy.display(), tail);
                steps.push(Step::Write {
                    path: file,
                    value: value.clone(),
                });
            }
        } else {
            steps.push(Step::Write {
                path: path.clone(),
                value: value.clone(),
            });
        }
        steps.push(Step::Hold {
            path: path.clone(),
            value: Some(value.clone()),
        });
    }
    Ok(steps)
}

/// The knob `id` (or its path), if this machine has it.
///
/// # Errors
/// The knobs cannot be read.
pub fn find(m: &Machine, id: &str) -> anyhow::Result<Option<Knob>> {
    Ok(catalog(m)?.into_iter().find(|k| k.id == id || k.path == id))
}
