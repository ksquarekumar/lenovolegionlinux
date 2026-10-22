//! Claude Code and Codex configuration: find each settings file, parse it, and report whether it is valid and
//! which top-level sections it sets; edit one key (a diff of the changed lines only, a backup, then the write); and
//! count each agent's tokens per day and model from its own transcripts. Values are never returned beyond the lines
//! an edit changes, because these files can hold tokens.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigFile {
    pub agent: String,
    pub scope: String,
    pub path: PathBuf,
    pub exists: bool,
    /// None when the file is missing; otherwise whether it parses.
    pub valid: Option<bool>,
    pub error: Option<String>,
    /// Top-level keys or tables, e.g. permissions, hooks, `mcp_servers`.
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
#[must_use]
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
        let dir = crate::scratch::Scratch::new(
            std::env::temp_dir().join(format!("legionctl-agents-{}", std::process::id())),
        );
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.json"),
            r#"{"permissions": {}, "env": {"TOKEN": "secret"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join(".codex/config.toml"),
            "model = 'x'\n[mcp_servers.legionctl]\ncommand = 'legionctl'\n",
        )
        .unwrap();
        std::fs::write(dir.join(".mcp.json"), "{ not json").unwrap();

        let files = config_files(&dir, &dir);
        let user = &files[0];
        assert_eq!(
            (user.valid, user.sections.clone()),
            (
                Some(true),
                vec!["permissions".to_string(), "env".to_string()]
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
    }
}

/// Top-level keys of Claude Code's settings that legionctl knows; another key is written with a warning (the schema
/// grows with each release).
const CLAUDE_KEYS: &[&str] = &[
    "$schema",
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
    "cleanupPeriodDays",
    "disableAllHooks",
    "disabledMcpjsonServers",
    "enableAllProjectMcpServers",
    "enabledMcpjsonServers",
    "enabledPlugins",
    "env",
    "extraKnownMarketplaces",
    "forceLoginMethod",
    "hooks",
    "includeCoAuthoredBy",
    "model",
    "outputStyle",
    "permissions",
    "statusLine",
    "alwaysThinkingEnabled",
    "theme",
];

/// Top-level keys of Codex's `config.toml` that legionctl knows.
const CODEX_KEYS: &[&str] = &[
    "model",
    "model_provider",
    "model_reasoning_effort",
    "approval_policy",
    "sandbox_mode",
    "mcp_servers",
    "profiles",
    "profile",
    "model_providers",
    "shell_environment_policy",
    "tools",
    "history",
    "projects",
    "notify",
    "features",
    "tui",
    "sandbox_workspace_write",
    "permissions",
    "default_permissions",
];

/// What an edit did (or would do).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Edit {
    pub path: PathBuf,
    /// Only the changed lines, `-` before and `+` after.
    pub changes: Vec<String>,
    /// Where the previous file was copied; none for a dry run or a new file.
    pub backup: Option<PathBuf>,
    pub warnings: Vec<String>,
    pub dry_run: bool,
}

/// A text as a JSON value when it parses as one (`true`, `3`, `{"a": 1}`), else as a string.
#[must_use]
pub fn value_of(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.into()))
}

fn json_set(text: &str, key: &[&str], value: serde_json::Value) -> Result<String, String> {
    let mut root: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(text).map_err(|e| format!("the file is not valid JSON: {e}"))?
    };
    let mut at = &mut root;
    for part in &key[..key.len() - 1] {
        let object = at
            .as_object_mut()
            .ok_or(format!("{part}: a parent is not an object"))?;
        at = object
            .entry((*part).to_string())
            .or_insert_with(|| serde_json::json!({}));
    }
    let object = at
        .as_object_mut()
        .ok_or("the parent of the key is not an object")?;
    object.insert(key[key.len() - 1].to_string(), value);
    serde_json::to_string_pretty(&root)
        .map(|t| t + "\n")
        .map_err(|e| e.to_string())
}

fn toml_set(text: &str, key: &[&str], value: &serde_json::Value) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("the file is not valid TOML: {e}"))?;
    let item: toml_edit::Item = match value {
        serde_json::Value::Object(_) | serde_json::Value::Null => {
            return Err("give a TOML value: a string, number, boolean or list".into());
        }
        other => {
            let toml_text = format!("v = {other}");
            let parsed: toml_edit::DocumentMut = toml_text.parse().map_err(|e| format!("{e}"))?;
            parsed["v"].clone()
        }
    };
    let mut at = doc.as_item_mut();
    for part in &key[..key.len() - 1] {
        let table = at
            .as_table_like_mut()
            .ok_or(format!("{part}: a parent is not a table"))?;
        if table.get(part).is_none() {
            // `[mcp_servers.ctx]` alone, without an empty `[mcp_servers]` above it
            let mut parent = toml_edit::Table::new();
            parent.set_implicit(true);
            table.insert(part, toml_edit::Item::Table(parent));
        }
        at = table.get_mut(part).ok_or("no table")?;
    }
    let table = at
        .as_table_like_mut()
        .ok_or("the parent of the key is not a table")?;
    table.insert(key[key.len() - 1], item);
    Ok(doc.to_string())
}

/// Set `key` (dotted: `permissions.defaultMode`, `mcp_servers.ctx.command`) to `value` in an agent's settings file:
/// the changed lines, a backup of the file as it was (`<file>.legionctl-<time>`), then the write. Unless `dry_run`.
///
/// # Errors
/// The file cannot be read (a missing file is empty) or does not parse, a parent of the key is not an object or
/// table, or the write fails.
pub fn set(
    path: &Path,
    key: &str,
    value: serde_json::Value,
    dry_run: bool,
) -> anyhow::Result<Edit> {
    let parts: Vec<&str> = key.split('.').collect();
    anyhow::ensure!(
        parts.iter().all(|p| !p.is_empty()),
        "the key {key} has an empty part"
    );
    set_all(path, &[(parts, value)], dry_run)
}

/// [`set`] of several keys in one write, so with one backup; each key is given by its parts, so a part can hold a dot
/// (a path as a key of Codex's `permissions.<profile>.filesystem`).
///
/// # Errors
/// As [`set`].
pub fn set_all(
    path: &Path,
    entries: &[(Vec<&str>, serde_json::Value)],
    dry_run: bool,
) -> anyhow::Result<Edit> {
    let before = legionctl_machine::sysroot::read_file(path)?.unwrap_or_default();
    let toml = path.extension().is_some_and(|e| e == "toml");
    let known = if toml { CODEX_KEYS } else { CLAUDE_KEYS };
    let mut after = before.clone();
    let mut warnings = Vec::new();
    for (parts, value) in entries {
        after = if toml {
            toml_set(&after, parts, value)
        } else {
            json_set(&after, parts, value.clone())
        }
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        if !known.contains(&parts[0]) {
            warnings.push(format!(
                "{} is not a top-level key legionctl knows; check the spelling",
                parts[0]
            ));
        }
    }
    let changes = similar::TextDiff::from_lines(&before, &after)
        .iter_all_changes()
        .filter_map(|c| match c.tag() {
            similar::ChangeTag::Delete => Some(format!("- {}", c.value().trim_end())),
            similar::ChangeTag::Insert => Some(format!("+ {}", c.value().trim_end())),
            similar::ChangeTag::Equal => None,
        })
        .collect();
    let mut edit = Edit {
        path: path.into(),
        changes,
        backup: None,
        warnings,
        dry_run,
    };
    if dry_run || before == after {
        return Ok(edit);
    }
    if path.exists() {
        let backup = path.with_extension(format!(
            "{}.legionctl-{}",
            path.extension().and_then(|e| e.to_str()).unwrap_or("bak"),
            crate::db::now()
        ));
        std::fs::copy(path, &backup)?;
        edit.backup = Some(backup);
    }
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")))?;
    let tmp = path.with_extension("legionctl-tmp");
    std::fs::write(&tmp, &after)?;
    std::fs::rename(&tmp, path)?;
    Ok(edit)
}

/// Tokens of one agent on one day with one model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    pub agent: String,
    /// YYYY-MM-DD (UTC).
    pub day: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct UsageReport {
    pub rows: Vec<Usage>,
}

fn jsonl_files(dir: &Path, since: std::time::SystemTime, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            jsonl_files(&path, since, out);
        } else if path.extension().is_some_and(|e| e == "jsonl")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|m| m >= since)
        {
            out.push(path);
        }
    }
}

fn add(
    rows: &mut std::collections::BTreeMap<(String, String, String), Usage>,
    agent: &str,
    day: &str,
    model: &str,
    u: [u64; 4],
) {
    let row = rows
        .entry((agent.into(), day.into(), model.into()))
        .or_insert_with(|| Usage {
            agent: agent.into(),
            day: day.into(),
            model: model.into(),
            ..Usage::default()
        });
    row.input += u[0];
    row.output += u[1];
    row.cache_read += u[2];
    row.cache_write += u[3];
}

/// Tokens per agent, day and model in the transcripts changed in the last `days` days: Claude Code's
/// `<claude>/projects/**/*.jsonl` (each message once: its usage repeats on every content block) and Codex's
/// `<codex>/sessions/**/*.jsonl` (`token_count` events, each turn's `last_token_usage`). `claude` and `codex` are
/// `~/.claude` and `~/.codex`.
#[must_use]
pub fn usage(claude: &Path, codex: &Path, days: u64) -> UsageReport {
    let since = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    let mut rows = std::collections::BTreeMap::new();
    let mut files = Vec::new();
    jsonl_files(&claude.join("projects"), since, &mut files);
    let mut seen = std::collections::BTreeSet::new();
    for file in &files {
        for line in std::fs::read_to_string(file).unwrap_or_default().lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let message = &v["message"];
            let (Some(id), Some(model)) = (message["id"].as_str(), message["model"].as_str())
            else {
                continue;
            };
            if model.starts_with('<') || !seen.insert(id.to_string()) {
                continue;
            }
            let u = &message["usage"];
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            let day = v["timestamp"]
                .as_str()
                .unwrap_or("")
                .get(..10)
                .unwrap_or("");
            add(
                &mut rows,
                "claude",
                day,
                model,
                [
                    n("input_tokens"),
                    n("output_tokens"),
                    n("cache_read_input_tokens"),
                    n("cache_creation_input_tokens"),
                ],
            );
        }
    }
    let mut files = Vec::new();
    jsonl_files(&codex.join("sessions"), since, &mut files);
    files.sort();
    for file in &files {
        let mut model = String::from("?");
        for line in std::fs::read_to_string(file).unwrap_or_default().lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let payload = &v["payload"];
            if let Some(m) = payload["model"].as_str() {
                model = m.to_string();
            }
            if payload["type"] != "token_count" {
                continue;
            }
            let u = &payload["info"]["last_token_usage"];
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            let day = v["timestamp"]
                .as_str()
                .unwrap_or("")
                .get(..10)
                .unwrap_or("");
            add(
                &mut rows,
                "codex",
                day,
                &model,
                [
                    n("input_tokens"),
                    n("output_tokens"),
                    n("cached_input_tokens"),
                    0,
                ],
            );
        }
    }
    UsageReport {
        rows: rows.into_values().collect(),
    }
}
