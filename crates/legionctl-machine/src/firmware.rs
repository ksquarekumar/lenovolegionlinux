//! The firmware through the `legionctl-wmi` module's sysfs (docs/design/08-kernel.md, sysfs ABI): fan RPM here, the fan
//! controls in [`crate::fan`]; and the package thermal status MSR (read-only).

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::sysroot::SysRoot;

/// The `name` of the `legionctl-wmi` module's hwmon device.
pub const LEGIONCTL_WMI: &str = "legionctl_wmi";
/// Why a fan read or write fails when the module's hwmon device is not there.
pub const LEGIONCTL_WMI_MISSING: &str = "legionctl_wmi is not loaded: see docs/design/08-kernel.md";

/// The hwmon device named `name`, if there is one.
///
/// # Errors
/// A hwmon device's name cannot be read.
pub fn hwmon(root: &SysRoot, name: &str) -> Result<Option<PathBuf>> {
    for h in root.children("/sys/class/hwmon", "hwmon")? {
        if root.read(h.join("name"))?.as_deref() == Some(name) {
            return Ok(Some(h));
        }
    }
    Ok(None)
}

/// The hwmon device of the `legionctl-wmi` module, found by its name (the number changes from start to start).
///
/// # Errors
/// The module is not loaded ([`LEGIONCTL_WMI_MISSING`]), or a hwmon device's name cannot be read.
pub fn legionctl_wmi(root: &SysRoot) -> Result<PathBuf> {
    match hwmon(root, LEGIONCTL_WMI)? {
        Some(h) => Ok(h),
        None => bail!(LEGIONCTL_WMI_MISSING),
    }
}

/// One number of the module's hwmon device (`fan1_input`, `pwm1_enable`, ...).
///
/// # Errors
/// The file is not there or not a number.
pub fn read_number(root: &SysRoot, hwmon: &std::path::Path, file: &str) -> Result<u64> {
    let path = hwmon.join(file);
    let text = root
        .read(&path)?
        .with_context(|| format!("{} is missing", path.display()))?;
    text.parse()
        .with_context(|| format!("{} is '{text}', not a number", path.display()))
}

/// Fan RPM of fan 1 and fan 2 from the module's hwmon device.
///
/// # Errors
/// The module is not loaded ([`LEGIONCTL_WMI_MISSING`]), or a fan file cannot be read.
pub fn fan_rpm(root: &SysRoot) -> Result<BTreeMap<String, u64>> {
    let hwmon = legionctl_wmi(root)?;
    ["fan1", "fan2"]
        .into_iter()
        .map(|fan| {
            Ok((
                fan.to_string(),
                read_number(root, &hwmon, &format!("{fan}_input"))?,
            ))
        })
        .collect()
}

const PACKAGE_THERM_STATUS: u64 = 0x1B1;
/// `IA32_PACKAGE_THERM_STATUS` log bits; they stay set until cleared, and we only read them.
const THROTTLE_LOG_BITS: [(u32, &str); 4] = [
    (1, "thermal"),
    (3, "PROCHOT"),
    (5, "critical temperature"),
    (11, "power limit"),
];

#[must_use]
pub fn throttle_reasons(value: u64) -> Vec<&'static str> {
    THROTTLE_LOG_BITS
        .iter()
        .filter(|(bit, _)| value >> bit & 1 == 1)
        .map(|(_, name)| *name)
        .collect()
}

/// `IA32_PACKAGE_THERM_STATUS` of CPU 0; `None` without root or the msr module.
fn package_therm_status(root: &SysRoot) -> Option<u64> {
    let mut msr = std::fs::File::open(root.path("/dev/cpu/0/msr")).ok()?;
    msr.seek(SeekFrom::Start(PACKAGE_THERM_STATUS)).ok()?;
    let mut buf = [0u8; 8];
    msr.read_exact(&mut buf).ok()?;
    Some(u64::from_le_bytes(buf))
}

/// Logged throttle reasons from CPU 0's MSR; `None` without root or the msr module.
#[must_use]
pub fn logged_throttle_reasons(root: &SysRoot) -> Option<Vec<&'static str>> {
    package_therm_status(root).map(throttle_reasons)
}

/// Throttle reasons active now: each status bit sits one below its log bit.
#[must_use]
pub fn current_throttle_reasons(root: &SysRoot) -> Option<Vec<&'static str>> {
    package_therm_status(root).map(|value| {
        THROTTLE_LOG_BITS
            .iter()
            .filter(|(bit, _)| value >> (bit - 1) & 1 == 1)
            .map(|(_, name)| *name)
            .collect()
    })
}
