//! Shared core for the legionctl CLI, TUI and MCP server: agents, hooks, reminders, worktrees and approved actions.
//! Machine state and load live in legionctl-machine.
//! Every surface calls these functions, so humans and agents see the same data and the same guard rails.

pub mod actions;
pub mod agents;
pub mod db;
pub mod hooks;
pub mod reminders;
pub mod setup;
pub mod skills;
pub mod worktree;
