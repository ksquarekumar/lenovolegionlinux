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
    out.extend(wmi_warnings(root, profile));
    out
}

/// Every finding, in a stable order; empty when nothing is wrong.
pub fn doctor(machine: &Machine) -> Vec<String> {
    let (root, profile) = (machine.root, machine.profile);
    let mut w = Vec::new();
    if let Some(reason) = root.read("/run/legionctl/safe-mode") {
        w.push(format!("safe mode: {reason}"));
    }
    if let Some(reason) = root.read(crate::configset::WORKLOAD_ERROR) {
        w.push(format!(
            "the eBPF workload sensor does not run: {reason}; layers with a workload condition never apply"
        ));
    }
    if !dmi_mismatches(root, profile).is_empty() {
        w.push("DMI does not match this repo profile".into());
    }

    let override_mode = machine.ledger.get(Key::Override);
    let wanted = machine.desired(machine.source(), override_mode.as_deref());
    w.extend(crate::configset::findings(machine.layers));
    let current = root.read(&profile.paths.platform_profile);
    let expected_mode = override_mode
        .clone()
        .or_else(|| machine.platform_choice(&wanted));
    if let Some(expected_mode) = &expected_mode
        && current.as_deref() != Some(expected_mode.as_str())
    {
        w.push(format!(
            "platform_profile is {}, expected {}",
            current.as_deref().map_or("None".into(), quoted),
            quoted(expected_mode)
        ));
    }
    if let (Some(path), Some(fan)) = (&profile.paths.fan_mode, wanted.text("firmware.fan_mode")) {
        let now = root.read(path);
        if now.as_deref() != Some(fan.as_str()) {
            w.push(format!(
                "{path} is {}, expected {}",
                now.as_deref().map_or("None".into(), quoted),
                quoted(&fan)
            ));
        }
    }

    // following Fn+Q: the governor comes from the Fn+Q layer, EPP from power-profiles-daemon
    let (governor, epp) = match &override_mode {
        Some(mode) => (
            machine
                .follows(mode)
                .then(|| wanted.text("cpu.governor"))
                .flatten(),
            None,
        ),
        None => (wanted.text("cpu.governor"), wanted.text("cpu.epp")),
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

    for (key, value) in wanted.under("sysctl.") {
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
    w.extend(crate::boot::unbuilt(root));
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
    w.extend(gpu_warnings(
        root,
        profile,
        wanted.text("nvidia.services").as_deref() == Some("start"),
    ));
    w.extend(desktop_warnings());
    w
}

/// The Legion WMI driver families: legionctl's module and the upstream drivers it replaces.
const WMI_FAMILIES: [&str; 2] = ["legionctl_wmi", "lenovo_wmi_"];

/// Two driver families on the Legion WMI devices (todo-143: `legionctl_wmi` autoloaded beside the upstream drivers),
/// and no `platform_profile` while a Legion WMI driver is loaded.
fn wmi_warnings(root: &SysRoot, profile: &Profile) -> Vec<String> {
    let mut w = Vec::new();
    let mut loaded = Vec::new();
    let mut bound = Vec::new();
    for driver in root.children("/sys/bus/wmi/drivers", "") {
        let name = driver.file_name().unwrap().to_string_lossy().into_owned();
        let Some(family) = WMI_FAMILIES.iter().find(|f| name.starts_with(**f)) else {
            continue;
        };
        // a bound device is a link named <GUID>-<instance>; bind, unbind, module and uevent are lower case
        let devices: Vec<String> = root
            .children(&driver, "")
            .iter()
            .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
            .filter(|d| d.len() > 36 && d.as_bytes()[8] == b'-')
            .collect();
        if !devices.is_empty() {
            bound.push((*family, format!("{name} ({})", devices.join(", "))));
        }
        loaded.push(name);
    }
    if bound.iter().any(|b| b.0 == WMI_FAMILIES[0]) && bound.iter().any(|b| b.0 == WMI_FAMILIES[1])
    {
        let drivers: Vec<&str> = bound.iter().map(|b| b.1.as_str()).collect();
        w.push(format!(
            "the Legion WMI devices are bound to legionctl_wmi and to the upstream lenovo_wmi drivers: {}; neither \
             side has all of them (see docs/design/08-kernel.md, First live load)",
            drivers.join("; ")
        ));
    }
    if !loaded.is_empty() && !root.exists(&profile.paths.platform_profile) {
        w.push(format!(
            "{} is missing while {} is loaded: legionctl cannot set the power mode",
            profile.paths.platform_profile,
            loaded.join(", ")
        ));
    }
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

/// Desktop GPU wiring, and NVIDIA power where the layers start its services (on AC).
fn gpu_warnings(root: &SysRoot, profile: &Profile, services: bool) -> Vec<String> {
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

    if let Some(power) = gpu::nvidia_power(root)
        && power.control.as_deref() == Some("on")
    {
        w.push(
            "the NVIDIA GPU never sleeps (power/control is on); `legionctl gpu power off` lets it go to D3cold when no \
             program uses it"
                .into(),
        );
    }
    if root
        .read("/proc/driver/nvidia/params")
        .is_some_and(|p| p.lines().any(|l| l.trim() == "DynamicPowerManagement: 0"))
    {
        w.push(
            "the nvidia driver has runtime power management off (NVreg_DynamicPowerManagement=0): the GPU never \
             reaches D3cold"
                .into(),
        );
    }

    if services {
        for unit in ["nvidia-powerd.service", "nvidia-persistenced.service"] {
            let state = system::unit_state("is-active", unit);
            if !["active", "unknown"].contains(&state.as_str()) {
                w.push(format!("{unit} is not active on AC"));
            }
        }
        // nvidia-powerd Dynamic Boost moves the limit between the base and base+25 W
        if let (Some(limit), Some(expected)) = (
            system::nvidia_power_limit(),
            profile.nvidia.expected_dynamic_power_w,
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
