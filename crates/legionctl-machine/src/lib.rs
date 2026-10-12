//! Machine control for this Lenovo Legion. Every path goes through a [`sysroot::SysRoot`], so the same code runs on
//! the machine and against recorded trees (tests, replays).

pub mod cleanup;
pub mod doctor;
pub mod firmware;
pub mod gpu;
pub mod kernel;
pub mod load;
pub mod policy;
pub mod profile;
pub mod react;
pub mod snapshot;
pub mod sysroot;
pub mod system;

pub fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}
