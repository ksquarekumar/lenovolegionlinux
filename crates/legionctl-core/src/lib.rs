//! Agent-side core of legionctl: agent settings, hooks, reminders, skills, setup, worktrees and the operator
//! database. `legionctl-operator` calls it; machine state and load live in legionctl-machine.

pub mod agents;
pub mod config;
pub mod db;
pub mod dossier;
pub mod hooks;
pub mod reminders;
pub mod setup;
pub mod skills;
pub mod worktree;
