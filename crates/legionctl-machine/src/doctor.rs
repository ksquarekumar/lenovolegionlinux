//! Read-only health checks: is the machine in the policy the profile describes, and is anything known to fight it
//! (old Legion tooling, missing or unwanted kernel modules, DKMS builds, throttling, dirty NTFS volumes, GPU wiring,
//! failed units, the OSD extension)?

use anyhow::Result;

use crate::gpu;
use crate::kernel;
use crate::ledger::Key;
use crate::policy::Machine;
use crate::profile::Profile;
use crate::snapshot::{cpufreq_counts, dmi_mismatches};
use crate::sysroot::SysRoot;
use crate::system;

fn quoted(s: &str) -> String {
    format!("'{s}'")
}

/// Unit states that mean "not running and not coming back".
fn unit_quiet(active: &str, enabled: &str) -> bool {
    active != "active" && ["disabled", "not-found", "masked"].contains(&enabled)
}

fn module_warnings(root: &SysRoot, profile: &Profile) -> Result<Vec<String>> {
    let spec = &profile.kernel_modules;
    let mut out = Vec::new();
    let kernels = kernel::installed_kernels(root)?;
    for module in &spec.required {
        for k in &kernels {
            if !kernel::module_available(module, k)? {
                out.push(format!(
                    "required module {module} is missing for kernel {k}"
                ));
            }
        }
    }
    let loaded = kernel::loaded_modules(root)?;
    let running = kernel::running_kernel(root)?;
    for module in &spec.required {
        if !loaded.contains(module) && !kernel::module_builtin(root, module, &running)? {
            out.push(format!("required module {module} is not loaded"));
        }
    }
    for (module, source) in kernel::autoload_entries(root)? {
        if !kernel::module_available(&module, &running)? {
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
    let rows = kernel::dkms_status()?;
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
    Ok(out)
}

/// One finding: its text, and the operation (with its input) that fixes it, when legionctl has one.
pub struct Warning {
    pub text: String,
    pub fix: Option<(&'static str, &'static str, serde_json::Value)>,
}

impl From<String> for Warning {
    fn from(text: String) -> Self {
        Self { text, fix: None }
    }
}

/// The findings of one check, or the reason why it cannot run (a finding too: a check that cannot run is never
/// a pass); the next `doctor` runs it again.
fn check<T: Into<Warning>>(w: &mut Vec<Warning>, name: &str, findings: Result<Vec<T>>) {
    match findings {
        Ok(found) => w.extend(found.into_iter().map(Into::into)),
        Err(e) => w.push(
            format!("the {name} check could not run ({e:#}); the next health check runs it again")
                .into(),
        ),
    }
}

/// Every finding, in a stable order; empty when nothing is wrong.
#[must_use]
pub fn doctor(machine: &Machine) -> Vec<Warning> {
    let (root, profile) = (machine.root, machine.profile);
    let mut w = Vec::new();
    check(&mut w, "daemon state", state_warnings(root, profile));
    check(&mut w, "policy", policy_warnings(machine));
    check(&mut w, "conflict", conflict_warnings(root, profile));
    check(&mut w, "kernel module", module_warnings(root, profile));
    check(&mut w, "Legion WMI", wmi_warnings(root, profile));
    check(&mut w, "boot image", crate::boot::unbuilt(root));
    check(&mut w, "boot entry", crate::boot::not_default(root));
    check(
        &mut w,
        "Windows volumes",
        crate::windows::dirty_ntfs()
            .map(|v| v.iter().map(|v| crate::windows::dirty_hazard(v)).collect()),
    );
    check(&mut w, "GPU", gpu_warnings(machine));
    check(&mut w, "desktop", desktop_warnings());
    check(&mut w, "login screen", crate::greeter::findings(root));
    w
}

/// Written by the daemon while the machine state that it announces cannot be read (the reason).
pub const OBSERVE_ERROR: &str = "/run/legionctl/observe.error";
/// Written by the daemon while the plan of an event fails (the event and the reason); removed when a plan works.
pub const POLICY_ERROR: &str = "/run/legionctl/policy.error";

/// Safe mode, the daemon's state loop, the workload sensor, and the DMI identity.
fn state_warnings(root: &SysRoot, profile: &Profile) -> Result<Vec<String>> {
    let mut w = Vec::new();
    if let Some(reason) = root.read("/run/legionctl/safe-mode")? {
        w.push(format!("safe mode: {reason}"));
    }
    if let Some(reason) = root.read(OBSERVE_ERROR)? {
        w.push(format!(
            "legionctl-system cannot read the machine state that it announces (mode, workload, fan): {reason}"
        ));
    }
    if let Some(reason) = root.read(POLICY_ERROR)? {
        w.push(format!(
            "legionctl-system cannot carry out the policy, the machine keeps its old state: {reason}"
        ));
    }
    if let Some(reason) = root.read(crate::configset::WORKLOAD_ERROR)? {
        w.push(format!(
            "the eBPF workload sensor does not run: {reason}; layers with a workload condition never apply"
        ));
    }
    if !dmi_mismatches(root, profile)?.is_empty() {
        w.push("this machine does not match legionctl's machine profile".into());
    }
    Ok(w)
}

/// The machine against its policy: firmware mode, fan mode, governor and EPP, sysctl.
fn policy_warnings(machine: &Machine) -> Result<Vec<String>> {
    let (root, profile) = (machine.root, machine.profile);
    let mut w = Vec::new();
    let override_mode = machine.ledger.get(Key::Override)?;
    let wanted = machine.desired(machine.source()?, override_mode.as_deref())?;
    w.extend(crate::configset::findings(machine.layers));
    let current = root.read(&profile.paths.platform_profile)?;
    let expected_mode = match override_mode.clone() {
        Some(mode) => Some(mode),
        None => machine.platform_choice(&wanted)?,
    };
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
        let now = root.read(path)?;
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
            let wrong: usize = cpufreq_counts(root, file)?
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
        if let Some(now) = root.read(format!("/proc/sys/{}", key.replace('.', "/")))?
            && now != value
        {
            w.push(format!(
                "sysctl {key} is {}, expected {}",
                quoted(&now),
                quoted(&value)
            ));
        }
    }
    Ok(w)
}

/// The Legion WMI driver families: legionctl's module and the upstream drivers it replaces.
const WMI_FAMILIES: [&str; 2] = ["legionctl_wmi", "lenovo_wmi_"];

/// The opt-in that makes udev load `legionctl_wmi` in place of the upstream drivers (todo-143).
const OPT_IN: &str = "/etc/modprobe.d/legionctl-wmi.conf";

/// The Legion WMI devices that `legionctl_wmi` binds (docs/design/08-kernel.md, What it binds).
const LEGION_WMI_GUIDS: [&str; 6] = [
    "887B54E3-DDDC-4B2C-8B88-68A26A8835D0",
    "DC2A8805-3A8C-41BA-A6F7-092E0089CD3B",
    "7A8F5407-CB67-4D6E-B547-39B3BE018154",
    "92549549-4BDE-4F06-AC04-CE8BF898DBAA",
    "D320289E-8FEA-41E0-86F9-911D83151B5F",
    "CE6C0974-0407-4F50-88BA-4FC3B6559AD8",
];

/// Two driver families on the Legion WMI devices (todo-143: `legionctl_wmi` autoloaded beside the upstream drivers),
/// a Legion WMI device that a loaded `legionctl_wmi` does not have, and no `platform_profile` while a Legion WMI
/// driver is loaded. A `legionctl_wmi` that is not loaded is the kernel module check's finding (the profile requires
/// it).
fn wmi_warnings(root: &SysRoot, profile: &Profile) -> Result<Vec<String>> {
    let mut w = Vec::new();
    let mut loaded = Vec::new();
    let mut bound = Vec::new();
    let mut ours = Vec::new();
    for driver in root.children("/sys/bus/wmi/drivers", "")? {
        let name = driver.file_name().unwrap().to_string_lossy().into_owned();
        let Some(family) = WMI_FAMILIES.iter().find(|f| name.starts_with(**f)) else {
            continue;
        };
        // a bound device is a link named <GUID>-<instance>; bind, unbind, module and uevent are lower case
        let mut devices = Vec::new();
        for d in root.children(&driver, "")? {
            let name = d.file_name().unwrap().to_string_lossy().into_owned();
            // read as a link, so that a recording (`record`) keeps the bound device
            if name.len() > 36 && name.as_bytes()[8] == b'-' && root.link(&d)?.is_some() {
                devices.push(name);
            }
        }
        if *family == WMI_FAMILIES[0] {
            ours.extend(devices.iter().map(|d| d[..36].to_string()));
        }
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
             side has all of them",
            drivers.join("; ")
        ));
    }
    let missing: Vec<&str> = LEGION_WMI_GUIDS
        .into_iter()
        .filter(|g| !ours.iter().any(|d| d == g))
        .collect();
    let ours_loaded = loaded.iter().any(|d| d.starts_with(WMI_FAMILIES[0]));
    if !ours_loaded && !root.exists(OPT_IN)? {
        w.push(format!(
            "{OPT_IN} is missing: legionctl_wmi does not load at start, and the fan controls fail without it"
        ));
    }
    if ours_loaded && !missing.is_empty() {
        w.push(format!(
            "legionctl_wmi is loaded but does not have the Legion WMI devices {}: the fan, power mode and firmware \
             attribute files of these devices are missing",
            missing.join(", ")
        ));
    }
    if !loaded.is_empty() && !root.exists(&profile.paths.platform_profile)? {
        w.push(format!(
            "{} is missing while {} is loaded: legionctl cannot set the power mode",
            profile.paths.platform_profile,
            loaded.join(", ")
        ));
    }
    Ok(w)
}

/// A unit's name as people write it: systemd's `\xNN` escapes (`mnt-windows\x2ddata.mount`) as their characters.
fn unit_name(unit: &str) -> String {
    let mut out = Vec::new();
    let mut rest = unit.as_bytes();
    while let Some(at) = rest.windows(2).position(|w| w == b"\\x") {
        out.extend_from_slice(&rest[..at]);
        let byte = rest
            .get(at + 2..at + 4)
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        if let Some(byte) = byte {
            out.push(byte);
            rest = &rest[at + 4..];
        } else {
            out.extend_from_slice(b"\\x");
            rest = &rest[at + 2..];
        }
    }
    out.extend_from_slice(rest);
    String::from_utf8_lossy(&out).into_owned()
}

/// Failed system and user units (the OSD extension is a row of `setup check`, which doctor shows).
fn desktop_warnings() -> Result<Vec<String>> {
    Ok(system::failed_units(false)?
        .into_iter()
        .map(|u| format!("the system unit {} failed", unit_name(&u)))
        .chain(
            system::failed_units(true)?
                .into_iter()
                .map(|u| format!("the unit {} of your login failed", unit_name(&u))),
        )
        .collect())
}

/// Units, packages and modules that fight legionctl over the same knobs.
fn conflict_warnings(root: &SysRoot, profile: &Profile) -> Result<Vec<String>> {
    let mut w = Vec::new();
    for unit in &profile.conflicts.systemd_units {
        let active = system::unit_state("is-active", unit)?;
        let enabled = system::unit_state("is-enabled", unit)?;
        if !unit_quiet(&active, &enabled) {
            w.push(format!(
                "{unit} fights legionctl ({active}, {enabled}): run legionctl cleanup"
            ));
        }
    }
    for package in &profile.conflicts.packages {
        if system::package_installed(package)? {
            w.push(format!("conflicting package is installed: {package}"));
        }
    }
    let loaded = kernel::loaded_modules(root)?;
    for module in &profile.conflicts.modules {
        if loaded.contains(module) {
            w.push(format!("conflicting module is loaded: {module}"));
        }
    }
    if loaded.contains("thinkpad_acpi")
        && root
            .read("/proc/acpi/ibm/fan")?
            .is_some_and(|f| f.contains("status:\t\tenabled"))
    {
        w.push(
            "thinkpad_acpi fan control appears enabled; this is not trusted on this machine".into(),
        );
    }
    Ok(w)
}

/// Desktop GPU wiring, and NVIDIA power where the layers start its services (on AC).
fn gpu_warnings(machine: &Machine) -> Result<Vec<Warning>> {
    let (root, profile) = (machine.root, machine.profile);
    let fn_q = machine.ledger.get(Key::Override)?;
    let services = machine
        .desired(machine.source()?, fn_q.as_deref())?
        .text("nvidia.services")
        .as_deref()
        == Some("start");
    let mut w = Vec::new();
    let outputs = gpu::outputs(root)?;
    let external_on_nvidia = outputs
        .iter()
        .any(|o| o.connected && o.driver == "nvidia" && !o.connector.starts_with("eDP"));
    if gpu::mode(&outputs) == gpu::GpuMode::Hybrid
        && external_on_nvidia
        && gpu::desktop_preference(root)? != gpu::DesktopGpu::Nvidia
    {
        w.push(Warning {
            text: "an external display runs on NVIDIA but the desktop may be drawn on Intel".into(),
            fix: Some((
                "Draw the desktop on NVIDIA",
                "gpu.primary",
                serde_json::json!({ "choice": "nvidia" }),
            )),
        });
    }

    if let Some(power) = gpu::nvidia_power(root)?
        && power.control.as_deref() == Some("on")
    {
        w.push(Warning {
            text: "the NVIDIA GPU never sleeps (power/control is on)".into(),
            fix: Some((
                "Let the NVIDIA GPU sleep",
                "gpu.power",
                serde_json::json!({ "state": "off" }),
            )),
        });
    }
    if root
        .read("/proc/driver/nvidia/params")?
        .is_some_and(|p| p.lines().any(|l| l.trim() == "DynamicPowerManagement: 0"))
    {
        w.push(
            "the nvidia driver has runtime power management off (NVreg_DynamicPowerManagement=0): the GPU never \
             reaches D3cold"
                .to_string()
                .into(),
        );
    }

    if services {
        for unit in ["nvidia-powerd.service", "nvidia-persistenced.service"] {
            let state = system::unit_state("is-active", unit)?;
            if state != "active" {
                w.push(format!("{unit} is not active on AC").into());
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
            w.push(
                format!("NVIDIA power limit is {limit} W, expected around {expected} W on AC")
                    .into(),
            );
        }
    }
    Ok(w)
}
