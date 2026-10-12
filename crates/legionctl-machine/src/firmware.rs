//! Firmware reads that need root: fan RPM through the Lenovo "Other Method" WMI interface via `acpi_call`, and the
//! package thermal status MSR. Both are read-only.

use std::io::{Read, Seek, SeekFrom};

use crate::sysroot::SysRoot;

const ACPI_CALL: &str = "/proc/acpi/call";
/// `\_SB_.GZFD.WMAE`, `GetFeatureValue` (method 17); feature 0x04030001/2 is fan 1/2 RPM.
const FAN_CALL: [(&str, &str); 2] = [
    ("fan1", "\\_SB_.GZFD.WMAE 0 17 b01000304"),
    ("fan2", "\\_SB_.GZFD.WMAE 0 17 b02000304"),
];

/// An `acpi_call` reply: `0x...` or a byte buffer `{0x.., ...}` (first 4 bytes, little endian); `None` for errors.
#[must_use]
pub fn parse_acpi_call(text: &str) -> Option<u64> {
    let text = text.trim().trim_end_matches('\0').trim();
    if let Some(hex) = text.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).ok();
    }
    let inner = text.strip_prefix('{')?.strip_suffix('}')?;
    let bytes: Vec<u8> = inner
        .split(',')
        .filter_map(|b| u8::from_str_radix(b.trim().trim_start_matches("0x"), 16).ok())
        .take(4)
        .collect();
    Some(
        bytes
            .iter()
            .rev()
            .fold(0, |acc, b| acc << 8 | u64::from(*b)),
    )
}

fn acpi_call(root: &SysRoot, call: &str) -> Option<u64> {
    std::fs::write(root.path(ACPI_CALL), call).ok()?;
    parse_acpi_call(&std::fs::read_to_string(root.path(ACPI_CALL)).ok()?)
}

/// Fan RPM from the firmware; `None` without root or `acpi_call`.
#[must_use]
pub fn fan_rpm(root: &SysRoot) -> Option<[(&'static str, u64); 2]> {
    let [(n1, c1), (n2, c2)] = FAN_CALL;
    Some([(n1, acpi_call(root, c1)?), (n2, acpi_call(root, c2)?)])
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

/// Logged throttle reasons from CPU 0's MSR; `None` without root or the msr module.
#[must_use]
pub fn logged_throttle_reasons(root: &SysRoot) -> Option<Vec<&'static str>> {
    let mut msr = std::fs::File::open(root.path("/dev/cpu/0/msr")).ok()?;
    msr.seek(SeekFrom::Start(PACKAGE_THERM_STATUS)).ok()?;
    let mut buf = [0u8; 8];
    msr.read_exact(&mut buf).ok()?;
    Some(throttle_reasons(u64::from_le_bytes(buf)))
}
