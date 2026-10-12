//! Agent setup: make ctx (https://ctx.rs) and legion available to Claude Code and Codex out of the box.
//!
//! - ctx: its bundled skill and MCP server for both agents (`ctx integrations install skill|mcp`).
//! - Codex: legion's MCP server in ~/.codex/config.toml, and the plugin's skills linked into ~/.codex/skills.
//! - Claude Code gets legion's MCP server, hooks and skills from the plugin itself.
//!
//! `check()` only reports; `apply()` changes user-level files and returns what it did.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use schemars::JsonSchema;
use serde::Serialize;

/// ctx names for the agents legion sets up.
pub const AGENTS: [&str; 2] = ["claude-code", "codex"];
const PLUGIN_SKILLS: &str = "/usr/share/legion-control/agent-plugin/skills";

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Item {
    /// e.g. "ctx skill", "ctx mcp", "legion mcp", "legion skill ste100"
    pub what: String,
    pub agent: String,
    /// "current" when in place; otherwise what is wrong (missing, outdated, invalid_config, ...)
    pub status: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Report {
    pub ctx_version: Option<String>,
    pub items: Vec<Item>,
}

impl Report {
    pub fn needs_attention(&self) -> Vec<&Item> {
        self.items
            .iter()
            .filter(|i| i.status != "current")
            .collect()
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// ctx is usually not on the PATH of non-interactive shells (hooks, MCP servers); look in its install dir too.
fn ctx_bin() -> Option<PathBuf> {
    let local = home().join(".local/bin/ctx");
    if local.is_file() {
        return Some(local);
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join("ctx"))
        .find(|p| p.is_file())
}

fn ctx(args: &[&str]) -> Result<String> {
    let bin = ctx_bin().context("ctx is not installed; run `legion setup --install-ctx`")?;
    let out = Command::new(bin)
        .args(args)
        .output()
        .context("running ctx")?;
    ensure!(
        out.status.success(),
        "ctx {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `ctx integrations status <kind> --format json` into items.
pub fn parse_ctx_status(kind: &str, json: &str) -> Vec<Item> {
    let value: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Item {
            what: format!("ctx {kind}"),
            agent: r["agent"].as_str().unwrap_or("?").into(),
            status: r["status"].as_str().unwrap_or("unknown").into(),
            path: r["path"].as_str().unwrap_or("").into(),
        })
        .collect()
}

fn ctx_items() -> Vec<Item> {
    let mut items = Vec::new();
    for kind in ["skill", "mcp"] {
        let mut args = vec!["integrations", "status", kind, "--format", "json"];
        for agent in AGENTS {
            args.extend(["--agent", agent]);
        }
        match ctx(&args) {
            Ok(json) => items.extend(parse_ctx_status(kind, &json)),
            Err(e) => items.push(Item {
                what: format!("ctx {kind}"),
                agent: "all".into(),
                status: e.to_string(),
                path: String::new(),
            }),
        }
    }
    items
}

fn codex_config() -> PathBuf {
    home().join(".codex/config.toml")
}

pub fn codex_has_legion(text: &str) -> Result<bool> {
    let table: toml::Table = text
        .parse()
        .context("~/.codex/config.toml is not valid TOML; fix it before setup")?;
    Ok(table
        .get("mcp_servers")
        .and_then(|s| s.get("legion"))
        .is_some())
}

pub fn plugin_skills() -> PathBuf {
    std::env::var_os("LEGION_PLUGIN_SKILLS")
        .map_or_else(|| PathBuf::from(PLUGIN_SKILLS), PathBuf::from)
}

fn skill_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(plugin_skills())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn codex_items() -> Vec<Item> {
    let config = codex_config();
    let status = match std::fs::read_to_string(&config) {
        Ok(text) => match codex_has_legion(&text) {
            Ok(true) => "current".into(),
            Ok(false) => "missing".into(),
            Err(e) => e.to_string(),
        },
        Err(_) => "missing".into(),
    };
    let mut items = vec![Item {
        what: "legion mcp".into(),
        agent: "codex".into(),
        status,
        path: config.display().to_string(),
    }];
    for name in skill_names() {
        let link = home().join(".codex/skills").join(format!("legion-{name}"));
        let status = if std::fs::read_link(&link).is_ok_and(|t| t == plugin_skills().join(&name)) {
            "current"
        } else {
            "missing"
        };
        items.push(Item {
            what: format!("legion skill {name}"),
            agent: "codex".into(),
            status: status.into(),
            path: link.display().to_string(),
        });
    }
    items
}

pub fn check() -> Report {
    let ctx_version = ctx(&["--version"]).ok().map(|v| v.trim().to_string());
    let mut items = if ctx_version.is_some() {
        ctx_items()
    } else {
        vec![Item {
            what: "ctx".into(),
            agent: "all".into(),
            status: "not installed".into(),
            path: String::new(),
        }]
    };
    items.extend(codex_items());
    Report { ctx_version, items }
}

/// Install ctx with its signed installer (user-level, ~/.local/bin), analytics off, shell rc files untouched.
fn install_ctx() -> Result<()> {
    let script = "curl -fsSL https://ctx.rs/install | CTX_ANALYTICS_ENABLED=false CTX_INSTALL_NO_MODIFY_PATH=1 sh";
    let out = Command::new("sh")
        .args(["-c", script])
        .output()
        .context("running the ctx installer")?;
    ensure!(
        out.status.success(),
        "ctx install failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Bring every item to "current". Returns one line per change.
pub fn apply(install_missing_ctx: bool) -> Result<Vec<String>> {
    let mut done = Vec::new();
    if ctx_bin().is_none() {
        ensure!(
            install_missing_ctx,
            "ctx is not installed; rerun with --install-ctx to install it"
        );
        install_ctx()?;
        done.push("installed ctx".to_string());
    }
    for kind in ["skill", "mcp"] {
        let mut args = vec!["integrations", "install", kind, "--quiet"];
        for agent in AGENTS {
            args.extend(["--agent", agent]);
        }
        ctx(&args)?;
        done.push(format!("ctx {kind}: installed for {}", AGENTS.join(", ")));
    }

    let config = codex_config();
    let text = std::fs::read_to_string(&config).unwrap_or_default();
    if !codex_has_legion(&text)? {
        if config.exists() {
            std::fs::copy(&config, config.with_extension("toml.legion-bak"))
                .context("backing up codex config")?;
        }
        std::fs::create_dir_all(config.parent().context("codex config has no parent")?)?;
        let block = "\n# added by `legion setup`\n[mcp_servers.legion]\ncommand = \"legion\"\nargs = [\"mcp\"]\n";
        let updated = format!("{}{block}", text.trim_end_matches('\n'));
        ensure!(
            codex_has_legion(&updated)?,
            "refusing to write: result would not contain [mcp_servers.legion]"
        );
        std::fs::write(&config, updated)?;
        done.push(format!(
            "legion mcp: added to {} (backup *.legion-bak)",
            config.display()
        ));
    }

    let skills_dir = home().join(".codex/skills");
    // skills that were renamed or removed leave dangling legion-* links behind; drop only those
    for entry in std::fs::read_dir(&skills_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with("legion-")
            && path.is_symlink()
            && !path.exists()
        {
            std::fs::remove_file(&path)?;
            done.push(format!("removed stale link {}", path.display()));
        }
    }
    for name in skill_names() {
        let target = plugin_skills().join(&name);
        let link = skills_dir.join(format!("legion-{name}"));
        if std::fs::read_link(&link).is_ok_and(|t| t == target) {
            continue;
        }
        std::fs::create_dir_all(&skills_dir)?;
        if link.is_symlink() {
            std::fs::remove_file(&link)?;
        }
        ensure!(
            !link.exists(),
            "{} exists and is not a legion link; not replacing it",
            link.display()
        );
        std::os::unix::fs::symlink(&target, &link)?;
        done.push(format!("legion skill {name}: linked for codex"));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ctx_status_json() {
        let json = r#"{"integration":"mcp","results":[
            {"agent":"claude-code","status":"current","path":"/h/.claude.json"},
            {"agent":"codex","status":"missing","path":"/h/.codex/config.toml"}]}"#;
        let items = parse_ctx_status("mcp", json);
        assert_eq!(items.len(), 2);
        assert_eq!(
            (items[1].agent.as_str(), items[1].status.as_str()),
            ("codex", "missing")
        );
        assert!(parse_ctx_status("mcp", "not json").is_empty());
    }

    #[test]
    fn detects_legion_in_codex_config() {
        assert!(codex_has_legion("[mcp_servers.legion]\ncommand = \"legion\"\n").unwrap());
        assert!(
            !codex_has_legion("model = \"x\"\n[mcp_servers.ctx]\ncommand = \"ctx\"\n").unwrap()
        );
        assert!(
            codex_has_legion("model = [\n").is_err(),
            "invalid TOML must stop setup"
        );
    }
}
