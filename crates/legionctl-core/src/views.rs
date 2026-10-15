//! Views (design 23, C4): a declarative JSONC file that binds one widget of a closed set to a read operation, so the
//! `ui-composer` member and a person make the thread's pane 3 from data, not code. A view lives in a config layer:
//! the repository's `.legionctl/views/` (shared with the code) above the user's `~/.config/legionctl/views/`. This
//! module holds the format, its checks (except the operation's class, which the operator knows), the files, and the
//! shape of a source's answer for each widget.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The closed set of widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Widget {
    /// An array of objects: one row each, one cell for each of `columns`.
    Table,
    /// Any JSON value as indented rows.
    Tree,
    /// A number against `max`.
    Gauge,
    /// An array of numbers.
    Sparkline,
    /// A string of Markdown.
    Markdown,
    /// A string of unified diff.
    Diff,
    /// An array: one row each (the fields of `columns` for an object).
    List,
    /// Other views by name, top to bottom.
    Stack,
}

impl Widget {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Widget::Table => "table",
            Widget::Tree => "tree",
            Widget::Gauge => "gauge",
            Widget::Sparkline => "sparkline",
            Widget::Markdown => "markdown",
            Widget::Diff => "diff",
            Widget::List => "list",
            Widget::Stack => "stack",
        }
    }
}

/// The type of a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Param {
    String,
    Integer,
}

/// Where a view reads its data.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// A read operation, by its dotted name (`thread.show`).
    pub op: String,
    /// The operation's input; `{param}` in a string is the parameter's value (a whole `"{param}"` of an integer
    /// parameter is the number).
    #[serde(default)]
    pub input: Value,
    /// The part of the answer to show: `$`, or `$.field.0.field` (a number is an array index).
    #[serde(default = "root")]
    pub path: String,
}

fn root() -> String {
    "$".into()
}

/// A view in a stack, or on a thread: its name and its parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Placed {
    pub name: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Refresh {
    /// Event kinds after which the view reads its source again; `*` at the end matches any rest
    /// (`code.tests.*`). A view reads again after each `views.save` too.
    #[serde(default)]
    pub events: Vec<String>,
}

/// One view file: `<layer>/views/<name>.jsonc`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct View {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// a-z, 0-9 and '-': the file is `<name>.jsonc`.
    pub name: String,
    /// The parameters and their types.
    #[serde(default)]
    pub params: BTreeMap<String, Param>,
    pub widget: Widget,
    /// Every widget but `stack` has one.
    #[serde(default)]
    pub source: Option<Source>,
    /// table: the field of each cell (required); list: the fields of an object item.
    #[serde(default)]
    pub columns: Vec<String>,
    /// gauge: the value of a full gauge (default 100).
    #[serde(default)]
    pub max: Option<f64>,
    /// stack: the views it holds.
    #[serde(default)]
    pub views: Vec<Placed>,
    #[serde(default)]
    pub refresh: Refresh,
}

/// The JSON schema of a view file, as `docs/api/view.schema.json` holds it.
#[must_use]
pub fn schema() -> (&'static str, Value) {
    (
        "docs/api/view.schema.json",
        serde_json::to_value(schemars::schema_for!(View)).unwrap_or_default(),
    )
}

/// The `{param}` names in the strings of `value`.
fn holes(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(start) = rest.find('{') {
                let Some(end) = rest[start..].find('}') else {
                    break;
                };
                out.push(rest[start + 1..start + end].to_string());
                rest = &rest[start + end + 1..];
            }
        }
        Value::Array(items) => items.iter().for_each(|v| holes(v, out)),
        Value::Object(map) => map.values().for_each(|v| holes(v, out)),
        _ => {}
    }
}

/// `value` with the parameters' values in its strings: a whole `"{param}"` is the value itself.
fn fill(value: &Value, values: &BTreeMap<String, Value>) -> Value {
    match value {
        Value::String(s) => {
            if let Some(v) = s
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .and_then(|name| values.get(name))
            {
                return v.clone();
            }
            let mut out = s.clone();
            for (name, v) in values {
                let text = v.as_str().map_or_else(|| v.to_string(), String::from);
                out = out.replace(&format!("{{{name}}}"), &text);
            }
            Value::from(out)
        }
        Value::Array(items) => items.iter().map(|v| fill(v, values)).collect(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), fill(v, values)))
            .collect(),
        other => other.clone(),
    }
}

impl View {
    /// A view from its JSONC text, with the checks that need no operator.
    ///
    /// # Errors
    /// The text is not a view: not JSON, a field or widget outside the format, a bad name, a widget without what it
    /// needs, or a `{param}` that the view does not declare.
    pub fn parse(text: &str) -> Result<View> {
        let view: View =
            serde_json::from_reader(json_comments::StripComments::new(text.as_bytes()))
                .context("not a view")?;
        ensure!(
            crate::dossier::valid_name(&view.name),
            "a view name is a-z, 0-9 and '-': {}",
            view.name
        );
        match (view.widget, &view.source) {
            (Widget::Stack, Some(_)) => bail!("a stack has views, not a source"),
            (Widget::Stack, None) => ensure!(!view.views.is_empty(), "a stack needs views"),
            (_, None) => bail!("a {} needs a source", view.widget.as_str()),
            (_, Some(source)) => {
                ensure!(view.views.is_empty(), "only a stack has views");
                ensure!(
                    source.path == "$" || source.path.starts_with("$."),
                    "a path starts with $ or $.: {}",
                    source.path
                );
                let mut names = Vec::new();
                holes(&source.input, &mut names);
                if let Some(name) = names.iter().find(|n| !view.params.contains_key(*n)) {
                    bail!("the input names {{{name}}}, which is not a parameter of the view");
                }
            }
        }
        ensure!(
            view.widget != Widget::Table || !view.columns.is_empty(),
            "a table needs columns"
        );
        Ok(view)
    }

    /// The source's input with the parameters in it.
    ///
    /// # Errors
    /// A parameter is missing or unknown, or an integer parameter is not a number.
    pub fn input(&self, params: &BTreeMap<String, String>) -> Result<Value> {
        if let Some(name) = params.keys().find(|k| !self.params.contains_key(*k)) {
            bail!("view {} has no parameter {name}", self.name);
        }
        let mut values = BTreeMap::new();
        for (name, kind) in &self.params {
            let given = params
                .get(name)
                .with_context(|| format!("view {} needs the parameter {name}", self.name))?;
            let value = match kind {
                Param::String => Value::from(given.as_str()),
                Param::Integer => Value::from(
                    given
                        .parse::<i64>()
                        .with_context(|| format!("{name} is an integer: {given}"))?,
                ),
            };
            values.insert(name.clone(), value);
        }
        let source = self.source.as_ref().context("a stack has no source")?;
        Ok(fill(&source.input, &values))
    }
}

/// The view folders, highest layer first: the repository's (when `repo` is in one), then the user's.
#[must_use]
pub fn dirs(repo: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
    let mut dirs = Vec::new();
    if let Some(root) = repo.and_then(crate::config::repo_root) {
        dirs.push(("repository", root.join(".legionctl/views")));
    }
    dirs.push(("user", crate::config::Dirs::from_env().user.join("views")));
    dirs
}

/// Every view file of the layers: (layer, file, the view or why it is not one). A name in a higher layer hides the
/// same name below it.
#[must_use]
pub fn list(repo: Option<&Path>) -> Vec<(&'static str, PathBuf, Result<View>)> {
    let mut out: Vec<(&'static str, PathBuf, Result<View>)> = Vec::new();
    for (layer, dir) in dirs(repo) {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonc"))
            .collect();
        files.sort();
        for file in files {
            if out
                .iter()
                .any(|(_, f, _)| f.file_name() == file.file_name())
            {
                continue;
            }
            let view = std::fs::read_to_string(&file)
                .map_err(anyhow::Error::from)
                .and_then(|text| View::parse(&text));
            out.push((layer, file, view));
        }
    }
    out
}

/// The view `name` of the highest layer that has it.
///
/// # Errors
/// No layer has it, or its file is not a view.
pub fn find(name: &str, repo: Option<&Path>) -> Result<View> {
    ensure!(crate::dossier::valid_name(name), "no view {name}");
    for (_, dir) in dirs(repo) {
        let file = dir.join(format!("{name}.jsonc"));
        if file.exists() {
            let text = std::fs::read_to_string(&file)?;
            return View::parse(&text).with_context(|| file.display().to_string());
        }
    }
    bail!("no view {name}; `views list` shows the views")
}

/// Write the view of `text` (already checked) into the repository's layer (`repo`) or the user's, as written: its
/// comments stay. Returns the file.
///
/// # Errors
/// `repo` is not in a repository, or the file cannot be written.
pub fn save(view: &View, text: &str, repo: Option<&Path>) -> Result<PathBuf> {
    let dir = match repo {
        Some(dir) => crate::config::repo_root(dir)
            .with_context(|| format!("{} is not in a repository", dir.display()))?
            .join(".legionctl/views"),
        None => crate::config::Dirs::from_env().user.join("views"),
    };
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.jsonc", view.name));
    let part = dir.join(format!(".{}.jsonc.part", view.name));
    std::fs::write(&part, text)?;
    std::fs::rename(&part, &file)?;
    Ok(file)
}

/// A view with its data, in the shape of its widget.
pub struct Rendered {
    pub name: String,
    pub widget: Widget,
    pub columns: Vec<String>,
    /// table: cells; list and tree: one cell per row.
    pub rows: Vec<Vec<String>>,
    /// gauge: the value; sparkline: the values.
    pub numbers: Vec<f64>,
    pub max: f64,
    /// markdown and diff.
    pub text: String,
    /// stack: its views.
    pub parts: Vec<Rendered>,
    /// The source failed, or its answer does not fit the widget; the other views of a stack stay.
    pub error: Option<String>,
    /// The view's `refresh.events`.
    pub refresh: Vec<String>,
}

/// A JSON value as one cell: a string as it is, anything else as JSON.
fn cell(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), String::from)
}

/// The part of `value` at `path` (`$` or `$.a.0.b`).
///
/// # Errors
/// A step of the path is not in the value.
pub fn pick(value: &Value, path: &str) -> Result<Value> {
    let mut at = value;
    for step in path
        .trim_start_matches('$')
        .split('.')
        .filter(|s| !s.is_empty())
    {
        at = match step.parse::<usize>() {
            Ok(i) if at.is_array() => &at[i],
            _ => &at[step],
        };
        ensure!(!at.is_null(), "the answer has nothing at {path}");
    }
    Ok(at.clone())
}

fn tree(value: &Value, depth: usize, key: &str, out: &mut Vec<Vec<String>>) {
    let pad = "  ".repeat(depth);
    let label = if key.is_empty() {
        String::new()
    } else {
        format!("{key}: ")
    };
    match value {
        Value::Object(map) => {
            if !key.is_empty() {
                out.push(vec![format!("{pad}{key}")]);
            }
            for (k, v) in map {
                tree(v, depth + usize::from(!key.is_empty()), k, out);
            }
        }
        Value::Array(items) => {
            if !key.is_empty() {
                out.push(vec![format!("{pad}{key}")]);
            }
            for (i, v) in items.iter().enumerate() {
                tree(v, depth + usize::from(!key.is_empty()), &i.to_string(), out);
            }
        }
        other => out.push(vec![format!("{pad}{label}{}", cell(other))]),
    }
}

impl Rendered {
    /// An empty view of `view` with `error`.
    #[must_use]
    pub fn failed(view: &View, error: String) -> Rendered {
        Rendered {
            name: view.name.clone(),
            widget: view.widget,
            columns: view.columns.clone(),
            rows: Vec::new(),
            numbers: Vec::new(),
            max: view.max.unwrap_or(100.0),
            text: String::new(),
            parts: Vec::new(),
            error: Some(error),
            refresh: view.refresh.events.clone(),
        }
    }

    /// The view with the answer of its source (not a stack).
    #[must_use]
    pub fn of(view: &View, answer: Result<Value>) -> Rendered {
        let shaped = answer.and_then(|answer| {
            let path = view.source.as_ref().map_or("$", |s| s.path.as_str());
            let data = pick(&answer, path)?;
            let mut out = Rendered::failed(view, String::new());
            out.error = None;
            let array = || {
                data.as_array()
                    .with_context(|| format!("a {} needs an array at {path}", view.widget.as_str()))
            };
            match view.widget {
                Widget::Table => {
                    out.rows = array()?
                        .iter()
                        .map(|row| view.columns.iter().map(|c| cell(&row[c])).collect())
                        .collect();
                }
                Widget::List => {
                    out.rows = array()?
                        .iter()
                        .map(|item| {
                            if item.is_object() && !view.columns.is_empty() {
                                vec![
                                    view.columns
                                        .iter()
                                        .map(|c| cell(&item[c]))
                                        .collect::<Vec<_>>()
                                        .join(" · "),
                                ]
                            } else {
                                vec![cell(item)]
                            }
                        })
                        .collect();
                }
                Widget::Tree => tree(&data, 0, "", &mut out.rows),
                Widget::Gauge => {
                    out.numbers = vec![
                        data.as_f64()
                            .with_context(|| format!("a gauge needs a number at {path}"))?,
                    ];
                }
                Widget::Sparkline => {
                    out.numbers = array()?
                        .iter()
                        .map(|v| {
                            v.as_f64()
                                .with_context(|| format!("a sparkline needs numbers at {path}"))
                        })
                        .collect::<Result<_>>()?;
                }
                Widget::Markdown | Widget::Diff => {
                    out.text = data
                        .as_str()
                        .with_context(|| {
                            format!("a {} needs a string at {path}", view.widget.as_str())
                        })?
                        .to_string();
                }
                Widget::Stack => bail!("a stack has no source"),
            }
            Ok(out)
        });
        shaped.unwrap_or_else(|e| Rendered::failed(view, format!("{e:#}")))
    }
}
