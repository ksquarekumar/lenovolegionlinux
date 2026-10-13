//! Read-only health checks: is the machine in the policy the profile describes, and is anything known to fight it
//! (old Legion tooling, missing or unwanted kernel modules, DKMS builds, throttling, dirty NTFS volumes, GPU wiring,
//! failed units, the OSD extension)?

use crate::gpu;
use crate::kernel;
use crate::ledger::Key;
use crate::policy::Machine;
use crate::profile::Profile;
use crate::snapshot::{cpufreq_counts, dmi_mismatches};
use crate::sysroot::SysRoot;
use crate::{firmware, system};

fn quoted(s: &str) -> String {
    format!("'{s}'")
}

/// Unit states that mean "not running and not coming back".
fn unit_quiet(active: &str, enabled: &str) -> bool {
    active != "active" && ["disabled", "not-found", "masked", "unknown"].contains(&enabled)
}

fn module_warnings(root: &SysRoot, profile: &Profile) -> Vec<String> {
    let spec = &profile.kernel_modules;
    let mut out = Vec::new();
    let kernels = kernel::installed_kernels(root);
    for module in &spec.required {
        for k in &kernels {
            if !kernel::module_available(module, k) {
                out.push(format!(
                    "required module {module} is missing for kernel {k}"
                ));
            }
        }
    }
    let loaded = kernel::loaded_modules(root);
    let running = kernel::running_kernel(root);
    for module in &spec.required {
        if !loaded.contains(module) && !kernel::module_builtin(root, module, &running) {
            out.push(format!("required module {module} is not loaded"));
        }
    }
    for (module, source) in kernel::autoload_entries(root) {
        if !kernel::module_available(&module, &running) {
            out.push(format!(
                "{source} autoloads {module}, which does not exist for {running} (fails every boot)"
            ));
        }
        if spec.unwanted_autoload.contains(&module) {
            out.push(format!(
                "{source} autoloads {module}, which this machine should not load"
            ));
        }
    }
    let rows = kernel::dkms_status();
    let mut names: Vec<&String> = rows.iter().map(|r| &r.0).collect();
    names.sort();
    names.dedup();
    for name in names {
        for k in &kernels {
            let built = rows
                .iter()
                .any(|r| &r.0 == name && &r.1 == k && r.2.starts_with("installed"));
            if !built {
                out.push(format!("DKMS module {name} is not built for kernel {k}"));
            }
        }
    }
    out
}

/// Every finding, in a stable order; empty when nothing is wrong.
pub fn doctor(machine: &Machine) -> Vec<String> {
    let (root, profile) = (machine.root, machine.profile);
    let mut w = Vec::new();
    if let Some(reason) = root.read("/run/legionctl/safe-mode") {
        w.push(format!("safe mode: {reason}"));
    }
    if !dmi_mismatches(root, profile).is_empty() {
        w.push("DMI does not match this repo profile".into());
    }

    let ac = machine.ac_online();
    let policy = if ac { &profile.ac } else { &profile.battery };
    let override_mode = machine.ledger.get(Key::Override);
    let current = root.read(&profile.paths.platform_profile);
    let wanted = override_mode
        .clone()
        .or_else(|| machine.platform_choice(policy));
    if let Some(wanted) = &wanted
        && current.as_deref() != Some(wanted.as_str())
    {
        w.push(format!(
            "platform_profile is {}, expected {}",
            current.as_deref().map_or("None".into(), quoted),
            quoted(wanted)
        ));
    }
    if let (Some(path), Some(fan)) = (
        &profile.paths.fan_mode,
        ac.then_some(()).and(profile.ac.firmware.fan_mode.as_ref()),
    ) && !fan.is_null()
    {
        let fan = crate::profile::text(fan);
        let now = root.read(path);
        if now.as_deref() != Some(fan.as_str()) {
            w.push(format!(
                "{path} is {}, expected {}",
                now.as_deref().map_or("None".into(), quoted),
                quoted(&fan)
            ));
        }
    }

    // following Fn+Q: the governor comes from firmware_follow, EPP from power-profiles-daemon
    let (governor, epp) = match &override_mode {
        Some(mode) => (
            profile
                .firmware_follow
                .get(mode)
                .and_then(|f| f.governor.clone()),
            None,
        ),
        None => (
            policy.cpu.governor.clone(),
            policy.cpu.energy_performance_preference.clone(),
        ),
    };
    for (file, label, expected) in [
        ("scaling_governor", "governor", governor),
        ("energy_performance_preference", "EPP", epp),
    ] {
        if let Some(expected) = expected {
            let wrong: usize = cpufreq_counts(root, file)
                .iter()
                .filter(|(v, _)| **v != expected)
                .map(|(_, n)| n)
                .sum();
            if wrong > 0 {
                w.push(format!(
                    "{wrong} cpufreq policies are not using {label} {}",
                    quoted(&expected)
                ));
            }
        }
    }

    let mut sysctl = profile.sysctl.clone();
    sysctl.extend(policy.sysctl.clone());
    for (key, value) in sysctl {
        if let Some(now) = root.read(format!("/proc/sys/{}", key.replace('.', "/")))
            && now != value
        {
            w.push(format!(
                "sysctl {key} is {}, expected {}",
                quoted(&now),
                quoted(&value)
            ));
        }
    }

    w.extend(conflict_warnings(root, profile));

    w.extend(module_warnings(root, profile));
    if let Some(reasons) = firmware::logged_throttle_reasons(root)
        && reasons
            .iter()
            .any(|r| ["PROCHOT", "critical temperature"].contains(r))
    {
        w.push(format!("CPU throttling logged: {}", reasons.join(", ")));
    }
    for volume in crate::windows::dirty_ntfs() {
        w.push(format!(
            "NTFS volume {volume} is marked dirty; run chkdsk in Windows (or ntfsfix -d, see docs/design/11-disks.md)"
        ));
    }
    w.extend(gpu_warnings(root, profile, ac));
    w.extend(desktop_warnings());
    w
}

const OSD_EXTENSION: &str = "legionctl-osd@legionctl";

/// Failed system and user units, and the OSD extension when it is not running.
fn desktop_warnings() -> Vec<String> {
    let mut w: Vec<String> = system::failed_units(false)
        .into_iter()
        .map(|u| format!("unit {u} failed; see systemctl status {u}"))
        .chain(
            system::failed_units(true)
                .into_iter()
                .map(|u| format!("user unit {u} failed; see systemctl --user status {u}")),
        )
        .collect();
    // ENABLED is the name of ACTIVE before GNOME 45
    if let Some(state) = system::extension_state(OSD_EXTENSION)
        && !["ACTIVE", "ENABLED"].contains(&state.as_str())
    {
        let fix = if state == "ERROR" || state == "OUT OF DATE" {
            "see journalctl --user -b /usr/bin/gnome-shell".to_string()
        } else {
            format!("gnome-extensions enable {OSD_EXTENSION}, then log in again")
        };
        w.push(format!(
            "the OSD extension {OSD_EXTENSION} is {state}; Fn+Q, Fn+R and Fn lock show no OSD ({fix})"
        ));
    }
    w
}

/// Units, packages and modules that fight legionctl over the same knobs.
fn conflict_warnings(root: &SysRoot, profile: &Profile) -> Vec<String> {
    let mut w = Vec::new();
    for unit in &profile.conflicts.systemd_units {
        let active = system::unit_state("is-active", unit);
        let enabled = system::unit_state("is-enabled", unit);
        if !unit_quiet(&active, &enabled) {
            w.push(format!(
                "conflicting unit {unit}: active={active}, enabled={enabled}"
            ));
        }
    }
    for package in &profile.conflicts.packages {
        if system::package_installed(package) {
            w.push(format!("conflicting package is installed: {package}"));
        }
    }
    let loaded = kernel::loaded_modules(root);
    for module in &profile.conflicts.modules {
        if loaded.contains(module) {
            w.push(format!("conflicting module is loaded: {module}"));
        }
    }
    if loaded.contains("thinkpad_acpi")
        && root
            .read("/proc/acpi/ibm/fan")
            .is_some_and(|f| f.contains("status:\t\tenabled"))
    {
        w.push(
            "thinkpad_acpi fan control appears enabled; this is not trusted on this machine".into(),
        );
    }
    w
}

/// Desktop GPU wiring and NVIDIA power on AC.
fn gpu_warnings(root: &SysRoot, profile: &Profile, ac: bool) -> Vec<String> {
    let mut w = Vec::new();
    let outputs = gpu::outputs(root);
    let external_on_nvidia = outputs
        .iter()
        .any(|o| o.connected && o.driver == "nvidia" && !o.connector.starts_with("eDP"));
    if gpu::mode(&outputs) == gpu::GpuMode::Hybrid
        && external_on_nvidia
        && gpu::desktop_preference(root) != gpu::DesktopGpu::Nvidia
    {
        w.push(
            "an external display runs on NVIDIA but the desktop may be drawn on Intel; try `legionctl gpu primary nvidia`"
                .into(),
        );
    }

    if ac {
        for unit in ["nvidia-powerd.service", "nvidia-persistenced.service"] {
            let state = system::unit_state("is-active", unit);
            if !["active", "unknown"].contains(&state.as_str()) {
                w.push(format!("{unit} is not active on AC"));
            }
        }
        // nvidia-powerd Dynamic Boost moves the limit between the base and base+25 W
        if let (Some(limit), Some(expected)) = (
            system::nvidia_power_limit(),
            profile.ac.nvidia.expected_dynamic_power_w,
        ) && !limit
            .parse::<f64>()
            .is_ok_and(|l| (0.0..=25.0).contains(&(l - expected)))
        {
            w.push(format!(
                "NVIDIA power limit is {limit} W, expected around {expected} W on AC"
            ));
        }
    }
    w
}
