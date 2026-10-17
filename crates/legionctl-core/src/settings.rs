//! The settings registry (DESIGN-20, DESIGN-14): every config key with its help, its value's schema and its
//! built-in default, read from the `Config` type's schema and the shipped defaults, so that the list cannot drift
//! from the code. Each module is a top-level section of the config.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One setting.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Setting {
    /// Dotted key; a map is one setting (its entries are the user's).
    pub key: String,
    /// The module that owns it: the key's first part.
    pub module: String,
    /// What it does, for the user (`config/settings-help.jsonc`; the code's doc comments are for builders).
    pub help: String,
    /// The JSON schema of its value.
    pub kind: Value,
    /// The built-in default (null: none).
    pub default: Value,
    /// Only the built-in defaults and /etc/legionctl set it.
    pub locked: bool,
    /// A repository's file sets it only after the user allowed that file once.
    pub approve: bool,
}

/// Every setting, in the order of the config type.
///
/// # Errors
/// The built-in defaults do not parse.
pub fn registry() -> Result<Vec<Setting>> {
    let schema = serde_json::to_value(schemars::schema_for!(crate::config::Config))?;
    let defaults = crate::config::defaults()?;
    let meta: crate::config::Meta = serde_json::from_value(defaults["config"].clone())?;
    let help = help()?;
    let mut out = Vec::new();
    walk(&schema, &schema, "", &defaults, &meta, &mut out);
    for setting in &mut out {
        setting.help = help.get(&setting.key).cloned().unwrap_or_default();
    }
    Ok(out)
}

/// The user's help of each setting, by key (`config/settings-help.jsonc`).
///
/// # Errors
/// The file does not parse.
pub fn help() -> Result<std::collections::BTreeMap<String, String>> {
    Ok(serde_json::from_reader(json_comments::StripComments::new(
        include_str!("../../../config/settings-help.jsonc").as_bytes(),
    ))?)
}

/// The schema behind a `$ref` to `#/$defs/<name>`, else `node`.
fn deref<'a>(node: &'a Value, root: &'a Value) -> &'a Value {
    node.get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
        .and_then(|name| root["$defs"].get(name))
        .map_or(node, |target| deref(target, root))
}

fn walk(
    node: &Value,
    root: &Value,
    key: &str,
    default: &Value,
    meta: &crate::config::Meta,
    out: &mut Vec<Setting>,
) {
    let target = deref(node, root);
    if let Some(props) = target["properties"].as_object() {
        for (name, sub) in props {
            let sub_key = if key.is_empty() {
                name.clone()
            } else {
                format!("{key}.{name}")
            };
            walk(sub, root, &sub_key, &default[name.as_str()], meta, out);
        }
        return;
    }
    out.push(Setting {
        key: key.to_string(),
        module: key.split('.').next().unwrap_or_default().to_string(),
        help: String::new(),
        kind: target.clone(),
        default: default.clone(),
        locked: crate::config::locked(&meta.locked, key),
        approve: crate::config::locked(&meta.approve, key),
    });
}
