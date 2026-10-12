//! Shared core for the legion CLI, TUI and MCP server: machine state, load, worktrees and approved actions.
//! Every surface calls these functions, so humans and agents see the same data and the same guard rails.

pub mod actions;
pub mod agents;
pub mod db;
pub mod hooks;
pub mod load;
pub mod reminders;
pub mod setup;
pub mod skills;
pub mod snapshot;
pub mod worktree;

use std::path::Path;

/// Read a small sysfs/procfs text file, trimmed. `None` when missing or unreadable.
pub(crate) fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

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
