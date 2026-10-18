//! Forms from the operation catalog (DESIGN-09 G3): each operation's input schema gives its fields, so every
//! operation is reachable from a button with no form made by hand. The app draws the fields; a terminal asks them as
//! prompts; both make the input here, with the same checks.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::OpInfo;

/// What a field takes.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text,
    Integer,
    Number,
    Bool,
    /// One of these words: a button each for up to [`BUTTONS`], else a dropdown.
    Choice(Vec<String>),
    /// Any of these words (a list of words): a multi-select.
    Choices(Vec<String>),
    /// Values with commas between them.
    List {
        integers: bool,
    },
}

/// A choice of up to this many words shows a button each; more is a dropdown.
pub const BUTTONS: usize = 3;

/// One field of an operation's form.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub kind: Kind,
    pub required: bool,
    /// Its help text, one line.
    pub help: String,
    /// The schema's default as text (empty when none).
    pub default: String,
}

/// The words of an enum or of a `oneOf` of enums and consts.
fn choices(prop: &Value) -> Option<Vec<String>> {
    let words = |v: &Value| -> Vec<String> {
        v["enum"]
            .as_array()
            .map(|e| {
                e.iter()
                    .filter_map(|w| w.as_str().map(String::from))
                    .collect()
            })
            .or_else(|| v["const"].as_str().map(|c| vec![c.to_string()]))
            .unwrap_or_default()
    };
    let mut out = words(prop);
    for part in prop["oneOf"].as_array().into_iter().flatten() {
        out.extend(words(part));
    }
    (!out.is_empty()).then_some(out)
}

/// The fields of `op`'s form: every input field but the ones the client fills (the session, a secret from the
/// environment) and `dry_run` (the approval flow sets it).
#[must_use]
pub fn fields(op: &OpInfo) -> Vec<Field> {
    fields_for(op, &BTreeMap::new())
}

/// The fields of `op`'s form for the texts `given`: a setting's value (`x-setting-value` names the key's field) is
/// the field of that setting, from the registry (a choice, yes or no, a number with its range, a list).
#[must_use]
pub fn fields_for(op: &OpInfo, given: &BTreeMap<String, String>) -> Vec<Field> {
    let mut schema = (op.input)();
    if let Some(props) = schema["properties"].as_object_mut() {
        for prop in props.values_mut() {
            let setting = prop["x-setting-value"]
                .as_str()
                .and_then(|field| given.get(field))
                .and_then(|key| Some((legionctl_core::settings::setting_of(key)?, key)));
            if let Some((setting, key)) = setting {
                let mut kind = legionctl_core::settings::kind_of(key).unwrap_or_default();
                let fits = legionctl_core::settings::describe(&kind);
                kind["description"] = format!("{} It takes {fits}.", setting.help).into();
                *prop = kind;
            }
        }
    }
    let required: Vec<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, prop)| {
            *name != "dry_run" && prop["format"] != "session" && prop["x-from-env"].is_null()
        })
        .map(|(name, prop)| {
            let types: Vec<&str> = match &prop["type"] {
                Value::String(t) => vec![t.as_str()],
                Value::Array(t) => t.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            let kind = if let Some(words) = choices(prop) {
                Kind::Choice(words)
            } else if types.contains(&"boolean") {
                Kind::Bool
            } else if types.contains(&"integer") {
                Kind::Integer
            } else if types.contains(&"number") {
                Kind::Number
            } else if let (true, Some(words)) = (types.contains(&"array"), choices(&prop["items"]))
            {
                Kind::Choices(words)
            } else if types.contains(&"array") {
                Kind::List {
                    integers: prop["items"]["type"] == "integer",
                }
            } else {
                Kind::Text
            };
            let default = match &prop["default"] {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Field {
                name: name.clone(),
                kind,
                required: required.contains(&name.as_str()),
                help: prop["description"]
                    .as_str()
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                default,
            }
        })
        .collect()
}

/// The input of `op` from the form's texts (`values`, by field): typed, empty optional fields left out (their
/// defaults apply); the client resolves its paths after ([`crate::cli::fill_user_fields`]).
///
/// # Errors
/// A required field is empty, or a value is not of its field's kind.
pub fn input(op: &OpInfo, values: &BTreeMap<String, String>) -> Result<Value> {
    let mut out = Map::new();
    for field in fields_for(op, values) {
        let text = values.get(&field.name).map_or("", |v| v.trim());
        if text.is_empty() {
            if field.required {
                bail!("{} is required: {}", field.name, field.help);
            }
            continue;
        }
        let what = || format!("{}: {text}", field.name);
        let value = match &field.kind {
            Kind::Text => Value::from(text),
            Kind::Integer => Value::from(
                text.parse::<i64>()
                    .with_context(|| format!("{} is a whole number", what()))?,
            ),
            Kind::Number => Value::from(
                text.parse::<f64>()
                    .with_context(|| format!("{} is a number", what()))?,
            ),
            Kind::Bool => match text {
                "true" | "yes" => Value::Bool(true),
                "false" | "no" => Value::Bool(false),
                _ => bail!("{} is yes or no", what()),
            },
            Kind::Choice(words) => {
                if !words.iter().any(|w| w == text) {
                    bail!("{} is one of {}", what(), words.join(", "));
                }
                Value::from(text)
            }
            Kind::Choices(words) => text
                .split(',')
                .map(str::trim)
                .filter(|w| !w.is_empty())
                .map(|w| {
                    if words.iter().any(|k| k == w) {
                        Ok(Value::from(w))
                    } else {
                        bail!(
                            "{} has {w}, which is not one of {}",
                            what(),
                            words.join(", ")
                        )
                    }
                })
                .collect::<Result<Vec<_>>>()?
                .into(),
            Kind::List { integers } => text
                .split(',')
                .map(str::trim)
                .filter(|w| !w.is_empty())
                .map(|w| {
                    if *integers {
                        w.parse::<i64>().map(Value::from).with_context(|| {
                            format!("{} has a value that is not a whole number", what())
                        })
                    } else {
                        Ok(Value::from(w))
                    }
                })
                .collect::<Result<Vec<_>>>()?
                .into(),
        };
        out.insert(field.name, value);
    }
    Ok(Value::Object(out))
}

/// The form's texts for an input that a button gives (its values as text).
#[must_use]
pub fn texts(input: &Value) -> BTreeMap<String, String> {
    input
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| {
            let text = match v {
                Value::String(s) => s.clone(),
                Value::Array(items) => items
                    .iter()
                    .map(|i| i.as_str().map_or_else(|| i.to_string(), String::from))
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            };
            (k.clone(), text)
        })
        .collect()
}

/// How a button's operation is approved (the class language of DESIGN-09 G2), the same on every surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// A read runs at once.
    Run,
    /// A change with a dry run shows its plan first.
    Plan,
    /// Another change asks with its operation and input.
    Confirm,
}

/// The route of `op`.
#[must_use]
pub fn route(op: &OpInfo) -> Route {
    // a read, or a change of the caller's own session (its dossier choice, its hooks): nothing to confirm
    if matches!(op.class, crate::Class::Read | crate::Class::Session) {
        Route::Run
    } else if (op.input)()["properties"].get("dry_run").is_some() {
        Route::Plan
    } else {
        Route::Confirm
    }
}

/// Whether a button with `input` needs the form first: a required field is missing, or it gives no input to an
/// operation that takes some.
#[must_use]
pub fn needed(op: &OpInfo, input: &Value) -> bool {
    let fields = fields(op);
    let given = input.as_object().map_or(0, Map::len);
    // a setting's new value is always chosen in the form
    let setting = (op.input)()["properties"]
        .as_object()
        .is_some_and(|p| p.values().any(|prop| prop["x-setting-value"].is_string()));
    setting
        || fields
            .iter()
            .any(|f| f.required && input.get(&f.name).is_none_or(Value::is_null))
        || (given == 0 && !fields.is_empty())
}
