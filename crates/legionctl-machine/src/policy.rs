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

use anyhow::{Context, Result, bail};

use serde_json::Value;

use crate::configset::{self, Conditions, Layer, Merged};
use crate::fan;
use crate::ledger::{FanKey, Key, Ledger};
use crate::profile::{self, Profile};
use crate::sysroot::SysRoot;

const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
const FIRMWARE_ATTRIBUTES: &str = "/sys/class/firmware-attributes/lenovo-wmi-other-0/attributes";
const DMI: &str = "/sys/class/dmi/id";

/// The full path of an executable on `PATH`.
#[must_use]
pub fn which(program: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| {
                m.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Write {
        path: String,
        value: String,
    },
    Run(Vec<String>),
    /// Run as a transient unit outside the daemon's sandbox (docs/design/13-daemon.md, Sandbox): pacman, ukify, sbsign
    /// and the `efibootmgr` changes.
    Unit(Vec<String>),
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
    /// A file of the `legionctl-wmi` module's hwmon device (found by its name when the step runs), read back.
    Hwmon {
        file: &'static str,
        value: u8,
    },
    /// Write the fan table through the module and read it back.
    FanCurve([u8; fan::POINTS]),
    /// A user's fan choice; `None` removes it.
    Fan {
        key: FanKey,
        value: Option<String>,
    },
    /// A systemd-boot variable in efivarfs (`LoaderEntryOneShot`): `bootctl` sets these only after a start through
    /// systemd-boot, and the first `kernel try` comes from rEFInd.
    LoaderVar {
        name: String,
        value: String,
    },
    /// Load an `OpenRGB` profile (`rgb.profile`) and record it.
    Rgb(String),
    /// Set the power-profiles-daemon profile over D-Bus.
    Ppd(String),
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

/// What the planner needs besides the sysroot: the ledger, power-profiles-daemon's current profile ([`crate::ppd::active`]:
/// none when it is not installed), which commands exist, and the configset layers.
pub struct Machine<'a> {
    pub root: &'a SysRoot,
    pub profile: &'a Profile,
    pub ledger: &'a Ledger,
    pub ppd_current: Option<String>,
    pub has_command: &'a dyn Fn(&str) -> bool,
    /// Every configset layer ([`configset::load`]); the used ones make the desired state.
    pub layers: &'a [Layer],
}

/// A value in single quotes, as the policy messages show it.
fn quoted(s: &str) -> String {
    format!("'{s}'")
}

impl Machine<'_> {
    /// A write of `value` to `path` when it has another value now.
    ///
    /// # Errors
    /// `path` cannot be read.
    pub(crate) fn write(
        &self,
        steps: &mut Vec<Step>,
        path: impl AsRef<Path>,
        value: &str,
    ) -> Result<()> {
        let path = path.as_ref();
        if !self.root.exists(path)? {
            steps.push(Step::Warn(format!("missing path: {}", path.display())));
        } else if self.root.read(path)?.as_deref() != Some(value) {
            steps.push(Step::Write {
                path: path.display().to_string(),
                value: value.into(),
            });
        }
        Ok(())
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

    /// Keep power-profiles-daemon (GNOME Power Mode) in step instead of fighting it; nothing when it is not installed.
    fn power_profile(&self, steps: &mut Vec<Step>, wanted: Option<&str>) {
        if let (Some(wanted), Some(current)) = (wanted, &self.ppd_current)
            && current != wanted
        {
            steps.push(Step::Ppd(wanted.into()));
        }
    }

    fn cpufreq(&self, steps: &mut Vec<Step>, name: &str, value: Option<&str>) -> Result<()> {
        if let Some(value) = value {
            for policy in self.root.children(CPUFREQ, "policy")? {
                self.write(steps, policy.join(name), value)?;
            }
        }
        Ok(())
    }

    /// The conditions the layers are tested against, for `source` while the policy follows the Fn+Q choice `fnq`.
    ///
    /// # Errors
    /// The workload or the running kernel cannot be read.
    pub fn conditions(&self, source: Source, fnq: Option<&str>) -> Result<Conditions> {
        Ok(Conditions {
            power: source,
            fnq: fnq.map(String::from),
            workload: configset::workload(self.root)?,
            kernel: crate::kernel::running_package(self.root)?,
        })
    }

    /// The desired state for `source` while the policy follows the Fn+Q choice `fnq` (configsets, note 01).
    ///
    /// # Errors
    /// The conditions cannot be read.
    pub fn desired(&self, source: Source, fnq: Option<&str>) -> Result<Merged> {
        Ok(configset::merge(
            self.layers,
            &self.conditions(source, fnq)?,
        ))
    }

    /// Whether a used layer has a policy for the Fn+Q mode `mode`.
    #[must_use]
    pub fn follows(&self, mode: &str) -> bool {
        self.layers
            .iter()
            .any(|l| l.off.is_none() && l.when.get("fnq").map(String::as_str) == Some(mode))
    }

    fn follow_policy(
        &self,
        steps: &mut Vec<Step>,
        firmware_mode: &str,
        wanted: &Merged,
    ) -> Result<()> {
        if !self.follows(firmware_mode) {
            steps.push(Step::Warn(format!(
                "no configset layer for the Fn+Q mode {}; leaving CPU policy alone",
                quoted(firmware_mode)
            )));
            return Ok(());
        }
        self.power_profile(steps, wanted.text("ppd").as_deref());
        self.cpufreq(
            steps,
            "scaling_governor",
            wanted.text("cpu.governor").as_deref(),
        )
    }

    /// The first firmware mode of the wanted `platform_profile` (a preference list) that this machine offers.
    ///
    /// # Errors
    /// The firmware mode choices cannot be read.
    pub fn platform_choice(&self, wanted: &Merged) -> Result<Option<String>> {
        let choices = self
            .root
            .read(&self.profile.paths.platform_profile_choices)?
            .unwrap_or_default();
        let available: Vec<&str> = choices.split_whitespace().collect();
        let preference: Vec<String> = match wanted.values.get("platform_profile") {
            None => return Ok(None),
            Some(Value::Array(list)) => list.iter().map(profile::text).collect(),
            Some(one) => vec![profile::text(one)],
        };
        Ok(preference
            .into_iter()
            .find(|c| available.contains(&c.as_str())))
    }

    fn firmware_policy(&self, steps: &mut Vec<Step>, wanted: &Merged) -> Result<()> {
        match self.platform_choice(wanted)? {
            Some(chosen) => {
                self.write(steps, &self.profile.paths.platform_profile, &chosen)?;
                steps.push(Step::Record {
                    key: Key::Written,
                    value: Some(chosen),
                });
            }
            None => steps.push(Step::Warn(
                "no matching platform_profile choice found".into(),
            )),
        }
        if let Some(fan_mode) = wanted.text("firmware.fan_mode") {
            match &self.profile.paths.fan_mode {
                Some(path) => self.write(steps, path, &fan_mode)?,
                None => steps.push(Step::Warn("no VPC2004 fan_mode path found".into())),
            }
        }
        Ok(())
    }

    fn firmware_attributes(&self, steps: &mut Vec<Step>, wanted: &Merged) -> Result<()> {
        for (name, value) in wanted.under("firmware.") {
            if name == "fan_mode" {
                continue;
            }
            let base = Path::new(FIRMWARE_ATTRIBUTES).join(&name);
            if self.root.read(base.join("current_value"))?.is_none() {
                steps.push(Step::Warn(format!("firmware attribute {name} not found")));
                continue;
            }
            let bound = |f: &str| -> Result<Option<String>> { self.root.read(base.join(f)) };
            let range = match (bound("min_value")?, bound("max_value")?) {
                (None, None) => None,
                (Some(low), Some(high)) => match (low.parse::<i64>(), high.parse::<i64>()) {
                    // some attributes publish min=max=0, meaning the firmware does not report a range
                    (Ok(0), Ok(0)) => None,
                    (Ok(low), Ok(high)) => Some((low, high)),
                    _ => {
                        steps.push(Step::Warn(format!(
                            "firmware attribute {name} has a range that is not a number ({low}-{high}); skipped"
                        )));
                        continue;
                    }
                },
                (low, high) => {
                    steps.push(Step::Warn(format!(
                        "firmware attribute {name} has only part of its range (min {}, max {}); skipped",
                        low.as_deref().unwrap_or("missing"),
                        high.as_deref().unwrap_or("missing")
                    )));
                    continue;
                }
            };
            if let Some((low, high)) = range
                && !value.parse().is_ok_and(|v: i64| (low..=high).contains(&v))
            {
                steps.push(Step::Warn(format!(
                    "firmware attribute {name}={value} is outside {low}-{high}; skipped"
                )));
                continue;
            }
            self.write(steps, base.join("current_value"), &value)?;
        }
        Ok(())
    }

    fn sysctl(&self, steps: &mut Vec<Step>, wanted: &Merged) -> Result<()> {
        for (key, value) in wanted.under("sysctl.") {
            self.write(
                steps,
                Path::new("/proc/sys").join(key.replace('.', "/")),
                &value,
            )?;
        }
        Ok(())
    }

    /// The CPU knobs. While legionctl follows an Fn+Q choice (`following`), the governor and EPP stay as the Fn+Q
    /// layer and power-profiles-daemon set them.
    fn cpu_policy(&self, steps: &mut Vec<Step>, wanted: &Merged, following: bool) -> Result<()> {
        for (id, path) in [
            (
                "cpu.ignore_ppc",
                "/sys/module/processor/parameters/ignore_ppc",
            ),
            (
                "cpu.no_turbo",
                "/sys/devices/system/cpu/intel_pstate/no_turbo",
            ),
            (
                "cpu.max_perf_pct",
                "/sys/devices/system/cpu/intel_pstate/max_perf_pct",
            ),
        ] {
            if let Some(value) = wanted.text(id) {
                self.write(steps, path, &value)?;
            }
        }
        if !following {
            self.cpufreq(
                steps,
                "scaling_governor",
                wanted.text("cpu.governor").as_deref(),
            )?;
            self.cpufreq(
                steps,
                "energy_performance_preference",
                wanted.text("cpu.epp").as_deref(),
            )?;
        }
        if let Some(bias) = wanted.text("cpu.energy_perf_bias") {
            for dir in self.root.children("/sys/devices/system/cpu", "cpu")? {
                if self.root.exists(dir.join("power/energy_perf_bias"))? {
                    self.write(steps, dir.join("power/energy_perf_bias"), &bias)?;
                }
            }
            if (self.has_command)("cpupower") {
                self.run(steps, &["cpupower", "set", "--perf-bias", &bias]);
            }
        }
        if wanted.text("cpu.idle_states").as_deref() == Some("enabled") {
            self.run(steps, &["cpupower", "idle-set", "-E"]);
        }
        Ok(())
    }

    fn nvidia(&self, steps: &mut Vec<Step>, wanted: &Merged) {
        if wanted.text("nvidia.services").as_deref() == Some("start") {
            // no wait for the start job: a stop of the daemon queued before it (an upgrade) waits for the daemon,
            // and the daemon would wait for the job until SIGKILL (todo-125)
            self.run(
                steps,
                &[
                    "systemctl",
                    "start",
                    "--no-block",
                    "nvidia-persistenced.service",
                    "nvidia-powerd.service",
                ],
            );
            self.run(steps, &["nvidia-smi", "-i", "0", "-pm", "1"]);
        }
    }

    /// The `OpenRGB` profile, when it is not the one loaded last in this boot.
    fn rgb(&self, steps: &mut Vec<Step>, wanted: &Merged) -> Result<()> {
        if let Some(profile) = wanted.text("rgb.profile")
            && self.ledger.get(Key::RgbProfile)?.as_deref() != Some(profile.as_str())
        {
            steps.push(Step::Rgb(profile));
        }
        Ok(())
    }

    /// The AC adapter is online.
    ///
    /// # Errors
    /// The AC adapter file is missing or cannot be read.
    pub fn ac_online(&self) -> Result<bool> {
        let path = &self.profile.paths.ac_adapter;
        Ok(self
            .root
            .read(path)?
            .with_context(|| format!("{path} is missing: the power source is unknown"))?
            == "1")
    }

    /// The power source now.
    ///
    /// # Errors
    /// The AC adapter file is missing or cannot be read.
    pub fn source(&self) -> Result<Source> {
        Ok(if self.ac_online()? {
            Source::Ac
        } else {
            Source::Battery
        })
    }

    /// # Errors
    /// The DMI identity cannot be read, or it does not match the profile and `force` is not set.
    pub fn require_supported_machine(&self, force: bool) -> Result<()> {
        let mut mismatches: Vec<String> = Vec::new();
        for (key, expected) in &self.profile.dmi_match {
            let live = self.root.read(Path::new(DMI).join(key))?;
            if live.as_deref() != Some(expected.as_str()) {
                let live = live.map_or("None".into(), |l| quoted(&l));
                mismatches.push(format!(
                    "  {key}: expected {}, got {live}",
                    quoted(expected)
                ));
            }
        }
        if !mismatches.is_empty() && !force {
            bail!(
                "DMI profile mismatch; refusing to apply policy for this machine:\n{}\nUse --force only if you intentionally want to override this guard.",
                mismatches.join("\n")
            );
        }
        Ok(())
    }

    /// The plan with each write to a path the user holds replaced by a note (D1: a hold pauses the policy there).
    fn without_held(&self, steps: Vec<Step>) -> Result<Vec<Step>> {
        let holds = self.ledger.holds()?;
        Ok(steps
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
            .collect())
    }

    /// Plan the policy for `source` (`None`: the current power source); the user's holds stay as they are.
    ///
    /// # Errors
    /// The machine is not supported (see [`Machine::require_supported_machine`]), or a machine file or the ledger
    /// cannot be read.
    pub fn apply(&self, source: Option<Source>, opts: Options) -> Result<(Source, Vec<Step>)> {
        let ac = self.ac_online()?;
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
        let mut override_mode = self.ledger.get(Key::Override)?;
        if !opts.reassert && !opts.keep_override {
            // a power-source change, boot or manual apply starts from the layers without a choice again
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
        let wanted = self.desired(source, override_mode.as_deref())?;
        if let Some(mode) = &override_mode {
            self.follow_policy(&mut steps, mode, &wanted)?;
        } else {
            self.firmware_policy(&mut steps, &wanted)?;
            // intel_pstate refuses EPP writes while the governor is performance, which makes PPD fail; governor first
            self.cpufreq(
                &mut steps,
                "scaling_governor",
                wanted.text("cpu.governor").as_deref(),
            )?;
            self.power_profile(&mut steps, wanted.text("ppd").as_deref());
            if opts.reassert {
                // power-profiles-daemon rewrites EPP on profile changes; reassert wins it back (the governor was
                // written before power-profiles-daemon)
                self.cpufreq(
                    &mut steps,
                    "energy_performance_preference",
                    wanted.text("cpu.epp").as_deref(),
                )?;
            }
        }
        self.firmware_attributes(&mut steps, &wanted)?;
        if !opts.reassert {
            self.sysctl(&mut steps, &wanted)?;
            self.cpu_policy(&mut steps, &wanted, override_mode.is_some())?;
            self.nvidia(&mut steps, &wanted);
            self.rgb(&mut steps, &wanted)?;
        }
        Ok((source, self.without_held(steps)?))
    }

    /// Plan the reaction to a `platform_profile` change: our own write is ignored, anything else is an Fn+Q choice.
    ///
    /// # Errors
    /// `platform_profile` or the ledger is not readable.
    pub fn follow(&self) -> Result<Vec<Step>> {
        let Some(current) = self.root.read(&self.profile.paths.platform_profile)? else {
            bail!("platform_profile is missing");
        };
        let active = match self.ledger.get(Key::Override)? {
            Some(mode) => Some(mode),
            None => self.ledger.get(Key::Written)?,
        };
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
        let wanted = self.desired(self.source()?, Some(&current))?;
        self.follow_policy(&mut steps, &current, &wanted)?;
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
        Step::Run(argv) | Step::Unit(argv) | Step::Try(argv) => {
            Some(format!("would run: {}", argv.join(" ")))
        }
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
        Step::Hwmon { file, value } => Some(format!(
            "would write {value} to {file} of hwmon legionctl_wmi"
        )),
        Step::Rgb(profile) => Some(format!("would load the OpenRGB profile {profile}")),
        Step::Ppd(profile) => Some(format!(
            "would set the power-profiles-daemon profile {profile}"
        )),
        Step::LoaderVar { name, value } => {
            Some(format!("would set the EFI variable {name} to {value}"))
        }
        Step::FanCurve(levels) => Some(format!(
            "would set the fan curve to {}: write the changed points of pwm1_auto_point1_pwm .. pwm1_auto_point10_pwm \
             of hwmon legionctl_wmi (point 10 down, then point 1 up), then read it back",
            fan::text(levels)
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

/// Carry out a plan. Returns the messages to show (writes done, the plan's warnings), in order.
///
/// # Errors
/// A step fails: the steps after it do not run, and the error names the step, its reason and what was done before it.
pub fn execute(root: &SysRoot, ledger: &Ledger, steps: &[Step]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for step in steps {
        if let Err(e) = execute_one(root, ledger, step, &mut out) {
            let what = describe(step).map_or_else(
                || format!("{step:?}"),
                |d| d.strip_prefix("would ").unwrap_or(&d).to_string(),
            );
            let done = if out.is_empty() {
                "nothing".to_string()
            } else {
                out.join("; ")
            };
            bail!("{what} failed: {e:#}; done before it: {done}");
        }
    }
    Ok(out)
}

fn execute_one(root: &SysRoot, ledger: &Ledger, step: &Step, out: &mut Vec<String>) -> Result<()> {
    match step {
        Step::Write { path, value } => {
            // undo writes this value back: a file that is not there has none
            let before = root
                .read(path)?
                .with_context(|| format!("{path} is missing"))?;
            std::fs::write(root.path(path), format!("{value}\n"))
                .with_context(|| format!("writing {path}"))?;
            // the D-Bus contract (note 13): `undo` reads what a write replaced from this line
            out.push(format!("wrote {path}: {value} (was {before})"));
            let after = root.read(path)?.unwrap_or_default();
            if after != *value {
                out.push(format!(
                    "warning: {path} read back as {} after writing {}",
                    quoted(&after),
                    quoted(value)
                ));
            }
        }
        Step::Run(argv) => run(Command::new(&argv[0]).args(&argv[1..]), argv)?,
        Step::Unit(argv) => run(&mut unit(argv), argv)?,
        Step::Create { path, content } => {
            std::fs::write(root.path(path), content).with_context(|| format!("writing {path}"))?;
            out.push(format!("wrote {path}"));
        }
        Step::Remove { path } => match std::fs::remove_file(root.path(path)) {
            Ok(()) => out.push(format!("removed {path}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {path}")),
        },
        Step::Try(argv) => {
            // its exit status may be a failure by design (the step after it does the real work); a program that does
            // not start is a failure
            Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .with_context(|| format!("running {}", argv[0]))?;
        }
        Step::Record { key, value } => ledger.set(*key, value.as_deref())?,
        Step::Hold { path, value } => {
            ledger.hold(path, value.as_deref())?;
            out.push(match value {
                Some(v) => format!("held {path} at {}", quoted(v)),
                None => format!("released {path}"),
            });
        }
        Step::Hwmon { file, value } => out.push(fan::write_hwmon(root, file, *value)?),
        Step::Rgb(profile) => {
            let before = ledger.get(Key::RgbProfile)?.unwrap_or_default();
            crate::rgb::load(profile)?;
            ledger.set(Key::RgbProfile, Some(profile))?;
            out.push(format!(
                "loaded the OpenRGB profile {profile} (was {before})"
            ));
        }
        Step::Ppd(profile) => {
            crate::ppd::set(profile)?;
            out.push(format!("set the power-profiles-daemon profile {profile}"));
        }
        Step::FanCurve(levels) => out.push(fan::write_curve(root, levels)?),
        Step::Fan { key, value } => ledger.set_fan(*key, value.as_deref())?,
        Step::Ec { addr, value } => {
            fan::ec_write(root, *addr, *value)?;
            out.push(format!("wrote EC RAM 0x{addr:04X}: {value:#04x}"));
        }
        Step::LoaderVar { name, value } => {
            loader_var(root, name, value)?;
            out.push(format!("set the EFI variable {name} to {value}"));
        }
        Step::Info(text) => out.push(text.clone()),
        Step::Warn(text) => out.push(format!("warning: {text}")),
    }
    Ok(())
}

/// Run `c` (the program `argv`).
///
/// # Errors
/// It cannot start or it fails: its error output.
fn run(c: &mut Command, argv: &[String]) -> Result<()> {
    let o = c
        .output()
        .with_context(|| format!("starting {}", argv.join(" ")))?;
    if !o.status.success() {
        let detail = if o.stderr.is_empty() {
            o.stdout
        } else {
            o.stderr
        };
        bail!(
            "{} exited with {}: {}",
            argv.join(" "),
            o.status,
            String::from_utf8_lossy(&detail).trim()
        );
    }
    Ok(())
}

/// `argv` as a transient unit with no sandbox: `--pipe` and `--wait` give back its output and exit status.
fn unit(argv: &[String]) -> Command {
    static STEPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let mut c = Command::new("systemd-run");
    c.args(["--wait", "--pipe", "--collect", "--quiet"])
        .arg(format!(
            "--unit=legionctl-step-{}-{}",
            std::process::id(),
            STEPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
        .arg("--")
        .args(argv);
    c
}

/// Write a systemd-boot variable (vendor GUID of the Boot Loader Interface) to efivarfs: the attributes (non-volatile,
/// boot and run time) and the value as UTF-16 with its end mark, in one write.
fn loader_var(root: &SysRoot, name: &str, value: &str) -> std::io::Result<()> {
    let path = root.path(format!(
        "/sys/firmware/efi/efivars/{name}-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"
    ));
    if path.exists() {
        // efivarfs makes each variable file immutable
        let _ = Command::new("chattr").arg("-i").arg(&path).output();
    }
    let mut bytes = vec![7, 0, 0, 0];
    bytes.extend(value.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    std::fs::write(path, bytes)
}
