//! The settings registry (DESIGN-20, DESIGN-14): every config key with its value's schema and built-in default (from
//! the `Config` type and the shipped defaults) and its declaration in `config/settings.jsonc` (label, help, scope,
//! class, restart, what it affects, its deep check), so the list cannot drift from the code: a key without a
//! declaration, or a declaration without a key, is an error. Each module is a top-level section of the config.
//!
//! The one write path of a setting is [`write`] (the user and project layers) and the operator's route to the system
//! service for the computer layer; both edit the layer's `managed.jsonc` with
//! [`legionctl_machine::settings_file::edit`] and keep each change in the history (`settings_history`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Resolved;
use crate::db::Db;

/// A layer that a setting may be set in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// `/etc/legionctl`, through the system service (the system asks for an administrator's password).
    Computer,
    /// `~/.config/legionctl`.
    User,
    /// The repository's `.legionctl`.
    Project,
}

impl Scope {
    /// The scope of a config layer's name (`config.precedence`); none for the built-in defaults.
    #[must_use]
    pub fn of_layer(layer: &str) -> Option<Scope> {
        match layer {
            "system" => Some(Scope::Computer),
            "user" => Some(Scope::User),
            "repo" => Some(Scope::Project),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Scope::Computer => "computer",
            Scope::User => "user",
            Scope::Project => "project",
        }
    }

    fn words(self) -> &'static str {
        match self {
            Scope::Computer => "this computer",
            Scope::User => "you",
            Scope::Project => "a project",
        }
    }
}

/// How a change is approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// No question.
    Safe,
    /// An agent's change asks you; a project file sets it only after you allowed that file.
    Powerful,
    /// As powerful, and it changes how this computer runs.
    Machine,
}

/// What must start again before a change applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Restart {
    None,
    Operator,
    Login,
    Reboot,
}

/// What legionctl checks when it uses a value (DESIGN-20 rule 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Check {
    /// The file or folder is there.
    Path,
    /// A folder that legionctl can write, or make.
    Folder,
    /// A port that is free on this computer (checked when it changes).
    Port,
}

/// This computer's own scope or class for settings (`config.overrides` of the defaults and /etc/legionctl): a key, a
/// key prefix or a pattern with `*` for one key part (`agents.configs.*.account`) -> what changes for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Override {
    /// The layers that may set it (only computer: locked for everybody else; no project: a project never sets it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Vec<Scope>>,
    /// How a change is approved (powerful: a project file sets it only after you allow that file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
}

/// The scope and class of `key`: its setting's declaration with the most specific override that holds it (the
/// pattern is the key or above it); a pattern below a setting is its own entry (none: no setting holds `key`).
#[must_use]
pub fn declared(key: &str, overrides: &BTreeMap<String, Override>) -> Option<(Vec<Scope>, Class)> {
    let setting = setting_of(key)?;
    let (mut scope, mut class) = (setting.scope.clone(), setting.class);
    let mut matching: Vec<(&String, &Override)> =
        overrides.iter().filter(|(p, _)| holds(p, key)).collect();
    matching.sort_by_key(|(p, _)| p.split('.').count());
    for (_, o) in matching {
        scope = o.scope.clone().unwrap_or(scope);
        class = o.class.unwrap_or(class);
    }
    Some((scope, class))
}

/// A setting's declaration in `config/settings.jsonc`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declared {
    label: String,
    help: String,
    scope: Vec<Scope>,
    class: Class,
    restart: Restart,
    affects: Vec<String>,
    #[serde(default)]
    check: Option<Check>,
    #[serde(default)]
    union: bool,
}

/// One setting.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Setting {
    /// Dotted key; a map is one setting (its entries are the user's).
    pub key: String,
    /// The module that owns it: the key's first part.
    pub module: String,
    /// Its name for the person.
    pub label: String,
    /// What it does, for the person.
    pub help: String,
    /// The JSON schema of its value, every part inline; `x-rules`: a list of patterns with a reason each.
    pub kind: Value,
    /// The built-in default (null: none).
    pub default: Value,
    /// The layers that may set it.
    pub scope: Vec<Scope>,
    pub class: Class,
    pub restart: Restart,
    /// The views and operation groups that use it.
    pub affects: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
    /// A list whose entries add up across the layers: a layer removes only its own entries, never one from below, so
    /// a project's entries apply before trust (they only restrict).
    #[serde(default)]
    pub union: bool,
}

/// The keys of the lists whose entries add up across the layers.
#[must_use]
pub fn unions() -> Vec<&'static str> {
    registry()
        .unwrap_or_default()
        .iter()
        .filter(|s| s.union)
        .map(|s| s.key.as_str())
        .collect()
}

static REGISTRY: OnceLock<Vec<Setting>> = OnceLock::new();

/// Every setting, in the order of the config type.
///
/// # Errors
/// The built-in defaults or the declarations do not parse, or a config key has no declaration, or a declaration
/// names no config key (a build defect: the settings tests fail on it).
pub fn registry() -> Result<&'static [Setting]> {
    if let Some(all) = REGISTRY.get() {
        return Ok(all);
    }
    let schema = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<crate::config::Config>()
        .to_value();
    let defaults = crate::config::defaults()?;
    let text = include_str!("../../../config/settings.jsonc");
    let mut declared: BTreeMap<String, Declared> =
        serde_json::from_reader(json_comments::StripComments::new(text.as_bytes()))
            .context("config/settings.jsonc")?;
    // a map keeps the last of two declarations of one key: each key once
    let twice: Vec<&String> = declared
        .keys()
        .filter(|k| {
            let at = format!("\"{k}\":");
            text.lines()
                .filter(|l| l.trim_start().starts_with(&at))
                .count()
                > 1
        })
        .collect();
    if !twice.is_empty() {
        bail!("config/settings.jsonc declares {twice:?} more than once");
    }
    let mut keys = Vec::new();
    walk(&schema, "", &defaults, &mut keys);
    let mut out = Vec::new();
    let mut missing = Vec::new();
    for (key, kind, default) in keys {
        let Some(d) = declared.remove(&key) else {
            missing.push(key);
            continue;
        };
        out.push(Setting {
            module: key.split('.').next().unwrap_or_default().to_string(),
            key,
            label: d.label,
            help: d.help,
            kind,
            default,
            scope: d.scope,
            class: d.class,
            restart: d.restart,
            affects: d.affects,
            check: d.check,
            union: d.union,
        });
    }
    if !missing.is_empty() || !declared.is_empty() {
        bail!(
            "config/settings.jsonc and the config differ: no declaration for {missing:?}; no config key for {:?}",
            declared.keys().collect::<Vec<_>>()
        );
    }
    Ok(REGISTRY.get_or_init(|| out))
}

fn walk(node: &Value, key: &str, default: &Value, out: &mut Vec<(String, Value, Value)>) {
    // a section is an object that must be there; an optional object (null: off) is one setting
    if let (Some(props), "object") = (
        node["properties"].as_object(),
        node["type"].as_str().unwrap_or_default(),
    ) {
        for (name, sub) in props {
            let sub_key = if key.is_empty() {
                name.clone()
            } else {
                format!("{key}.{name}")
            };
            walk(sub, &sub_key, &default[name.as_str()], out);
        }
        // a map flattened into the section (`checks.<name>`): one setting for its entries
        if node["additionalProperties"].is_object() {
            out.push((
                format!("{key}.*"),
                node["additionalProperties"].clone(),
                Value::Null,
            ));
        }
        return;
    }
    out.push((key.to_string(), node.clone(), default.clone()));
}

/// Whether the setting key `setting` (`*` is one key part) is `key` or above it.
fn holds(setting: &str, key: &str) -> bool {
    let mut parts = key.split('.');
    setting
        .split('.')
        .all(|s| parts.next().is_some_and(|p| s == "*" || s == p))
}

/// The setting that holds `key`: its own, or the map setting above it (`reminders.ste100.enabled` is in `reminders`,
/// `checks.lint` in `checks.*`).
#[must_use]
pub fn setting_of(key: &str) -> Option<&'static Setting> {
    registry().ok()?.iter().find(|s| holds(&s.key, key))
}

/// The schema of the value at `key`: the setting's kind, or the part of it below for a key inside a map.
#[must_use]
pub fn kind_of(key: &str) -> Option<Value> {
    let setting = setting_of(key)?;
    let mut kind = setting.kind.clone();
    for part in key.split('.').skip(setting.key.split('.').count()) {
        let next = kind["properties"]
            .get(part)
            .or_else(|| kind.get("additionalProperties"))
            .filter(|k| k.is_object())
            .cloned()?;
        kind = next;
    }
    Some(kind)
}

/// The settings whose key is close to `key`, for a key that is not one: the longest shared start first.
#[must_use]
pub fn closest(key: &str) -> Vec<&'static str> {
    let shared = |k: &str| {
        k.chars()
            .zip(key.chars())
            .take_while(|(a, b)| a == b)
            .count()
    };
    let mut all: Vec<(usize, &'static str)> = registry()
        .unwrap_or_default()
        .iter()
        .map(|s| (shared(&s.key), s.key.as_str()))
        .filter(|(n, _)| *n > 2)
        .collect();
    all.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    all.into_iter().take(3).map(|(_, k)| k).collect()
}

/// The types that a schema allows (`anyOf` and `type` lists unfolded).
fn types(kind: &Value) -> Vec<&str> {
    let mut out: Vec<&str> = match &kind["type"] {
        Value::String(t) => vec![t.as_str()],
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    for part in kind["anyOf"]
        .as_array()
        .into_iter()
        .chain(kind["oneOf"].as_array())
        .flatten()
    {
        out.extend(types(part));
    }
    if out.is_empty() && kind.get("enum").is_some() {
        out.push("string");
    }
    out
}

/// The words of a schema's enum, with those of its `oneOf` parts.
fn words(kind: &Value) -> Vec<&str> {
    let mut out: Vec<&str> = kind["enum"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for part in kind["oneOf"].as_array().into_iter().flatten() {
        out.extend(words(part));
        out.extend(part["const"].as_str());
    }
    out
}

/// What a value of `kind` is, in words.
#[must_use]
pub fn describe(kind: &Value) -> String {
    let range = |what: &str| match (kind["minimum"].as_f64(), kind["maximum"].as_f64()) {
        (Some(lo), Some(hi)) => format!("{what} from {lo} to {hi}"),
        (Some(lo), None) => format!("{what} from {lo}"),
        _ => what.to_string(),
    };
    let enumerated = words(kind);
    let mut out: Vec<String> = Vec::new();
    for t in types(kind) {
        let word = match t {
            "null" => continue,
            "boolean" => "true or false".to_string(),
            "integer" => range("a whole number"),
            "number" => range("a number"),
            "array" => "a list".into(),
            "object" => "a JSON object".into(),
            _ if !enumerated.is_empty() => format!("one of {}", enumerated.join(", ")),
            _ => "text".into(),
        };
        if !out.contains(&word) {
            out.push(word);
        }
    }
    out.join(" or ")
}

/// `text` as a value of `kind`: plain text for a setting that takes text, else JSON (`5`, `true`, `["a"]`).
///
/// # Errors
/// The text is not JSON and the setting does not take text.
pub fn parse(kind: &Value, text: &str) -> Result<Value> {
    if types(kind).contains(&"string") && !text.starts_with(['"', '[', '{']) && text != "null" {
        return Ok(Value::String(text.to_string()));
    }
    serde_json::from_str(text)
        .map_err(|_| anyhow::anyhow!("it takes {}; {text} is not one", describe(kind)))
}

/// Why `value` does not fit `kind` at its top (its type, its words, its range, the entries of a list of rules), if it
/// does not; the whole config's shape is checked after.
fn unfit(kind: &Value, value: &Value) -> Option<String> {
    let ts = types(kind);
    let enumerated = words(kind);
    let fits = match value {
        Value::Null => ts.contains(&"null"),
        Value::Bool(_) => ts.contains(&"boolean"),
        Value::Number(n) => {
            ts.contains(&"number") || (ts.contains(&"integer") && (n.is_i64() || n.is_u64()))
        }
        Value::String(s) => {
            ts.contains(&"string") && (enumerated.is_empty() || enumerated.contains(&s.as_str()))
        }
        Value::Array(_) => ts.contains(&"array"),
        Value::Object(_) => ts.contains(&"object"),
    };
    let out_of_range = value.as_f64().is_some_and(|n| {
        kind["minimum"].as_f64().is_some_and(|lo| n < lo)
            || kind["maximum"].as_f64().is_some_and(|hi| n > hi)
    });
    if !fits || out_of_range {
        return Some(format!("it takes {}", describe(kind)));
    }
    if kind["x-rules"] == true
        && let Ok(rules) = serde_json::from_value::<Vec<crate::actions::Rule>>(value.clone())
    {
        return rules.iter().find_map(crate::actions::unfit);
    }
    None
}

/// A change of one setting in one layer.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Change {
    /// The history entry's id.
    pub id: i64,
    /// Seconds since the epoch.
    pub at: i64,
    pub key: String,
    pub scope: Scope,
    /// The layer file.
    pub file: String,
    /// The value it had in that layer before (null: none there).
    pub old: Value,
    /// The value it has there now (null: none there).
    pub new: Value,
    /// `user`, `agent:<id>` or `session:<id>`.
    pub by: String,
}

/// The settings history in the operator database (step 17 -> 18).
macro_rules! tables {
    () => {
        "
     CREATE TABLE settings_history (
        id    INTEGER PRIMARY KEY,
        at    INTEGER NOT NULL,
        key   TEXT NOT NULL,
        scope TEXT NOT NULL,
        file  TEXT NOT NULL,
        old   TEXT NOT NULL,
        new   TEXT NOT NULL,
        by    TEXT NOT NULL
     );
     CREATE INDEX settings_history_key ON settings_history (key, id);"
    };
}
pub(crate) use tables;

const COLUMNS: &str = "id, at, key, scope, file, old, new, by";

fn change_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Change> {
    let json = |s: String| serde_json::from_str(&s).unwrap_or(Value::Null);
    let scope: String = r.get(3)?;
    Ok(Change {
        id: r.get(0)?,
        at: r.get(1)?,
        key: r.get(2)?,
        scope: serde_json::from_value(Value::String(scope)).unwrap_or(Scope::User),
        file: r.get(4)?,
        old: json(r.get(5)?),
        new: json(r.get(6)?),
        by: r.get(7)?,
    })
}

impl Db {
    /// The changes of `key` and the keys inside it (or of every setting), newest first, at most `limit`.
    ///
    /// # Errors
    /// The database read fails.
    pub fn settings_history(&self, key: Option<&str>, limit: u32) -> Result<Vec<Change>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLUMNS} FROM settings_history WHERE ?1 IS NULL OR key = ?1 OR key LIKE ?1 || '.%'
             ORDER BY id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![key, limit], change_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One change.
    ///
    /// # Errors
    /// The database read fails.
    pub fn settings_change(&self, id: i64) -> Result<Option<Change>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM settings_history WHERE id = ?1"),
                [id],
                change_row,
            )
            .optional()?)
    }
}

/// What a write is: the key, the layer, and the new value there (`None`: remove it there).
pub struct Edit<'a> {
    pub key: &'a str,
    pub scope: Scope,
    pub value: Option<Value>,
    /// `user`, `agent:<id>` or `session:<id>`.
    pub by: &'a str,
}

/// Check `edit` against the registry and the config it would make: the key is a setting, the layer is in its scope
/// and not locked, and the value fits its schema and the whole config's shape. The setting.
///
/// # Errors
/// One sentence for the person: what is wrong and what fits.
pub fn validate(resolved: &Resolved, edit: &Edit) -> Result<&'static Setting> {
    let key = edit.key;
    let Some(setting) = setting_of(key) else {
        let close = closest(key);
        bail!(
            "{key} is not a setting{}",
            if close.is_empty() {
                " (legionctl config settings lists them)".to_string()
            } else {
                format!("; did you mean {}?", close.join(", "))
            }
        );
    };
    if edit.scope != Scope::Computer && resolved.locked_key(key) {
        bail!(
            "{} ({key}) is locked: only this computer's administrator can change it",
            setting.label
        );
    }
    let (scope, _) = resolved
        .declared(key)
        .unwrap_or_else(|| (setting.scope.clone(), setting.class));
    if !scope.contains(&edit.scope) {
        bail!(
            "{} ({key}) is set only for {}",
            setting.label,
            scope
                .iter()
                .map(|s| s.words())
                .collect::<Vec<_>>()
                .join(" or ")
        );
    }
    let Some(value) = &edit.value else {
        return Ok(setting);
    };
    // null at an entry of a map removes the entry, also from the layers below
    let removes = value.is_null() && key != setting.key;
    let kind = kind_of(key).unwrap_or_else(|| setting.kind.clone());
    if let (false, Some(why)) = (removes, unfit(&kind, value)) {
        bail!("{} ({key}): {why}", setting.label);
    }
    let mut merged = resolved.value.clone();
    put(&mut merged, key, (!removes).then(|| value.clone()));
    if let Err(e) = serde_json::from_value::<crate::config::Config>(merged) {
        bail!(
            "{} ({key}) does not take this value: {}",
            setting.label,
            e.to_string().split(" at line").next().unwrap_or_default()
        );
    }
    Ok(setting)
}

/// `value` at the dotted `key` of `into`, with the objects on the way; `None` removes the key.
fn put(into: &mut Value, key: &str, value: Option<Value>) {
    let (parents, last) = key.rsplit_once('.').map_or(("", key), |(p, l)| (p, l));
    let mut at = into;
    for part in parents.split('.').filter(|p| !p.is_empty()) {
        if !at.is_object() {
            *at = Value::Object(serde_json::Map::new());
        }
        let Value::Object(map) = at else { return };
        at = map.entry(part.to_string()).or_insert(Value::Null);
    }
    if !at.is_object() {
        *at = Value::Object(serde_json::Map::new());
    }
    if let Value::Object(map) = at {
        match value {
            Some(v) => map.insert(last.to_string(), v),
            None => map.remove(last),
        };
    }
}

/// The deep check of `setting` for `value` (DESIGN-20 rule 9): why it cannot be used, with the step that fixes it.
#[must_use]
pub fn deep_check(setting: &Setting, value: &Value) -> Option<String> {
    let label = &setting.label;
    match setting.check? {
        Check::Path => {
            let path = crate::config::expand_home(value.as_str()?).ok()?;
            (!path.exists()).then(|| {
                format!(
                    "{label}: {} is not there. Put it there, or change the setting.",
                    path.display()
                )
            })
        }
        Check::Folder => {
            let path = crate::config::expand_home(value.as_str()?).ok()?;
            let writable = |p: &Path| {
                p.is_dir() && rustix::fs::access(p, rustix::fs::Access::WRITE_OK).is_ok()
            };
            let ok = if path.exists() {
                writable(&path)
            } else {
                path.ancestors().find(|a| a.exists()).is_some_and(writable)
            };
            (!ok).then(|| {
                format!(
                    "{label}: legionctl cannot write or make {}. Give your user write access to it, or change the \
                     setting.",
                    path.display()
                )
            })
        }
        Check::Port => {
            let port = u16::try_from(value.as_u64()?).ok().filter(|p| *p != 0)?;
            std::net::TcpListener::bind(("127.0.0.1", port))
                .is_err()
                .then(|| format!("{label}: port {port} is in use. Choose another port."))
        }
    }
}

/// The layer folder of `scope` for `resolved`'s place.
///
/// # Errors
/// A project scope where no repository applies.
pub fn layer_dir(resolved: &Resolved, scope: Scope) -> Result<PathBuf> {
    match scope {
        Scope::User => Ok(resolved.user_dir().to_path_buf()),
        Scope::Project => resolved.repo_dir().map(Path::to_path_buf).context(
            "this folder is in no git repository, so it has no project settings; set it for you instead",
        ),
        Scope::Computer => Ok(crate::config::Dirs::from_env().system),
    }
}

/// Put the history row of `edit` (old value `old`, file `file`) in `conn`; the change.
fn insert(conn: &Connection, edit: &Edit, file: &Path, old: Value) -> Result<Change> {
    let at = crate::db::now();
    let new = edit.value.clone().unwrap_or(Value::Null);
    conn.execute(
        "INSERT INTO settings_history (at, key, scope, file, old, new, by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            at,
            edit.key,
            edit.scope.name(),
            file.display().to_string(),
            old.to_string(),
            new.to_string(),
            edit.by
        ],
    )?;
    Ok(Change {
        id: conn.last_insert_rowid(),
        at,
        key: edit.key.to_string(),
        scope: edit.scope,
        file: file.display().to_string(),
        old,
        new,
        by: edit.by.to_string(),
    })
}

/// Keep a change that the system service made in the computer layer, as [`write`] keeps its own.
///
/// # Errors
/// The history cannot be written.
pub fn record(edit: &Edit, file: &Path, old: Value) -> Result<Change> {
    insert(Db::open_default()?.conn(), edit, file, old)
}

/// Write `edit` into the user or project layer (validated first) and keep it in the history, in one transaction of
/// the history that ends after the file is in place (a failed file write leaves no row). A project file that the
/// user had allowed stays allowed with its new content: the change is the user's own, or one the user allowed.
///
/// # Errors
/// The edit does not validate, or the layer file or the history cannot be written.
pub fn write(resolved: &Resolved, edit: &Edit) -> Result<Change> {
    validate(resolved, edit)?;
    anyhow::ensure!(
        edit.scope != Scope::Computer,
        "a computer setting goes through the system service"
    );
    let dir = layer_dir(resolved, edit.scope)?;
    let file = dir.join(legionctl_machine::settings_file::MANAGED);
    let was_allowed = edit.scope == Scope::Project && project_file_allowed(&file)?;
    let db = Db::open_default()?;
    let tx = db.conn().unchecked_transaction()?;
    let mut change = insert(&tx, edit, &file, Value::Null)?;
    change.old = legionctl_machine::settings_file::edit(&dir, edit.key, edit.value.clone())?;
    if edit.value.is_none() && change.old.is_null() {
        // the transaction ends without the row: nothing changed
        bail!(
            "{} is not set {} (legionctl config settings shows where its value comes from)",
            edit.key,
            match edit.scope {
                Scope::Project => "for this project",
                Scope::User | Scope::Computer => "for you",
            }
        );
    }
    tx.execute(
        "UPDATE settings_history SET old = ?2 WHERE id = ?1",
        params![change.id, change.old.to_string()],
    )?;
    tx.commit()?;
    if was_allowed {
        allow_project_file(&file)?;
    }
    Ok(change)
}

/// Whether the user allowed the project file's content as it is now, or there is no file yet.
fn project_file_allowed(file: &Path) -> Result<bool> {
    if !file.exists() {
        return Ok(true);
    }
    let value = legionctl_machine::profile::read_jsonc(file)?;
    Ok(crate::config::Approvals::read()?.of(file, &value).2 == Some(true))
}

fn allow_project_file(file: &Path) -> Result<()> {
    let value = legionctl_machine::profile::read_jsonc(file)?;
    let (name, hash, _) = crate::config::Approvals::read()?.of(file, &value);
    crate::config::Approvals::answer(&name, &hash, true)
}

/// Set `key` to `value` in your layer as `by`: the operations that change a setting for you (hooks, reminders).
///
/// # Errors
/// As [`write`].
pub fn set_mine(resolved: &Resolved, by: &str, key: &str, value: Value) -> Result<()> {
    write(
        resolved,
        &Edit {
            key,
            scope: Scope::User,
            value: Some(value),
            by,
        },
    )
    .map(|_| ())
}
