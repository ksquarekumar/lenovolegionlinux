//! Shared core for the legion CLI, TUI and MCP server: machine state, load, worktrees and approved actions.
//! Every surface calls these functions, so humans and agents see the same data and the same guard rails.

pub mod actions;
pub mod agents;
pub mod load;
pub mod snapshot;
pub mod worktree;

use std::path::Path;

/// Read a small sysfs/procfs text file, trimmed. `None` when missing or unreadable.
pub(crate) fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}
