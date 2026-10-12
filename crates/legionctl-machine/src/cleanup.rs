//! Disable what earlier Legion tooling left behind: conflicting units always, and loaded modules and installed
//! packages only when asked.

use anyhow::Result;

use crate::kernel;
use crate::policy::{Machine, Step};
use crate::system;

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub force: bool,
    pub unload_modules: bool,
    pub remove_packages: bool,
}

pub fn plan(machine: &Machine, opts: Options) -> Result<Vec<Step>> {
    machine.require_supported_machine(opts.force)?;
    let conflicts = &machine.profile.conflicts;
    let mut steps = Vec::new();
    for unit in &conflicts.systemd_units {
        let active = system::unit_state("is-active", unit);
        let enabled = system::unit_state("is-enabled", unit);
        if active == "active"
            || !["disabled", "not-found", "masked", "unknown"].contains(&enabled.as_str())
        {
            machine.run(&mut steps, &["systemctl", "disable", "--now", unit]);
        }
        steps.push(Step::Try(vec![
            "systemctl".into(),
            "reset-failed".into(),
            unit.clone(),
        ]));
    }
    let loaded = kernel::loaded_modules(machine.root);
    for module in conflicts.modules.iter().filter(|m| loaded.contains(*m)) {
        if opts.unload_modules {
            machine.run(&mut steps, &["modprobe", "-r", module]);
        } else {
            steps.push(Step::Warn(format!(
                "obsolete module is loaded but was not unloaded without --unload-modules: {module}"
            )));
        }
    }
    let installed: Vec<&String> = conflicts
        .packages
        .iter()
        .filter(|p| system::package_installed(p))
        .collect();
    if opts.remove_packages && !installed.is_empty() {
        let mut argv = vec!["pacman", "-Rns"];
        argv.extend(installed.iter().map(|p| p.as_str()));
        machine.run(&mut steps, &argv);
    } else {
        for package in installed {
            steps.push(Step::Warn(format!(
                "conflicting package is installed but was not removed without --remove-packages: {package}"
            )));
        }
    }
    Ok(steps)
}
