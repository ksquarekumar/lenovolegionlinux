//! Agent-side core of legionctl: agent settings, hooks, reminders, skills, setup, worktrees and the operator
//! database. `legionctl-operator` calls it; machine state and load live in legionctl-machine.

pub mod agents;
pub mod cache;
pub mod codegraph;
pub mod config;
pub mod db;
pub mod dossier;
pub mod error;
pub mod folder;
pub mod gates;
pub mod graph;
pub mod hooks;
pub mod images;
pub mod packages;
pub mod qmp;
pub mod reminders;
pub mod session;
pub mod setup;
pub mod skills;
pub mod workspace;
