//! Which legion skills exist, where each agent has them installed, and whether their reminder is on.

use std::path::{Path, PathBuf};

use anyhow::Result;
use schemars::JsonSchema;
use serde::Serialize;

use crate::reminders;
use crate::setup::plugin_skills;

const CLAUDE_PLUGIN_KEY: &str = "legion@lenovolegionlinux";

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SkillInfo {
    /// Skill name; Claude Code loads it as `legion:<name>`.
    pub name: String,
    pub description: String,
    /// Path inside the installed Claude Code plugin, when the plugin is installed.
    pub claude: Option<PathBuf>,
    /// Codex link (~/.codex/skills/legion-<name>) and whether it resolves.
    pub codex: PathBuf,
    pub codex_linked: bool,
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

/// Install path of the legion plugin from Claude Code's registry, if installed.
pub fn claude_plugin_dir(home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(home.join(".claude/plugins/installed_plugins.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let plugins = value.get("plugins").unwrap_or(&value);
    let entry = &plugins[CLAUDE_PLUGIN_KEY];
    let first = entry.as_array().and_then(|a| a.first()).unwrap_or(entry);
    first["installPath"].as_str().map(PathBuf::from)
}

/// Reminder ids follow the skill name without its "-style" suffix (ste100-style -> ste100).
fn reminder_id(skill: &str) -> String {
    skill.trim_end_matches("-style").into()
}

pub fn inventory(home: &Path, reminders: &reminders::Store) -> Result<Vec<SkillInfo>> {
    let claude = claude_plugin_dir(home);
    let known = reminders.list()?;
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
        let codex = home.join(".codex/skills").join(format!("legion-{name}"));
        let reminder = known.iter().find(|r| r.id == reminder_id(&name));
        out.push(SkillInfo {
            description: description(&skill_md),
            claude: claude
                .as_ref()
                .map(|d| d.join("skills").join(&name))
                .filter(|p| p.join("SKILL.md").is_file()),
            codex_linked: codex.join("SKILL.md").is_file(),
            codex,
            reminder: reminder.map(|r| r.id.clone()),
            reminder_on: reminder.map(|r| r.enabled),
            name,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_claude_plugin_registry() {
        let home = std::env::temp_dir().join(format!("legion-skills-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            r#"{"version": 2, "plugins": {"legion@lenovolegionlinux": [{"scope": "user", "installPath": "/x/legion/0.3.0"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            claude_plugin_dir(&home),
            Some(PathBuf::from("/x/legion/0.3.0"))
        );
        assert_eq!(reminder_id("ste100-style"), "ste100");
        let _ = std::fs::remove_dir_all(&home);
    }
}
