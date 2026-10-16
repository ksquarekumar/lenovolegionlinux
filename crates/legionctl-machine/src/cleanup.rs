//! Disable what earlier Legion tooling left behind: conflicting units always, loaded modules only when asked.
//! Installed conflicting packages are reported with the command that removes them: pacman asks questions and writes
//! `/usr`, so a human runs it.

use anyhow::Result;

use crate::kernel;
use crate::policy::{Machine, Step};
use crate::system;

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub force: bool,
    pub unload_modules: bool,
}

/// Plan the cleanup that `opts` asks for.
///
/// # Errors
/// The machine does not match the profile and `opts.force` is not set.
pub fn plan(machine: &Machine, opts: Options) -> Result<Vec<Step>> {
    machine.require_supported_machine(opts.force)?;
    let conflicts = &machine.profile.conflicts;
    let mut steps = Vec::new();
    for unit in &conflicts.systemd_units {
        let active = system::unit_state("is-active", unit)?;
        let enabled = system::unit_state("is-enabled", unit)?;
        if active == "active" || !["disabled", "not-found", "masked"].contains(&enabled.as_str()) {
            machine.run(&mut steps, &["systemctl", "disable", "--now", unit]);
        }
        steps.push(Step::Try(vec![
            "systemctl".into(),
            "reset-failed".into(),
            unit.clone(),
        ]));
    }
    let loaded = kernel::loaded_modules(machine.root)?;
    for module in conflicts.modules.iter().filter(|m| loaded.contains(*m)) {
        if opts.unload_modules {
            machine.run(&mut steps, &["modprobe", "-r", module]);
        } else {
            steps.push(Step::Warn(format!(
                "obsolete module is loaded but was not unloaded without unload_modules: {module}"
            )));
        }
    }
    let mut installed: Vec<&str> = Vec::new();
    for p in &conflicts.packages {
        if system::package_installed(p)? {
            installed.push(p);
        }
    }
    if !installed.is_empty() {
        steps.push(Step::Warn(format!(
            "conflicting packages are installed; remove them with: sudo pacman -Rns {}",
            installed.join(" ")
        )));
    }
    Ok(steps)
}
