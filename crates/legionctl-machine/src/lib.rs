//! Machine control for this Lenovo Legion. Every path goes through a [`sysroot::SysRoot`], so the same code runs on
//! the machine and against recorded trees (tests, replays).

pub mod bench;
pub mod boot;
pub mod cleanup;
pub mod doctor;
pub mod fan;
pub mod firmware;
pub mod gpu;
pub mod hive;
pub mod kernel;
pub mod keys;
pub mod knobs;
pub mod ledger;
pub mod load;
pub mod policy;
pub mod profile;
pub mod react;
pub mod snapshot;
pub mod sysroot;
pub mod system;
pub mod telemetry;
pub mod tune;
pub mod vfio;
pub mod windows;
pub mod winvm;

#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let unit = (1..UNITS.len()).rev().find(|u| bytes >> (10 * u) > 0);
    match unit {
        None => format!("{bytes} B"),
        Some(u) => {
            // tenths of the unit, rounded, in integers so every u64 is exact
            let tenths = (u128::from(bytes) * 10 + (1u128 << (10 * u - 1))) >> (10 * u);
            format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[u])
        }
    }
}

/// The machine daemon's D-Bus interface as committed (`dbus/org.legionctl.System1.xml`).
pub const INTERFACE_XML: &str = include_str!("../../../dbus/org.legionctl.System1.xml");

/// FNV-1a of `bytes`: a build's or a contract's identity, not a secret.
#[must_use]
pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// FNV-1a of the committed interface: the daemon's `Version` property, equal for a daemon and an operator built from
/// the same contract.
#[must_use]
pub fn interface_version() -> String {
    format!("{:016x}", fnv(INTERFACE_XML.as_bytes()))
}
