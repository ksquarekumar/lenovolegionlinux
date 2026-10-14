//! Firmware calls that need root: the Lenovo WMI methods through `acpi_call` (fan RPM here, the fan controls in
//! [`crate::fan`]), and the package thermal status MSR (read-only).

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Mutex, PoisonError};

use anyhow::{Context, Result, bail};

use crate::sysroot::SysRoot;

const ACPI_CALL: &str = "/proc/acpi/call";
/// `\_SB_.GZFD.WMAE`, `GetFeatureValue` (method 17); feature 0x04030001/2 is fan 1/2 RPM.
const FAN_CALL: [(&str, &str); 2] = [
    ("fan1", "\\_SB_.GZFD.WMAE 0 17 b01000304"),
    ("fan2", "\\_SB_.GZFD.WMAE 0 17 b02000304"),
];
/// `/proc/acpi/call` keeps one reply for all callers, so a call and its reply must not mix with another call.
static ONE_CALL: Mutex<()> = Mutex::new(());

/// The bytes of an `acpi_call` buffer reply `{0x.., ...}`; `None` for any other reply.
#[must_use]
pub fn parse_acpi_buffer(text: &str) -> Option<Vec<u8>> {
    let inner = text
        .trim()
        .trim_end_matches('\0')
        .trim()
        .strip_prefix('{')?
        .strip_suffix('}')?;
    Some(
        inner
            .split(',')
            .filter_map(|b| u8::from_str_radix(b.trim().trim_start_matches("0x"), 16).ok())
            .collect(),
    )
}

/// An `acpi_call` reply: `0x...` or a byte buffer `{0x.., ...}` (first 4 bytes, little endian); `None` for errors.
#[must_use]
pub fn parse_acpi_call(text: &str) -> Option<u64> {
    let text = text.trim().trim_end_matches('\0').trim();
    if let Some(hex) = text.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).ok();
    }
    Some(
        parse_acpi_buffer(text)?
            .iter()
            .take(4)
            .rev()
            .fold(0, |acc, b| acc << 8 | u64::from(*b)),
    )
}

/// Call an ACPI method through `acpi_call` and return its reply.
///
/// # Errors
/// `acpi_call` is not there or not writable (no root), or the method failed (`Error: ...`).
pub fn acpi_call(root: &SysRoot, call: &str) -> Result<String> {
    let _one = ONE_CALL.lock().unwrap_or_else(PoisonError::into_inner);
    std::fs::write(root.path(ACPI_CALL), call).with_context(|| format!("calling {call}"))?;
    let reply = std::fs::read_to_string(root.path(ACPI_CALL))
        .with_context(|| format!("reading the reply to {call}"))?;
    let reply = reply.trim().trim_end_matches('\0').trim();
    if reply.starts_with("Error") || reply == "not called" {
        bail!("{call}: {reply}");
    }
    Ok(reply.to_string())
}

/// Fan RPM from the firmware; `None` without root or `acpi_call`.
#[must_use]
pub fn fan_rpm(root: &SysRoot) -> Option<[(&'static str, u64); 2]> {
    let read = |call| parse_acpi_call(&acpi_call(root, call).ok()?);
    let [(n1, c1), (n2, c2)] = FAN_CALL;
    Some([(n1, read(c1)?), (n2, read(c2)?)])
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
