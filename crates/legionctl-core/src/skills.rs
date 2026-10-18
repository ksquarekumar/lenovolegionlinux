//! Which legionctl skills exist, where each agent has them installed, and whether their reminder is on.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Resolved;
use crate::plugin::{claude_plugin_dir, codex_plugin_dir};
use crate::reminders;
use crate::setup::plugin_skills;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillInfo {
    /// Skill name; Claude Code and Codex load it as `legionctl:<name>`.
    pub name: String,
    pub description: String,
    /// Path inside the installed Claude Code plugin, when the plugin is installed.
    pub claude: Option<PathBuf>,
    /// Path inside the installed Codex plugin, when the plugin is installed.
    pub codex: Option<PathBuf>,
    /// Reminder that keeps the skill in force, and its state.
    pub reminder: Option<String>,
    pub reminder_on: Option<bool>,
}

fn description(skill_md: &Path) -> String {
    std::fs::read_to_string(skill_md)
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("description:").map(|d| d.trim().to_string()))
        })
        .unwrap_or_default()
}

/// Reminder ids follow the skill name without its "-style" suffix (ste100-style -> ste100).
fn reminder_id(skill: &str) -> String {
    skill.trim_end_matches("-style").into()
}

/// The skill `name` in an installed plugin's folder, when it is there.
fn in_plugin(plugin: Option<&Path>, name: &str) -> Option<PathBuf> {
    plugin
        .map(|d| d.join("skills").join(name))
        .filter(|p| p.join("SKILL.md").is_file())
}

#[must_use]
pub fn inventory(home: &Path, config: &Resolved) -> Vec<SkillInfo> {
    let claude = claude_plugin_dir(home);
    let codex = std::fs::read_to_string(home.join(".codex/config.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| codex_plugin_dir(home, &t));
    let known = reminders::list(config);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(plugin_skills())
        .into_iter()
        .flatten()
        .flatten()
    {
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let reminder = known.iter().find(|r| r.id == reminder_id(&name));
        out.push(SkillInfo {
            description: description(&skill_md),
            claude: in_plugin(claude.as_deref(), &name),
            codex: in_plugin(codex.as_deref(), &name),
            reminder: reminder.map(|r| r.id.clone()),
            reminder_on: reminder.map(|r| r.enabled),
            name,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}
