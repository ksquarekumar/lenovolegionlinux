//! Power policy: what to write for a power source (apply, reassert) or a firmware mode the user chose (follow), as a
//! plan of steps that is either printed (dry run) or executed. Only changed values become writes.
//!
//! Fn+Q changes `platform_profile` in firmware and emits the same uevent as our own writes. The ledger (`system.db`)
//! records the value we wrote; any other value is the user's choice, kept as an override until the power source
//! changes, the machine reboots (ledger rows are per boot), or `apply` runs.
//!
//! Power limits are never written through RAPL: firmware mode changes rewrite the MMIO limits, so a second write
//! path only fights the firmware (docs/design/15-layout.md, one path per capability).

use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};

use crate::fan;
use crate::ledger::{FanKey, Key, Ledger};
use crate::profile::{self, ModePolicy, Profile};
use crate::sysroot::SysRoot;

const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
const FIRMWARE_ATTRIBUTES: &str = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes";
const DMI: &str = "/sys/class/dmi/id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Write {
        path: String,
        value: String,
    },
    Run(Vec<String>),
    /// Run and ignore failure (for example `systemctl reset-failed` of a unit that may not exist).
    Try(Vec<String>),
    /// Create or replace a whole file (configuration we own, such as a udev rule).
    Create {
        path: String,
        content: String,
    },
    /// Remove a file we own, if it exists.
    Remove {
        path: String,
    },
    /// A ledger value; `None` removes it.
    Record {
        key: Key,
        value: Option<String>,
    },
    /// The user's hold on a path (D1); `None` releases it.
    Hold {
        path: String,
        value: Option<String>,
    },
    /// A firmware method through `acpi_call`.
    Acpi(String),
    /// Write the fan table and read it back.
    FanCurve([u8; fan::POINTS]),
    /// A user's fan choice; `None` removes it.
    Fan {
        key: FanKey,
        value: Option<String>,
    },
    /// One EC RAM byte through port I/O (the EC fan lock only).
    Ec {
        addr: u16,
        value: u8,
    },
    Info(String),
    Warn(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Ac,
    Battery,
}

impl Source {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Source::Ac => "ac",
            Source::Battery => "battery",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Apply even if DMI does not match the profile.
    pub force: bool,
    /// Restore firmware mode and PPD only, as a periodic check would.
    pub reassert: bool,
    /// Keep an Fn+Q choice (after resume).
    pub keep_override: bool,
}

/// What the planner needs besides the sysroot: the ledger, power-profiles-daemon's current profile and which commands
/// exist.
pub struct Machine<'a> {
    pub root: &'a SysRoot,
    pub profile: &'a Profile,
    pub ledger: &'a Ledger,
    pub ppd_current: Option<String>,
    pub has_command: &'a dyn Fn(&str) -> bool,
}

/// A value in single quotes, as the policy messages show it.
fn quoted(s: &str) -> String {
    format!("'{s}'")
}

impl Machine<'_> {
    fn write(&self, steps: &mut Vec<Step>, path: impl AsRef<Path>, value: &str) {
        let path = path.as_ref();
        if !self.root.exists(path) {
            steps.push(Step::Warn(format!("missing path: {}", path.display())));
        } else if self.root.read(path).as_deref() != Some(value) {
            steps.push(Step::Write {
                path: path.display().to_string(),
                value: value.into(),
            });
        }
    }

    pub fn run(&self, steps: &mut Vec<Step>, argv: &[&str]) {
        if (self.has_command)(argv[0]) {
            steps.push(Step::Run(
                argv.iter().map(std::string::ToString::to_string).collect(),
            ));
        } else {
            steps.push(Step::Warn(format!("missing command: {}", argv[0])));
        }
    }

    /// Keep power-profiles-daemon (GNOME Power Mode) in step instead of fighting it.
    fn power_profile(&self, steps: &mut Vec<Step>, wanted: Option<&str>) {
        if let Some(wanted) = wanted
            && (self.has_command)("powerprofilesctl")
            && self.ppd_current.as_deref() != Some(wanted)
        {
            self.run(steps, &["powerprofilesctl", "set", wanted]);
        }
    }

    fn cpufreq(&self, steps: &mut Vec<Step>, name: &str, value: Option<&str>) {
        if let Some(value) = value {
            for policy in self.root.children(CPUFREQ, "policy") {
                self.write(steps, policy.join(name), value);
            }
        }
    }

    fn follow_policy(&self, steps: &mut Vec<Step>, firmware_mode: &str) {
        let Some(follow) = self.profile.firmware_follow.get(firmware_mode) else {
            steps.push(Step::Warn(format!(
                "no firmware_follow entry for {}; leaving CPU policy alone",
                quoted(firmware_mode)
            )));
            return;
        };
        self.power_profile(steps, follow.power_profile.as_deref());
        self.cpufreq(steps, "scaling_governor", follow.governor.as_deref());
    }

    /// The first firmware mode in `policy`'s preference that this machine offers.
    #[must_use]
    pub fn platform_choice(&self, policy: &ModePolicy) -> Option<String> {
        let choices = self
            .root
            .read(&self.profile.paths.platform_profile_choices)
            .unwrap_or_default();
        let available: Vec<&str> = choices.split_whitespace().collect();
        policy
            .firmware
            .platform_profile_preference
            .iter()
            .find(|c| available.contains(&c.as_str()))
            .cloned()
    }

    fn firmware_policy(&self, steps: &mut Vec<Step>, policy: &ModePolicy) {
        match self.platform_choice(policy) {
            Some(chosen) => {
                self.write(steps, &self.profile.paths.platform_profile, &chosen);
                steps.push(Step::Record {
                    key: Key::Written,
                    value: Some(chosen),
                });
            }
            None => steps.push(Step::Warn(
                "no matching platform_profile choice found".into(),
            )),
        }
        if let Some(fan_mode) = policy.firmware.fan_mode.as_ref().filter(|v| !v.is_null()) {
            match &self.profile.paths.fan_mode {
                Some(path) => self.write(steps, path, &profile::text(fan_mode)),
                None => steps.push(Step::Warn("no VPC2004 fan_mode path found".into())),
            }
        }
    }

    fn firmware_attributes(&self, steps: &mut Vec<Step>, policy: &ModePolicy) {
        for (name, value) in &policy.firmware_attributes {
            let base = Path::new(FIRMWARE_ATTRIBUTES).join(name);
            if self.root.read(base.join("current_value")).is_none() {
                steps.push(Step::Warn(format!("firmware attribute {name} not found")));
                continue;
            }
            let bound = |f: &str| -> i64 {
                self.root
                    .read(base.join(f))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            };
            let (low, high) = (bound("min_value"), bound("max_value"));
            let value = profile::text(value);
            // some attributes publish min=max=0, meaning the firmware does not report a range
            if high != 0 && !value.parse().is_ok_and(|v: i64| (low..=high).contains(&v)) {
                steps.push(Step::Warn(format!(
                    "firmware attribute {name}={value} is outside {low}-{high}; skipped"
                )));
                continue;
            }
            self.write(steps, base.join("current_value"), &value);
        }
    }

    fn sysctl(&self, steps: &mut Vec<Step>, policy: &ModePolicy) {
        let mut all = self.profile.sysctl.clone();
        all.extend(policy.sysctl.clone());
        for (key, value) in all {
            self.write(
                steps,
                Path::new("/proc/sys").join(key.replace('.', "/")),
                &value,
            );
        }
    }

    /// The CPU knobs of `policy`. While legionctl follows an Fn+Q choice (`following`), the governor and EPP stay
    /// as `firmware_follow` and power-profiles-daemon set them.
    fn cpu_policy(&self, steps: &mut Vec<Step>, policy: &ModePolicy, following: bool) {
        let cpu = &policy.cpu;
        for (path, value) in [
            (
                "/sys/module/processor/parameters/ignore_ppc",
                &cpu.ignore_ppc,
            ),
            (
                "/sys/devices/system/cpu/intel_pstate/no_turbo",
                &cpu.no_turbo,
            ),
            (
                "/sys/devices/system/cpu/intel_pstate/max_perf_pct",
                &cpu.max_perf_pct,
            ),
        ] {
            if let Some(value) = value {
                self.write(steps, path, value);
            }
        }
        if !following {
            self.cpufreq(steps, "scaling_governor", cpu.governor.as_deref());
            self.cpufreq(
                steps,
                "energy_performance_preference",
                cpu.energy_performance_preference.as_deref(),
            );
        }
        if let Some(bias) = &cpu.energy_perf_bias {
            for dir in self.root.children("/sys/devices/system/cpu", "cpu") {
                if self.root.exists(dir.join("power/energy_perf_bias")) {
                    self.write(steps, dir.join("power/energy_perf_bias"), bias);
                }
            }
            if (self.has_command)("cpupower") {
                self.run(steps, &["cpupower", "set", "--perf-bias", bias]);
            }
        }
        // re-enable every C-state in case something disabled them; deep idle frees turbo headroom
        if cpu.enable_all_idle_states {
            self.run(steps, &["cpupower", "idle-set", "-E"]);
        }
    }

    fn nvidia(&self, steps: &mut Vec<Step>, policy: &ModePolicy) {
        let nv = &policy.nvidia;
        if nv.start_persistenced {
            self.run(
                steps,
                &["systemctl", "start", "nvidia-persistenced.service"],
            );
        }
        if nv.start_powerd {
            self.run(steps, &["systemctl", "start", "nvidia-powerd.service"]);
        }
        if nv.set_persistence_mode {
            self.run(steps, &["nvidia-smi", "-i", "0", "-pm", "1"]);
        }
    }

    #[must_use]
    pub fn ac_online(&self) -> bool {
        self.root.read(&self.profile.paths.ac_adapter).as_deref() == Some("1")
    }

    /// # Errors
    /// The DMI identity does not match the profile and `force` is not set.
    pub fn require_supported_machine(&self, force: bool) -> Result<()> {
        let mismatches: Vec<String> = self
            .profile
            .dmi_match
            .iter()
            .filter_map(|(key, expected)| {
                let live = self.root.read(Path::new(DMI).join(key));
                (live.as_deref() != Some(expected.as_str())).then(|| {
                    let live = live.map_or("None".into(), |l| quoted(&l));
                    format!("  {key}: expected {}, got {live}", quoted(expected))
                })
            })
            .collect();
        if !mismatches.is_empty() && !force {
            bail!(
                "DMI profile mismatch; refusing to apply policy for this machine:\n{}\nUse --force only if you intentionally want to override this guard.",
                mismatches.join("\n")
            );
        }
        Ok(())
    }

    /// The plan with each write to a path the user holds replaced by a note (D1: a hold pauses the policy there).
    fn without_held(&self, steps: Vec<Step>) -> Vec<Step> {
        let holds = self.ledger.holds();
        steps
            .into_iter()
            .map(|step| match &step {
                Step::Write { path, .. } => match holds.iter().find(|(p, _)| held_by(p, path)) {
                    Some((_, value)) => {
                        Step::Info(format!("{path} is held at {} by the user", quoted(value)))
                    }
                    None => step,
                },
                _ => step,
            })
            .collect()
    }

    /// Plan the policy for `source` (`None`: the current power source); the user's holds stay as they are.
    ///
    /// # Errors
    /// The machine is not supported (see [`Machine::require_supported_machine`]).
    pub fn apply(&self, source: Option<Source>, opts: Options) -> Result<(Source, Vec<Step>)> {
        let ac = self.ac_online();
        let source = source.unwrap_or(if ac { Source::Ac } else { Source::Battery });
        self.require_supported_machine(opts.force)?;
        let mut steps = Vec::new();
        match (source, ac) {
            (Source::Battery, true) => steps.push(Step::Warn(
                "ADP0 is online but battery policy was requested".into(),
            )),
            (Source::Ac, false) => steps.push(Step::Warn(
                "ADP0 is offline but AC policy was requested".into(),
            )),
            _ => {}
        }
        let policy = match source {
            Source::Ac => &self.profile.ac,
            Source::Battery => &self.profile.battery,
        };
        let mut override_mode = self.ledger.get(Key::Override);
        if !opts.reassert && !opts.keep_override {
            // a power-source change, boot or manual apply starts from the profile defaults again
            if let Some(mode) = override_mode.take() {
                steps.push(Step::Info(format!(
                    "clearing firmware override {}",
                    quoted(&mode)
                )));
                steps.push(Step::Record {
                    key: Key::Override,
                    value: None,
                });
            }
        }
        if let Some(mode) = &override_mode {
            self.follow_policy(&mut steps, mode);
        } else {
            self.firmware_policy(&mut steps, policy);
            // intel_pstate refuses EPP writes while the governor is performance, which makes PPD fail; governor first
            self.cpufreq(
                &mut steps,
                "scaling_governor",
                policy.cpu.governor.as_deref(),
            );
            self.power_profile(&mut steps, policy.power_profile.as_deref());
            if opts.reassert {
                // power-profiles-daemon rewrites EPP on profile changes; reassert wins it back (the governor was
                // written before power-profiles-daemon)
                self.cpufreq(
                    &mut steps,
                    "energy_performance_preference",
                    policy.cpu.energy_performance_preference.as_deref(),
                );
            }
        }
        self.firmware_attributes(&mut steps, policy);
        if !opts.reassert {
            self.sysctl(&mut steps, policy);
            self.cpu_policy(&mut steps, policy, override_mode.is_some());
            if source == Source::Ac {
                self.nvidia(&mut steps, policy);
            }
        }
        Ok((source, self.without_held(steps)))
    }

    /// Plan the reaction to a `platform_profile` change: our own write is ignored, anything else is an Fn+Q choice.
    ///
    /// # Errors
    /// `platform_profile` is not readable.
    pub fn follow(&self) -> Result<Vec<Step>> {
        let Some(current) = self.root.read(&self.profile.paths.platform_profile) else {
            bail!("platform_profile is not readable");
        };
        let active = self
            .ledger
            .get(Key::Override)
            .or_else(|| self.ledger.get(Key::Written));
        if active.as_deref() == Some(current.as_str()) {
            return Ok(vec![Step::Info(format!(
                "platform_profile {} is already the active policy; nothing to follow",
                quoted(&current)
            ))]);
        }
        let mut steps = vec![
            Step::Info(format!(
                "following firmware mode {} (Fn+Q)",
                quoted(&current)
            )),
            Step::Record {
                key: Key::Override,
                value: Some(current.clone()),
            },
        ];
        self.follow_policy(&mut steps, &current);
        Ok(steps)
    }
}

/// A dry run's lines with the per-CPU ones grouped: consecutive writes of the same value to `policy0` ...
/// `policy31` are one line, `.../policy*/scaling_governor: powersave (32 CPUs)`; the order stays.
#[must_use]
pub fn grouped(steps: &[Step]) -> Vec<String> {
    let mut groups: Vec<(String, usize)> = Vec::new();
    for line in steps.iter().filter_map(describe) {
        let key = per_cpu(&line);
        match groups.last_mut() {
            Some((last, n)) if *last == key => *n += 1,
            _ => groups.push((key, 1)),
        }
    }
    groups
        .into_iter()
        .map(|(line, n)| {
            if n > 1 {
                format!("{line} ({n} CPUs)")
            } else {
                line
            }
        })
        .collect()
}

/// `policy12/` and `cpu12/` as `policy*/` and `cpu*/`.
fn per_cpu(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = ["/policy", "/cpu"]
        .iter()
        .filter_map(|p| rest.find(p).map(|i| (i, p.len())))
        .min()
    {
        let (i, len) = at;
        let digits = rest[i + len..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        out.push_str(&rest[..i + len]);
        if digits > 0 && rest[i + len + digits..].starts_with('/') {
            out.push('*');
        } else {
            out.push_str(&rest[i + len..i + len + digits]);
        }
        rest = &rest[i + len + digits..];
    }
    out.push_str(rest);
    out
}

/// One line per step as a dry run shows it; `None` for steps a dry run does not print.
#[must_use]
pub fn describe(step: &Step) -> Option<String> {
    match step {
        Step::Write { path, value } => Some(format!("would write {path}: {value}")),
        Step::Run(argv) | Step::Try(argv) => Some(format!("would run: {}", argv.join(" "))),
        Step::Create { path, content } => Some(format!("would write {path}:\n{content}")),
        Step::Remove { path } => Some(format!("would remove {path}")),
        Step::Info(text) => Some(text.clone()),
        Step::Warn(text) => Some(format!("warning: {text}")),
        Step::Record { .. } | Step::Fan { .. } => None,
        Step::Hold {
            path,
            value: Some(v),
        } => Some(format!("would hold {path} at {}", quoted(v))),
        Step::Hold { path, value: None } => Some(format!("would release {path}")),
        Step::Acpi(call) => Some(format!("would call {call}")),
        Step::FanCurve(levels) => Some(format!(
            "would set the fan curve to {}: call {}, then read it back",
            fan::text(levels),
            fan::set_table_call(levels)
        )),
        Step::Ec { addr, value } => Some(format!(
            "would write EC RAM 0x{addr:04X}: {value:#04x}, through port I/O: {}",
            fan::ec_port_writes(*addr, *value)
                .iter()
                .map(|(port, byte)| format!("{port:#x}={byte:#04x}"))
                .collect::<Vec<_>>()
                .join(" ")
        )),
    }
}

/// A write that `undo` can turn back: the file, what it wrote, and what it replaced.
#[must_use]
pub fn parse_write(line: &str) -> Option<(String, String, String)> {
    let rest = line.strip_prefix("wrote ")?;
    let (path, rest) = rest.split_once(": ")?;
    let (value, before) = rest.strip_suffix(')')?.rsplit_once(" (was ")?;
    Some((path.into(), value.into(), before.into()))
}

/// Whether `path` is one of the files that a hold on `pattern` keeps (a `*` stands for one CPU policy folder).
#[must_use]
pub fn held_by(pattern: &str, path: &str) -> bool {
    match pattern.split_once('*') {
        Some((head, tail)) => {
            path.len() >= head.len() + tail.len()
                && path.starts_with(head)
                && path.ends_with(tail)
                && !path[head.len()..path.len() - tail.len()].contains('/')
        }
        None => pattern == path,
    }
}

/// Carry out a plan. Returns the messages to show (writes done, warnings), in order.
pub fn execute(root: &SysRoot, ledger: &Ledger, steps: &[Step]) -> Vec<String> {
    let mut out = Vec::new();
    for step in steps {
        match step {
            Step::Write { path, value } => {
                let before = root.read(path).unwrap_or_default();
                if let Err(e) = std::fs::write(root.path(path), format!("{value}\n")) {
                    out.push(format!("warning: path is not writable: {path} ({e})"));
                    continue;
                }
                // the D-Bus contract (note 13): `undo` reads what a write replaced from this line
                out.push(format!("wrote {path}: {value} (was {before})"));
                let after = root.read(path).unwrap_or_default();
                if after != *value {
                    out.push(format!(
                        "warning: {path} read back as {} after writing {}",
                        quoted(&after),
                        quoted(value)
                    ));
                }
            }
            Step::Run(argv) => match Command::new(&argv[0]).args(&argv[1..]).output() {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let detail = if o.stderr.is_empty() {
                        o.stdout
                    } else {
                        o.stderr
                    };
                    out.push(format!(
                        "warning: command failed: {}: {}",
                        argv.join(" "),
                        String::from_utf8_lossy(&detail).trim()
                    ));
                }
                Err(e) => out.push(format!("warning: command failed: {}: {e}", argv.join(" "))),
            },
            Step::Create { path, content } => match std::fs::write(root.path(path), content) {
                Ok(()) => out.push(format!("wrote {path}")),
                Err(e) => out.push(format!("warning: could not write {path}: {e}")),
            },
            Step::Remove { path } => match std::fs::remove_file(root.path(path)) {
                Ok(()) => out.push(format!("removed {path}")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => out.push(format!("warning: could not remove {path}: {e}")),
            },
            Step::Try(argv) => {
                let _ = Command::new(&argv[0]).args(&argv[1..]).output();
            }
            Step::Record { key, value } => {
                if let Err(e) = ledger.set(*key, value.as_deref()) {
                    out.push(format!("warning: {e:#}"));
                }
            }
            Step::Hold { path, value } => match ledger.hold(path, value.as_deref()) {
                Ok(()) => out.push(match value {
                    Some(v) => format!("held {path} at {}", quoted(v)),
                    None => format!("released {path}"),
                }),
                Err(e) => out.push(format!("warning: {e:#}")),
            },
            Step::Acpi(call) => out.push(match crate::firmware::acpi_call(root, call) {
                Ok(reply) => format!("called {call}: {reply}"),
                Err(e) => format!("warning: firmware call failed: {e:#}"),
            }),
            Step::FanCurve(levels) => out.extend(fan::write_curve(root, levels)),
            Step::Fan { key, value } => {
                if let Err(e) = ledger.set_fan(*key, value.as_deref()) {
                    out.push(format!("warning: {e:#}"));
                }
            }
            Step::Ec { addr, value } => out.push(match fan::ec_write(root, *addr, *value) {
                Ok(()) => format!("wrote EC RAM 0x{addr:04X}: {value:#04x}"),
                Err(e) => format!("warning: EC RAM 0x{addr:04X} was not written: {e:#}"),
            }),
            Step::Info(text) => out.push(text.clone()),
            Step::Warn(text) => out.push(format!("warning: {text}")),
        }
    }
    out
}
