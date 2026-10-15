//! Agent-side core of legionctl: agent settings, hooks, reminders, skills, setup, worktrees and the operator
//! database. `legionctl-operator` calls it; machine state and load live in legionctl-machine.

pub mod agents;
pub mod archetypes;
pub mod cache;
pub mod codegraph;
pub mod codetests;
pub mod config;
pub mod db;
pub mod dossier;
pub mod error;
pub mod folder;
pub mod gates;
pub mod graph;
pub mod home;
pub mod hooks;
pub mod images;
pub mod migrate;
pub mod packages;
pub mod planning;
pub mod qmp;
pub mod reminders;
pub mod session;
pub mod setup;
pub mod skills;
pub mod thread;
pub mod views;
pub mod workspace;

/// The test folder guard that the integration tests use too.
#[cfg(test)]
#[path = "../../legionctl/tests/common/scratch.rs"]
mod scratch;
