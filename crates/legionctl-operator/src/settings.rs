//! The settings operations (DESIGN-14, DESIGN-20): the list from the registry with each value, its source and its
//! deep check; set, unset, add, remove and revert through the one writer (the user and project layers) or the system
//! service (the computer layer, which polkit approves); the history.

use anyhow::{Context, Result, bail};
use legionctl_api::{
    ConfigHistoryIn, ConfigItemIn, ConfigRevertIn, ConfigSetIn, ConfigUnsetIn, Ctx, SettingChanged,
    SettingRow, SettingsHistory, SettingsIn, SettingsView,
};
use legionctl_core::config::Resolved;
use legionctl_core::db::Db;
use legionctl_core::settings::{self, Edit, Restart, Scope, Setting};
use serde_json::{Value, json};

use crate::engine::{Operator, config};

/// The layers in merge order, as the person names them.
fn layer_word(layer: &str) -> &str {
    match Scope::of_layer(layer) {
        Some(scope) => scope.name(),
        None => "defaults",
    }
}

/// The place of `layer` in the merge order (`config.precedence`): a later layer wins.
fn rank(resolved: &Resolved, layer: &str) -> usize {
    resolved
        .layers
        .iter()
        .position(|l| l.name == layer)
        .unwrap_or_default()
}

/// Plain text as it is, other values as JSON.
pub(crate) fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A value in a line: as [`text`], and a long list or map as its count (Change shows it whole).
fn short(value: &Value) -> String {
    let all = text(value);
    match value {
        Value::Array(items) if all.len() > 60 => format!("{} items", items.len()),
        Value::Object(map) if all.len() > 60 => format!("{} entries", map.len()),
        _ => all,
    }
}

/// The value of `key` in `resolved`, and the highest layer that set it or a key inside it.
fn value_and_source(resolved: &Resolved, key: &str) -> (Value, String) {
    // the entries of a map in a section (`checks.*`): the section's keys that are no setting of their own
    if let Some(section) = key.strip_suffix(".*") {
        let explained = resolved.explain(section);
        let entries: serde_json::Map<String, Value> = explained
            .value
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(name, _)| {
                settings::setting_of(&format!("{section}.{name}")).is_some_and(|s| s.key == key)
            })
            .map(|(name, v)| (name.clone(), v.clone()))
            .collect();
        let from = explained
            .settings
            .iter()
            .filter(|(k, _)| {
                entries
                    .keys()
                    .any(|n| k.starts_with(&format!("{section}.{n}")))
            })
            .map(|(_, s)| s.source.layer.as_str())
            .max_by_key(|l| rank(resolved, l))
            .unwrap_or("defaults");
        return (Value::Object(entries), layer_word(from).to_string());
    }
    let explained = resolved.explain(key);
    let from = explained
        .settings
        .iter()
        .map(|(_, s)| s.source.layer.as_str())
        .max_by_key(|l| rank(resolved, l))
        .unwrap_or("defaults");
    (explained.value, layer_word(from).to_string())
}

/// Whether `setting` is a map whose entries are the person's (`reminders`, `views.composer_models`, `checks.*`).
fn is_map(setting: &Setting) -> bool {
    setting.key.ends_with(".*") || setting.kind["additionalProperties"].is_object()
}

/// What a row offers besides Change and Reset: a map's entries in a view of their own.
fn row_actions(setting: &Setting) -> Result<Vec<legionctl_api::ResourceAction>> {
    if !is_map(setting) {
        return Ok(Vec::new());
    }
    Ok(vec![crate::resources::action(
        "Entries",
        "views.render",
        json!({ "name": "setting-entries", "params": [format!("key={}", setting.key.trim_end_matches(".*"))] }),
    )?])
}

/// The entries of a list setting, each with the lowest layer that has it (where it comes from); of a map setting,
/// each entry with its key and the highest layer that sets it.
fn entries(resolved: &Resolved, setting: &Setting, value: &Value) -> Vec<Value> {
    if let (Value::Object(map), true) = (value, is_map(setting)) {
        let base = setting.key.trim_end_matches(".*");
        return map
            .iter()
            .map(|(name, v)| {
                let at = format!("{base}.{name}");
                let from = resolved
                    .trace
                    .iter()
                    .filter(|(k, _)| **k == at || k.starts_with(&format!("{at}.")))
                    .flat_map(|(_, sets)| sets.iter().map(|s| s.source.layer.as_str()))
                    .max_by_key(|l| rank(resolved, l))
                    .unwrap_or("defaults");
                json!({ "name": name, "key": at, "text": short(v), "edit": v, "from": layer_word(from) })
            })
            .collect();
    }
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let sets = resolved
        .trace
        .get(&setting.key)
        .cloned()
        .unwrap_or_default();
    items
        .iter()
        .map(|item| {
            let from = sets
                .iter()
                .find(|s| s.value.as_array().is_some_and(|a| a.contains(item)))
                .map_or("defaults", |s| layer_word(&s.source.layer));
            match item {
                Value::Object(map) => {
                    let mut entry = map.clone();
                    entry.insert("from".into(), from.into());
                    Value::Object(entry)
                }
                other => json!({ "value": other, "from": from }),
            }
        })
        .collect()
}

pub(crate) fn list(ctx: &Ctx, input: &SettingsIn) -> Result<SettingsView> {
    rows(config(Some(ctx.folder()?))?, input)
}

fn rows(resolved: Resolved, input: &SettingsIn) -> Result<SettingsView> {
    // an empty filter is none (a view's parameter that the person left empty)
    let given = |f: &Option<String>| f.clone().filter(|f| !f.trim().is_empty());
    let search = given(&input.search).map(|s| s.to_lowercase());
    let (module, key, view) = (given(&input.module), given(&input.key), given(&input.view));
    let mut items = Vec::new();
    for setting in settings::registry()? {
        if module.as_ref().is_some_and(|m| *m != setting.module)
            || key.as_ref().is_some_and(|k| *k != setting.key)
            || view.as_ref().is_some_and(|v| !setting.affects.contains(v))
            || search.as_ref().is_some_and(|words| {
                let all =
                    format!("{} {} {}", setting.label, setting.key, setting.help).to_lowercase();
                !words.split_whitespace().all(|w| all.contains(w))
            })
        {
            continue;
        }
        let (value, from) = value_and_source(&resolved, &setting.key);
        if input.changed && from == "defaults" {
            continue;
        }
        let finding = resolved.held.get(&setting.key).cloned().unwrap_or_default();
        let mut setting = setting.clone();
        if let Some((scope, class)) = resolved.declared(&setting.key) {
            (setting.scope, setting.class) = (scope, class);
        }
        items.push(SettingRow {
            actions: row_actions(&setting)?,
            entries: entries(&resolved, &setting, &value),
            text: short(&value),
            edit: if value.is_null() {
                json!("")
            } else {
                value.clone()
            },
            locked: resolved.locked_key(&setting.key),
            setting,
            value,
            from,
            finding,
        });
    }
    Ok(SettingsView {
        owned: resolved.config.project.owns.clone(),
        items,
        problems: resolved.problems,
    })
}

/// The settings whose value does not work (their deep checks), as health findings with a button that changes them.
pub(crate) fn findings() -> Result<Vec<legionctl_api::Finding>> {
    let all = rows(
        config(None)?,
        &SettingsIn {
            search: None,
            module: None,
            key: None,
            view: None,
            changed: true,
        },
    )?;
    all.items
        .into_iter()
        .filter(|row| !row.finding.is_empty())
        .map(|row| {
            Ok(legionctl_api::Finding {
                text: row.finding,
                actions: vec![crate::resources::action(
                    "Change",
                    "config.set",
                    json!({ "key": row.setting.key }),
                )?],
            })
        })
        .collect()
}

/// The words of a change for the person: the new value and where, then what must start again, and a check that fails.
fn lines(setting: &Setting, resolved_after: &Resolved, edit: &Edit) -> Vec<String> {
    let whom = match edit.scope {
        Scope::User => "for you",
        Scope::Project => "for this project",
        Scope::UserRepo => "for you in this repository",
        Scope::Computer => "for this computer",
    };
    let (value, _) = value_and_source(resolved_after, &setting.key);
    let mut out = vec![match &edit.value {
        Some(_) => format!(
            "{}: set {whom}. In use now: {}.",
            setting.label,
            short(&value)
        ),
        None => format!(
            "{}: not set {whom} now. In use now: {}.",
            setting.label,
            short(&value)
        ),
    }];
    match setting.restart {
        Restart::None => {}
        Restart::Operator => out.push(
            "Restart legionctl to use it: systemctl --user restart legionctl-operator".into(),
        ),
        Restart::Login => out.push("Log out and in again to use it.".into()),
        Restart::Reboot => out.push("Restart the computer to use it.".into()),
    }
    out
}

impl Operator {
    /// Write `edit` in the layer of its scope: the computer's through the system service (polkit asks), the others
    /// through the one writer; a deep check that fails refuses the value, so the old one stays in force.
    fn change_setting(
        &self,
        project: &std::path::Path,
        edit: &Edit,
        entry: Option<&settings::Entry>,
    ) -> Result<SettingChanged> {
        let resolved = config(Some(project))?;
        let setting = settings::validate(&resolved, edit)?;
        if let Some(value) = &edit.value {
            let (now, _) = value_and_source(&resolved, edit.key);
            if edit.key == setting.key
                && *value != now
                && let Some(why) = settings::deep_check(setting, value)
            {
                bail!("{why}");
            }
        }
        let change = if edit.scope == Scope::Computer {
            let value = edit
                .value
                .as_ref()
                .map_or_else(String::new, Value::to_string);
            let key = edit.key.to_string();
            let (file, old) = self.system(async move |p| p.set_setting(&key, &value).await)?;
            settings::record(
                edit,
                std::path::Path::new(&file),
                // the service says "" for a key that was not there
                (!old.is_empty())
                    .then(|| serde_json::from_str(&old))
                    .transpose()?,
            )?
        } else {
            settings::write(&resolved, edit, entry)?
        };
        Ok(SettingChanged {
            lines: lines(setting, &config(Some(project))?, edit),
            change,
        })
    }

    pub(crate) fn settings_set(&self, ctx: &Ctx, input: &ConfigSetIn) -> Result<SettingChanged> {
        let kind = settings::kind_of(&input.key);
        // the CLI gives text: a setting that does not take text reads it as JSON
        let value = match (&input.value, &kind) {
            (Value::String(t), Some(kind)) => {
                settings::parse(kind, t).with_context(|| input.key.clone())?
            }
            _ => input.value.clone(),
        };
        let by = crate::calls::principal_key(&ctx.principal);
        self.change_setting(
            ctx.folder()?,
            &Edit {
                key: &input.key,
                scope: input.scope,
                value: Some(value),
                by: &by,
            },
            None,
        )
    }

    pub(crate) fn settings_unset(
        &self,
        ctx: &Ctx,
        input: &ConfigUnsetIn,
    ) -> Result<SettingChanged> {
        let by = crate::calls::principal_key(&ctx.principal);
        self.change_setting(
            ctx.folder()?,
            &Edit {
                key: &input.key,
                scope: input.scope,
                value: None,
                by: &by,
            },
            None,
        )
    }

    /// The list `key` as the layers up to `scope` make it, and the setting.
    fn base_list(ctx: &Ctx, input: &ConfigItemIn) -> Result<(&'static Setting, Vec<Value>)> {
        let setting = settings::setting_of(&input.key)
            .filter(|s| s.key == input.key && s.kind["type"] == "array")
            .with_context(|| format!("{} is not a list setting", input.key))?;
        // the list as the layers up to the one that changes give it: none of a layer above it goes into it
        let last = match input.scope {
            Scope::Computer => "system",
            Scope::User => "user",
            Scope::Project => "repo",
            Scope::UserRepo => "user-repo",
        };
        let project = match input.scope {
            Scope::Project | Scope::UserRepo => Some(ctx.folder()?),
            Scope::User | Scope::Computer => None,
        };
        let resolved = legionctl_core::config::resolve_through(
            &legionctl_core::config::Dirs::from_env(),
            project,
            Some(last),
        )?;
        // a list whose entries add up: the layer's own entries (what legionctl wrote there)
        if setting.union {
            let file = settings::layer_dir(&resolved, input.scope)?
                .join(legionctl_machine::settings_file::MANAGED)
                .display()
                .to_string();
            let own = resolved
                .trace
                .get(&input.key)
                .into_iter()
                .flatten()
                .filter(|s| s.source.file == file)
                .filter_map(|s| s.value.as_array().cloned())
                .flatten()
                .collect();
            return Ok((setting, own));
        }
        let (value, _) = value_and_source(&resolved, &input.key);
        Ok((setting, value.as_array().cloned().unwrap_or_default()))
    }

    /// The entry of `input` as the list's item: a rule with its pattern and reason, else the value.
    fn item(setting: &Setting, input: &ConfigItemIn) -> Result<Value> {
        if setting.kind["x-rules"] == true {
            let pattern = text(&input.value);
            let mut rule =
                json!({ "pattern": pattern, "reason": input.reason.clone().unwrap_or_default() });
            if let Some(set) = &input.set {
                rule["set"] = json!(set);
            }
            return Ok(rule);
        }
        if input.reason.is_some() || input.set.is_some() {
            bail!("{} takes no reason and no set", setting.label);
        }
        Ok(match (&input.value, setting.kind.get("items")) {
            (Value::String(t), Some(kind)) => settings::parse(kind, t)?,
            (other, _) => other.clone(),
        })
    }

    pub(crate) fn settings_add(&self, ctx: &Ctx, input: &ConfigItemIn) -> Result<SettingChanged> {
        let (setting, mut items) = Self::base_list(ctx, input)?;
        let item = Self::item(setting, input)?;
        if items
            .iter()
            .any(|i| settings::same_entry(setting, i, &item))
        {
            bail!(
                "{}: {} is in the list already",
                setting.label,
                text(&input.value)
            );
        }
        items.push(item.clone());
        let by = crate::calls::principal_key(&ctx.principal);
        self.change_setting(
            ctx.folder()?,
            &Edit {
                key: &input.key,
                scope: input.scope,
                value: Some(Value::Array(items)),
                by: &by,
            },
            Some(&settings::Entry {
                setting,
                item: &item,
                add: true,
            }),
        )
    }

    pub(crate) fn settings_remove(
        &self,
        ctx: &Ctx,
        input: &ConfigItemIn,
    ) -> Result<SettingChanged> {
        let by = crate::calls::principal_key(&ctx.principal);
        // an entry of a map: null at its key removes it, also from the layers below
        if let Some(map) = settings::setting_of(&input.key)
            .filter(|s| is_map(s) && s.key.trim_end_matches(".*") == input.key)
        {
            let resolved = config(Some(ctx.folder()?))?;
            let (value, _) = value_and_source(&resolved, &map.key);
            let name = text(&input.value);
            if value.get(&name).is_none() {
                bail!("{}: there is no {name}", map.label);
            }
            return self.change_setting(
                ctx.folder()?,
                &Edit {
                    key: &format!("{}.{name}", input.key),
                    scope: input.scope,
                    value: Some(Value::Null),
                    by: &by,
                },
                None,
            );
        }
        let (setting, mut items) = Self::base_list(ctx, input)?;
        let item = Self::item(
            setting,
            &ConfigItemIn {
                reason: None,
                set: None,
                ..input.clone()
            },
        )?;
        let before = items.len();
        items.retain(|i| !settings::same_entry(setting, i, &item));
        if items.len() == before && setting.union {
            bail!(
                "{}: {} is not one of the entries {}; each layer removes only its own (legionctl config settings shows where each entry comes from)",
                setting.label,
                text(&input.value),
                match input.scope {
                    Scope::User => "for you",
                    Scope::Project => "of this project",
                    Scope::UserRepo => "of yours for this repository",
                    Scope::Computer => "of this computer",
                }
            );
        }
        if items.len() == before {
            bail!(
                "{}: {} is not in the list",
                setting.label,
                text(&input.value)
            );
        }
        self.change_setting(
            ctx.folder()?,
            &Edit {
                key: &input.key,
                scope: input.scope,
                value: Some(Value::Array(items)),
                by: &by,
            },
            Some(&settings::Entry {
                setting,
                item: &item,
                add: false,
            }),
        )
    }

    pub(crate) fn settings_revert(
        &self,
        ctx: &Ctx,
        input: &ConfigRevertIn,
    ) -> Result<SettingChanged> {
        let change = Db::open_default()?
            .settings_change(input.id)?
            .with_context(|| {
                format!(
                    "no change {} (legionctl config history lists them)",
                    input.id
                )
            })?;
        // a project change's file is <repository>/.legionctl/managed.jsonc
        let project = match change.scope {
            Scope::Project => std::path::Path::new(&change.file)
                .parent()
                .and_then(std::path::Path::parent)
                .map(std::path::Path::to_path_buf)
                .context("the change's project folder")?,
            // your layer for a repository is found from a checkout of that repository: the caller's
            Scope::UserRepo => {
                let here = ctx.folder()?.to_path_buf();
                let file = settings::layer_dir(&config(Some(&here))?, Scope::UserRepo)?
                    .join(legionctl_machine::settings_file::MANAGED);
                anyhow::ensure!(
                    file.display().to_string() == change.file,
                    "change {} is in {}: undo it in a checkout of that repository",
                    input.id,
                    change.file
                );
                here
            }
            Scope::User | Scope::Computer => std::path::PathBuf::from("/"),
        };
        let by = crate::calls::principal_key(&ctx.principal);
        self.change_setting(
            &project,
            &Edit {
                key: &change.key,
                scope: change.scope,
                value: change.old.clone(),
                by: &by,
            },
            None,
        )
    }
}

pub(crate) fn history(input: &ConfigHistoryIn) -> Result<SettingsHistory> {
    Ok(SettingsHistory {
        items: Db::open_default()?.settings_history(input.key.as_deref(), input.limit)?,
    })
}
