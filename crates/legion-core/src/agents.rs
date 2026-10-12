//! Claude Code and Codex configuration: find each settings file, parse it, and report whether it is valid and
//! which top-level sections it sets. Values are never returned, because these files can hold tokens.
//! Editing (schema-checked, previewed as a diff) is designed in docs/design/12-operator.md.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConfigFile {
    pub agent: String,
    pub scope: String,
    pub path: PathBuf,
    pub exists: bool,
    /// None when the file is missing; otherwise whether it parses.
    pub valid: Option<bool>,
    pub error: Option<String>,
    /// Top-level keys or tables, e.g. permissions, hooks, mcp_servers.
    pub sections: Vec<String>,
}

fn inspect(agent: &str, scope: &str, path: PathBuf) -> ConfigFile {
    let mut file = ConfigFile {
        agent: agent.into(),
        scope: scope.into(),
        exists: path.is_file(),
        path,
        valid: None,
        error: None,
        sections: vec![],
    };
    let Ok(text) = std::fs::read_to_string(&file.path) else {
        return file;
    };
    let parsed: Result<Vec<String>, String> = if file.path.extension().is_some_and(|e| e == "toml")
    {
        text.parse::<toml::Table>()
            .map(|t| t.keys().cloned().collect())
            .map_err(|e| e.to_string())
    } else {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text)
            .map(|m| m.keys().cloned().collect())
            .map_err(|e| e.to_string())
    };
    match parsed {
        Ok(sections) => (file.valid, file.sections) = (Some(true), sections),
        Err(error) => (file.valid, file.error) = (Some(false), Some(error)),
    }
    file
}

/// User-level files plus the project-level ones for `project` (usually the current directory).
pub fn config_files(home: &Path, project: &Path) -> Vec<ConfigFile> {
    vec![
        inspect("claude", "user", home.join(".claude/settings.json")),
        inspect("claude", "project", project.join(".claude/settings.json")),
        inspect(
            "claude",
            "project-local",
            project.join(".claude/settings.local.json"),
        ),
        inspect("claude", "project-mcp", project.join(".mcp.json")),
        inspect("codex", "user", home.join(".codex/config.toml")),
        inspect("codex", "project", project.join(".codex/config.toml")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_validity_and_sections_without_values() {
        let dir = std::env::temp_dir().join(format!("legion-agents-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.json"),
            r#"{"permissions": {}, "env": {"TOKEN": "secret"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join(".codex/config.toml"),
            "model = 'x'\n[mcp_servers.legion]\ncommand = 'legion'\n",
        )
        .unwrap();
        std::fs::write(dir.join(".mcp.json"), "{ not json").unwrap();

        let files = config_files(&dir, &dir);
        let user = &files[0];
        assert_eq!(
            (user.valid, user.sections.clone()),
            (
                Some(true),
                vec!["env".to_string(), "permissions".to_string()]
            )
        );
        assert!(
            !serde_json::to_string(user).unwrap().contains("secret"),
            "values must never be exposed"
        );
        let codex = files
            .iter()
            .find(|f| f.agent == "codex" && f.scope == "user")
            .unwrap();
        assert_eq!(
            codex.sections,
            vec!["mcp_servers".to_string(), "model".to_string()]
        );
        let mcp = files.iter().find(|f| f.scope == "project-mcp").unwrap();
        assert_eq!(mcp.valid, Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
