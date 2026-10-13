//! The knob catalog (note 16, `legionctl knobs`): every value legionctl reads or writes on this machine, with its
//! current value, the value the profile wants for the current power source, the firmware's range, and the policy:
//! `enforce` (legionctl restores it), `follow` (a firmware or user change is accepted, Fn+Q) or `observe` (only read).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ledger::Key;
use crate::policy::Machine;
use crate::policy::Step;
use crate::profile::{Follow, ModePolicy, text};

const FIRMWARE_ATTRIBUTES: &str = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes";
/// The `sched_ext` scheduler that runs now (`scx_lavd`, ...); missing when the kernel scheduler runs.
const SCHED_EXT: &str = "/sys/kernel/sched_ext/root/ops";
const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";

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
    /// What the profile wants now; `None` for a knob legionctl does not set.
    pub desired: Option<String>,
    pub min: Option<String>,
    pub max: Option<String>,
    pub choices: Vec<String>,
    pub policy: KnobPolicy,
    /// The value the user holds it at (`knobs hold`, `undo`); the policy leaves it alone until released.
    pub held: Option<String>,
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
        }
    }

    fn wants(mut self, desired: Option<String>) -> Knob {
        if desired.is_some() {
            self.desired = desired;
            self.policy = KnobPolicy::Enforce;
        }
        self
    }
}

/// A per-CPU value as one: the value when every CPU agrees, else `a×n b×m`.
fn per_cpu(m: &Machine, file: &str) -> (String, Option<String>) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let policies = m.root.children(CPUFREQ, "policy");
    for policy in &policies {
        if let Some(v) = m.root.read(policy.join(file)) {
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
    (path, value)
}

fn policy_for<'a>(m: &'a Machine) -> &'a ModePolicy {
    if m.ac_online() {
        &m.profile.ac
    } else {
        &m.profile.battery
    }
}

/// The firmware's tunables (`lenovo-wmi-other`), with their range.
fn firmware_attributes(m: &Machine, policy: &ModePolicy) -> Vec<Knob> {
    let mut knobs = Vec::new();
    for attribute in m.root.children(FIRMWARE_ATTRIBUTES, "") {
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
            read("current_value"),
            KnobPolicy::Observe,
        )
        .wants(policy.firmware_attributes.get(&name).map(text));
        knob.min = read("min_value");
        knob.max = read("max_value");
        knob.choices = read("possible_values")
            .map(|v| v.split(';').map(String::from).collect())
            .unwrap_or_default();
        knobs.push(knob);
    }
    knobs
}

/// The CPU's power knobs; following Fn+Q, the governor comes from `firmware_follow`.
fn cpu(m: &Machine, policy: &ModePolicy, follow: Option<&Follow>) -> Vec<Knob> {
    let mut knobs = Vec::new();
    let governor = follow
        .and_then(|f| f.governor.clone())
        .or_else(|| policy.cpu.governor.clone());
    for (id, file, desired) in [
        ("cpu.governor", "scaling_governor", governor),
        (
            "cpu.epp",
            "energy_performance_preference",
            policy.cpu.energy_performance_preference.clone(),
        ),
    ] {
        let (path, value) = per_cpu(m, file);
        knobs.push(Knob::new(id, path, value, KnobPolicy::Observe).wants(desired));
    }
    for (id, path, desired) in [
        (
            "cpu.no_turbo",
            "/sys/devices/system/cpu/intel_pstate/no_turbo",
            policy.cpu.no_turbo.clone(),
        ),
        (
            "cpu.max_perf_pct",
            "/sys/devices/system/cpu/intel_pstate/max_perf_pct",
            policy.cpu.max_perf_pct.clone(),
        ),
        (
            "cpu.ignore_ppc",
            "/sys/module/processor/parameters/ignore_ppc",
            policy.cpu.ignore_ppc.clone(),
        ),
        (
            "cpu.energy_perf_bias",
            "/sys/devices/system/cpu/cpu0/power/energy_perf_bias",
            policy.cpu.energy_perf_bias.clone(),
        ),
    ] {
        knobs.push(Knob::new(id, path, m.root.read(path), KnobPolicy::Observe).wants(desired));
    }
    knobs
}

/// Every knob, read now.
#[must_use]
pub fn catalog(m: &Machine) -> Vec<Knob> {
    let policy = policy_for(m);
    let paths = &m.profile.paths;
    let fn_q = m.ledger.get(Key::Override);
    let mut knobs = Vec::new();

    let mut profile = Knob::new(
        "platform_profile",
        &paths.platform_profile,
        m.root.read(&paths.platform_profile),
        KnobPolicy::Follow,
    );
    profile.choices = m
        .root
        .read(&paths.platform_profile_choices)
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect();
    profile.desired = fn_q.clone().or_else(|| m.platform_choice(policy));
    knobs.push(profile);

    let follow = fn_q
        .as_deref()
        .and_then(|mode| m.profile.firmware_follow.get(mode));
    knobs.extend(cpu(m, policy, follow));
    knobs.push(Knob::new(
        "sched_ext",
        SCHED_EXT,
        m.root.read(SCHED_EXT),
        KnobPolicy::Observe,
    ));
    let ppd = follow
        .and_then(|f| f.power_profile.clone())
        .or_else(|| policy.power_profile.clone());
    knobs.push(
        Knob::new(
            "ppd",
            "power-profiles-daemon",
            m.ppd_current.clone(),
            KnobPolicy::Observe,
        )
        .wants(ppd),
    );

    let mut sysctl = m.profile.sysctl.clone();
    sysctl.extend(policy.sysctl.clone());
    for (key, desired) in sysctl {
        let path = format!("/proc/sys/{}", key.replace('.', "/"));
        knobs.push(
            Knob::new(
                format!("sysctl.{key}"),
                &path,
                m.root.read(&path),
                KnobPolicy::Observe,
            )
            .wants(Some(desired)),
        );
    }

    knobs.extend(firmware_attributes(m, policy));
    if let Some(fan) = &paths.fan_mode {
        knobs.push(
            Knob::new(
                "firmware.fan_mode",
                fan,
                m.root.read(fan),
                KnobPolicy::Observe,
            )
            .wants(
                policy
                    .firmware
                    .fan_mode
                    .as_ref()
                    .filter(|v| !v.is_null())
                    .map(text),
            ),
        );
    }
    let holds = m.ledger.holds();
    for knob in &mut knobs {
        // a hold on the knob's path, or on one CPU's file of a per-CPU knob
        knob.held = holds
            .iter()
            .find(|(path, _)| crate::policy::held_by(&knob.path, path))
            .map(|(_, value)| value.clone());
    }
    knobs
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
    let knobs = catalog(m);
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
            ) {
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
#[must_use]
pub fn find(m: &Machine, id: &str) -> Option<Knob> {
    catalog(m).into_iter().find(|k| k.id == id || k.path == id)
}
