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

/// Findings as a report: `doctor: warnings` and one line per finding, or `doctor: no obvious conflicts`.
pub fn doctor(profile: Option<&Path>) -> Result<(bool, String)> {
    let (profile, _) = profile::load(profile)?;
    let root = SysRoot::from_env();
    let findings = legionctl_machine::doctor::doctor(&machine(&root, &profile));
    let report = if findings.is_empty() {
        "doctor: no obvious conflicts\n".to_string()
    } else {
        let lines: String = findings.iter().map(|f| format!("- {f}\n")).collect();
        format!("doctor: warnings\n{lines}")
    };
    Ok((findings.is_empty(), report))
}

pub fn gpu_status() {
    let root = SysRoot::from_env();
    let outputs = legionctl_machine::gpu::outputs(&root);
    println!("gpu_mode: {}", legionctl_machine::gpu::mode(&outputs));
    println!(
        "desktop GPU preference: {} (Mutter reads it at login)",
        legionctl_machine::gpu::desktop_preference(&root)
    );
    for o in outputs {
        println!(
            "  {}: {} ({}) {}",
            o.connector,
            o.card,
            o.driver,
            if o.connected { "connected" } else { "-" }
        );
    }
}

/// Which GPU Mutter draws the desktop on (`nvidia`, `intel`, `auto`); takes effect at the next login.
pub fn gpu_primary(choice: &str, dry_run: bool) -> Result<()> {
    let (profile, _) = profile::load(None)?;
    let root = SysRoot::from_env();
    let steps = legionctl_machine::gpu::set_desktop(&machine(&root, &profile), choice)?;
    finish(&root, &steps, dry_run, false);
    Ok(())
}

/// Niceness of this process, from /proc/self/stat (field 19).
fn current_nice() -> i32 {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(16)?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// Replace this process with `cmd`, rendering on `gpu`, pinned to a core class, at an absolute niceness.
pub fn run(gpu: &str, cores: &str, nice: Option<i32>, cmd: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    anyhow::ensure!(
        !cmd.is_empty(),
        "usage: legionctl run [--gpu nvidia|intel] [--cores p|e] [--nice N] -- command ..."
    );
    let root = SysRoot::from_env();
    let mut argv: Vec<String> = cmd.to_vec();
    if let Some(n) = nice {
        // `nice -n` is relative to the caller's niceness; make --nice the absolute value the program gets
        argv = [
            vec!["nice".into(), "-n".into(), (n - current_nice()).to_string()],
            argv,
        ]
        .concat();
    }
    if cores != "all" {
        let cpus = legionctl_machine::gpu::core_class_cpus(&root, cores).ok_or_else(|| {
            anyhow::anyhow!("this CPU does not report performance/efficiency core groups")
        })?;
        // a transient scope pins the program and everything it starts; keep `$` in the command literal
        let scope = [
            "systemd-run",
            "--user",
            "--scope",
            "--quiet",
            "--expand-environment=no",
            "-p",
        ];
        argv = [
            scope.iter().map(|s| s.to_string()).collect(),
            vec![format!("AllowedCPUs={cpus}"), "--".into()],
            argv,
        ]
        .concat();
    }
    let err = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .envs(legionctl_machine::gpu::render_env(&root, gpu))
        .exec();
    Err(err.into())
}

pub fn cleanup(run: Run, opts: legionctl_machine::cleanup::Options) -> Result<()> {
    let (profile, _) = profile::load(run.profile)?;
    let root = SysRoot::from_env();
    let steps = legionctl_machine::cleanup::plan(&machine(&root, &profile), opts)?;
    finish(&root, &steps, run.dry_run, run.quiet);
    if !run.quiet {
        println!("cleanup completed");
    }
    Ok(())
}

pub fn capabilities(path: Option<&Path>) -> Result<()> {
    let (profile, _) = profile::load(path)?;
    println!("profile: {}", profile.id);
    for c in &profile.capabilities {
        println!("{}: {}", c.id, c.name);
        let or = |v: &Option<String>| v.clone().unwrap_or_else(|| "unknown".into());
        println!("  status: {}", or(&c.status));
        println!("  current_owner: {}", or(&c.current_owner));
        println!("  planned_owner: {}", or(&c.planned_owner));
        if let Some(notes) = &c.notes {
            println!("  notes: {notes}");
        }
    }
    Ok(())
}
