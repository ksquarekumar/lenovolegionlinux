//! Machine control for this Lenovo Legion. Every path goes through a [`sysroot::SysRoot`], so the same code runs on
//! the machine and against recorded trees (tests, replays).

pub mod cleanup;
pub mod doctor;
pub mod firmware;
pub mod gpu;
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
