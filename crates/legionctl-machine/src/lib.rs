//! Machine control for this Lenovo Legion. Every path goes through a [`sysroot::SysRoot`], so the same code runs on
//! the machine and against recorded trees (tests, replays).

pub mod policy;
pub mod profile;
pub mod sysroot;
