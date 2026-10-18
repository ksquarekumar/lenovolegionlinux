//! The legionctl plugin of Claude Code and of Codex: the hook events this build knows, where each harness installed the
//! plugin, and the findings when the installed plugin is not of this build. Codex reads the same marketplace
//! (`.claude-plugin/marketplace.json`) and the plugin's own manifest (`.codex-plugin/plugin.json`).

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::setup::plugin_root;

/// The events of `legionctl hook <event>` in this build.
pub const HOOK_EVENTS: [&str; 10] = [
    "session-start",
    "prompt",
    "pre-tool",
    "subagent-start",
    "pre-compact",
    "post-tool",
    "stop",
    "session-end",
    "teammate-idle",
    "task-completed",
];

/// The marketplace (its name in its marketplace.json) and the plugin, as Claude Code's and Codex's registries name them.
pub(crate) const MARKETPLACE: &str = "lenovolegionlinux";
pub(crate) const PLUGIN_KEY: &str = "legionctl@lenovolegionlinux";

/// The package's marketplace: the folder of its marketplace.json, with the plugin in ./agent-plugin.
pub(crate) fn package_marketplace() -> PathBuf {
    let plugin = plugin_root();
    plugin.parent().map_or(plugin.clone(), Path::to_path_buf)
}

/// The folder of Claude Code's marketplace of legionctl, when it is a folder marketplace.
pub(crate) fn marketplace_source(home: &Path) -> Option<PathBuf> {
    json(&home.join(".claude/plugins/known_marketplaces.json"))
        .and_then(|m| m[MARKETPLACE]["source"]["path"].as_str().map(PathBuf::from))
}

fn json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Install path of the legionctl plugin from Claude Code's registry, if installed.
pub fn claude_plugin_dir(home: &Path) -> Option<PathBuf> {
    let value = json(&home.join(".claude/plugins/installed_plugins.json"))?;
    let plugins = value.get("plugins").unwrap_or(&value);
    let entry = &plugins[PLUGIN_KEY];
    let first = entry.as_array().and_then(|a| a.first()).unwrap_or(entry);
    first["installPath"].as_str().map(PathBuf::from)
}

/// The legionctl plugin's folder in the folder marketplace `market`, from its marketplace.json.
fn folder_plugin(market: &Path) -> Option<PathBuf> {
    let manifest = json(&market.join(".claude-plugin/marketplace.json"))?;
    let plugin = manifest["plugins"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == "legionctl")?;
    Some(market.join(plugin["source"].as_str()?.trim_start_matches("./")))
}

/// The doctor findings of the installed plugin: its marketplace is not the package's folder, or its hooks name events
/// that this build does not know (a hook with an unknown event fails at each call). None when it is not installed.
#[must_use]
pub fn findings(home: &Path) -> Vec<String> {
    let Some(dir) = claude_plugin_dir(home) else {
        return Vec::new();
    };
    let package = package_marketplace();
    let package = package.as_path();
    let fix = "legionctl setup apply installs it from the package";
    let mut out = Vec::new();
    let source = marketplace_source(home);
    if source.as_deref() != Some(package) {
        out.push(format!(
            "the Claude Code marketplace {MARKETPLACE} is {}, not the package folder {}: its plugin and this legionctl \
             can be of different builds; {fix}",
            source
                .as_ref()
                .map_or_else(|| "not a folder".into(), |s| s.display().to_string()),
            package.display()
        ));
    }
    // Claude Code reads the plugin of a folder marketplace from that folder at each session start, not from its
    // install path (a copy made at the install)
    let dir = source.as_deref().and_then(folder_plugin).unwrap_or(dir);
    let hooks = dir.join("hooks/hooks.json");
    let Some(hooks_json) = json(&hooks) else {
        out.push(format!(
            "the hooks of the legionctl plugin ({}) cannot be read as JSON; {fix}",
            hooks.display()
        ));
        return out;
    };
    let unknown: Vec<&str> = hooks_json["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|events| events.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group["hooks"].as_array())
        .flatten()
        .filter_map(|hook| hook["command"].as_str()?.strip_prefix("legionctl hook "))
        .filter(|event| !HOOK_EVENTS.contains(event))
        .collect();
    if !unknown.is_empty() {
        out.push(format!(
            "the legionctl plugin ({}) has hooks for events that this legionctl does not know: {}; each of these hooks \
             fails; {fix}",
            dir.display(),
            unknown.join(", ")
        ));
    }
    out
}

/// The folder of the legionctl plugin that Codex installed (its copy in ~/.codex/plugins/cache, one version), when
/// Codex has it on.
#[must_use]
pub fn codex_plugin_dir(home: &Path, config: &toml::Table) -> Option<PathBuf> {
    let on = config
        .get("plugins")
        .and_then(|p| p.get(PLUGIN_KEY))
        .and_then(|p| p.get("enabled"))
        .and_then(toml::Value::as_bool)?;
    let versions = home
        .join(".codex/plugins/cache")
        .join(MARKETPLACE)
        .join("legionctl");
    let mut dirs = std::fs::read_dir(versions)
        .ok()?
        .flatten()
        .map(|e| e.path());
    let dir = dirs.next()?;
    (on && dirs.next().is_none()).then_some(dir)
}

/// The Codex plugin's row: `current` when Codex has it from the package's folder and its copy has every file of the
/// package's plugin as it is; else `missing` or `not of this build`.
#[must_use]
pub fn codex_plugin_status(home: &Path, config: &toml::Table) -> String {
    let market = package_marketplace();
    let from_package = config
        .get("marketplaces")
        .and_then(|m| m.get(MARKETPLACE))
        .and_then(|m| m.get("source"))
        .and_then(toml::Value::as_str)
        .is_some_and(|s| Path::new(s) == market);
    match codex_plugin_dir(home, config) {
        Some(dir) if from_package && same_files(&plugin_root(), &dir) => "current",
        Some(_) => "not of this build",
        None => "missing",
    }
    .into()
}

/// Whether each file under `package` is in `copy` with the same bytes (Codex adds its own files to its copy).
fn same_files(package: &Path, copy: &Path) -> bool {
    std::fs::read_dir(package)
        .into_iter()
        .flatten()
        .flatten()
        .all(|e| {
            let (from, to) = (e.path(), copy.join(e.file_name()));
            if from.is_dir() {
                same_files(&from, &to)
            } else {
                std::fs::read(&from).ok() == std::fs::read(&to).ok()
            }
        })
}

/// The Codex hooks' state until the user trusts them in Codex: no setup step gives that trust.
pub const CODEX_HOOKS_UNTRUSTED: &str =
    "not trusted yet: start Codex and choose to trust the legionctl hooks";

/// The Codex hooks' row, from Codex itself (its app server's `hooks/list`): `current` when Codex trusts each hook of
/// the legionctl plugin, else the step for the user (Codex asks at its next start). Codex runs no hook it does not
/// trust, and the trust is the user's to give.
#[must_use]
pub fn codex_hooks_status(codex: &Path, home: &Path) -> String {
    match codex_hook_trust(codex, home) {
        Ok(states) if states.is_empty() => "missing".into(),
        Ok(states) if states.iter().all(|s| s == "trusted") => "current".into(),
        Ok(_) => CODEX_HOOKS_UNTRUSTED.into(),
        Err(e) => format!("{e:#}"),
    }
}

/// The trust state of each hook of the legionctl plugin, as `codex app-server` lists them.
fn codex_hook_trust(codex: &Path, home: &Path) -> anyhow::Result<Vec<String>> {
    use std::io::{BufRead, Write};
    let mut child = std::process::Command::new(codex)
        .arg("app-server")
        .current_dir(home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("running codex app-server: {e}"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("codex app-server has no input"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("codex app-server has no output"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(output)
            .lines()
            .map_while(Result::ok)
        {
            if let Ok(message) = serde_json::from_str::<Value>(&line)
                && message["id"] == 2
            {
                let _ = tx.send(message);
                return;
            }
        }
    });
    for message in [
        serde_json::json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "legionctl", "version": env!("CARGO_PKG_VERSION")}}}),
        serde_json::json!({"method": "initialized"}),
        serde_json::json!({"id": 2, "method": "hooks/list", "params": {"cwds": [home]}}),
    ] {
        writeln!(input, "{message}")?;
    }
    let answer = rx.recv_timeout(std::time::Duration::from_secs(20));
    let _ = child.kill();
    let _ = child.wait();
    let answer = answer
        .map_err(|_| anyhow::anyhow!("codex app-server gave no list of its hooks in 20 s"))?;
    anyhow::ensure!(
        answer["error"].is_null(),
        "codex app-server: {}",
        answer["error"]["message"]
    );
    Ok(answer["result"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d["hooks"].as_array())
        .flatten()
        .filter(|h| h["pluginId"] == PLUGIN_KEY)
        .filter_map(|h| h["trustStatus"].as_str().map(String::from))
        .collect())
}
