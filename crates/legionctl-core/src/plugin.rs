//! The legionctl plugin of Claude Code: the hook events this build knows, where Claude Code installed the plugin, and
//! the doctor findings when the installed plugin is not of this build.

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

/// The marketplace (its name in its marketplace.json) and the plugin, as Claude Code's registry names them.
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
