//! Machine policy commands: apply, reassert, follow, validate. Plans come from legionctl-machine; this module only
//! gathers what the planner needs from the live system and prints or executes the plan.

use std::path::Path;
use std::process::Command;

use anyhow::Result;
use legionctl_machine::policy::{self, Machine, Options, Source, Step};
use legionctl_machine::profile::{self, Profile};
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
