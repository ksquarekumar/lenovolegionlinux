//! Machine policy commands: apply, reassert, follow, validate. Plans come from legionctl-machine; this module only
//! gathers what the planner needs from the live system and prints or executes the plan.

use std::path::Path;
use std::process::Command;

use anyhow::Result;
use legionctl_machine::policy::{self, Machine, Options, Source, Step};
use legionctl_machine::profile::{self, Profile};
use legionctl_machine::snapshot::{Detail, Snapshot};
use legionctl_machine::sysroot::SysRoot;

fn has_command(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(name)).is_ok_and(|m| {
                m.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
    })
}

fn ppd_current() -> Option<String> {
    let out = Command::new("powerprofilesctl").arg("get").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn machine<'a>(root: &'a SysRoot, profile: &'a Profile) -> Machine<'a> {
    Machine {
        root,
        profile,
        ppd_current: has_command("powerprofilesctl").then(ppd_current).flatten(),
        has_command: &has_command,
    }
}

/// Print a dry run, or execute; warnings go to stderr, the rest to stdout unless quiet.
fn finish(root: &SysRoot, steps: &[Step], dry_run: bool, quiet: bool) {
    let lines: Vec<String> = if dry_run {
        steps.iter().filter_map(policy::describe).collect()
    } else {
        policy::execute(root, steps)
    };
    for line in lines {
        if line.starts_with("warning: ") {
            eprintln!("{line}");
        } else if !quiet {
            println!("{line}");
        }
    }
}

pub struct Run<'a> {
    pub profile: Option<&'a Path>,
    pub dry_run: bool,
    pub quiet: bool,
}

pub fn apply(run: Run, source: Option<Source>, opts: Options) -> Result<()> {
    let (profile, _) = profile::load(run.profile)?;
    let root = SysRoot::from_env();
    let (source, steps) = machine(&root, &profile).apply(source, opts)?;
    finish(&root, &steps, run.dry_run, run.quiet);
    if !run.quiet {
        let verb = if opts.reassert {
            "reasserted"
        } else {
            "applied"
        };
        let what = if opts.reassert {
            "firmware policy"
        } else {
            "policy"
        };
        println!("{verb} {} {what}", source.name());
    }
    Ok(())
}

pub fn follow(run: Run) -> Result<()> {
    let (profile, _) = profile::load(run.profile)?;
    let root = SysRoot::from_env();
    let steps = machine(&root, &profile).follow()?;
    finish(&root, &steps, run.dry_run, run.quiet);
    Ok(())
}

/// Prints each problem; returns whether the profile is valid.
pub fn validate(path: Option<&Path>) -> Result<bool> {
    let path = path.unwrap_or(Path::new(profile::DEFAULT_PATH));
    let errors = profile::validate(path)?;
    for e in &errors {
        println!("- {e}");
    }
    println!(
        "validate: {} ({})",
        if errors.is_empty() { "ok" } else { "failed" },
        path.display()
    );
    Ok(errors.is_empty())
}

pub fn snapshot(profile: Option<&Path>, detail: Detail) -> Result<Snapshot> {
    let (profile, _) = profile::load(profile)?;
    Ok(Snapshot::read(&SysRoot::from_env(), &profile, detail))
}

fn counts(map: &std::collections::BTreeMap<String, usize>) -> String {
    let parts: Vec<String> = map.iter().map(|(k, n)| format!("{k}×{n}")).collect();
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" ")
    }
}

pub fn print_status(s: &Snapshot) {
    let v = |k: &str| {
        s.firmware_versions
            .get(k)
            .cloned()
            .flatten()
            .unwrap_or_else(|| "?".into())
    };
    println!(
        "profile {} · DMI {} · BIOS {} ({}) · EC {}",
        s.profile_id,
        if s.dmi_mismatches.is_empty() {
            "match"
        } else {
            "MISMATCH"
        },
        v("bios_version"),
        v("bios_release"),
        v("ec_firmware_release")
    );
    for m in &s.dmi_mismatches {
        println!(
            "  {}: expected '{}', got {}",
            m.key,
            m.expected,
            m.actual
                .as_deref()
                .map_or("none".into(), |a| format!("'{a}'"))
        );
    }
    println!(
        "power: {}  mode: {}{}  (choices: {})",
        s.power_source,
        s.platform_profile.as_deref().unwrap_or("?"),
        s.fn_q_override
            .as_ref()
            .map_or(String::new(), |o| format!(" (Fn+Q: {o})")),
        s.platform_profile_choices.join(" ")
    );
    let c = |k: &str| {
        s.cpu_settings
            .get(k)
            .cloned()
            .flatten()
            .unwrap_or_else(|| "?".into())
    };
    println!(
        "cpu: governor {}  EPP {}  no_turbo {}  max_perf_pct {}  ignore_ppc {}",
        counts(&s.governors),
        counts(&s.energy_performance_preference),
        c("no_turbo"),
        c("max_perf_pct"),
        c("ignore_ppc")
    );
    let limits: Vec<String> = s
        .firmware_limits
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    println!("firmware limits: {}", limits.join(" "));
    for zone in &s.rapl {
        let parts: Vec<String> = zone
            .constraints
            .iter()
            .map(|c| {
                format!(
                    "{} {}",
                    c.name,
                    c.limit_w.map_or("?".into(), |w| format!("{w:.0} W"))
                )
            })
            .collect();
        println!("rapl {}: {}", zone.zone, parts.join(", "));
    }
    println!(
        "temp: {}  fans: {}  throttle logged: {}",
        s.package_temp_c
            .map_or("?".into(), |t| format!("{t:.0} °C")),
        s.fan_rpm
            .as_ref()
            .map_or("unknown (needs root)".into(), |f| f
                .values()
                .map(|r| format!("{r} rpm"))
                .collect::<Vec<_>>()
                .join(" / ")),
        s.throttle_reasons
            .as_ref()
            .map_or("unknown (needs root)".into(), |r| if r.is_empty() {
                "none".into()
            } else {
                r.join(", ")
            })
    );
    println!(
        "gpu: {} · desktop on {} · NVIDIA limit {}",
        s.gpu_mode,
        s.desktop_gpu,
        s.nvidia_power_limit_w
            .as_deref()
            .map_or("?".into(), |w| format!("{w} W"))
    );
    for o in s.outputs.iter().filter(|o| o.connected) {
        println!("  {} on {} ({})", o.connector, o.card, o.driver);
    }
    for u in &s.units {
        println!("unit {}: {} / {}", u.unit, u.active, u.enabled);
    }
    let modules: Vec<String> = s
        .modules
        .iter()
        .map(|(m, l)| format!("{m}{}", if *l { "" } else { " (not loaded)" }))
        .collect();
    println!("modules: {}", modules.join(", "));
    println!("{}", legionctl_machine::load::brief(&s.load));
}
