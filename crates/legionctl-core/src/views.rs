//! Views (design 23 C4, DESIGN-09): a declarative JSONC file that binds one widget of a closed set to a read operation,
//! so that agents (the `ui-composer`) and people make the screens from data, not code. A view lives in a layer: the
//! repository's `.legionctl/views/` (shared with the code) above the user's `~/.config/legionctl/views/` above the
//! seeds that legionctl ships (`config/views/`, compiled in: the answers to the common intents, so the app opens with
//! no model call). Each save is a version (`views/.history/<name>/<n>.jsonc`; the seed is version 0); which version
//! shows is a fact (DESIGN-10). This module holds the format, its checks (except the operation's class, which the
//! operator knows), the files, and the shape of a source's answer for each widget, so that every surface (app,
//! CLI) draws the same data.

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
    /// An array: one row each (the fields of `columns` for an object); one object: a row per field of `columns`
    /// (all its fields without them), "field: value".
    List,
    /// Other views, top to bottom: by name, or inline.
    Stack,
    /// One value with its unit, its state against `warn` and `critical`, a note and a trend.
    Stat,
    /// An array of objects as lines over time: `x` (seconds since the epoch, or any number), `y`, one chart per value
    /// of `series` (a chart never mixes kinds), with axes.
    Chart,
    /// Nodes (an array of objects, `node` says their fields) as a tree with counts, children in the data or read
    /// on a click (`expand`), a click on a leaf sets a parameter (`select`), marks from a second source
    /// (`highlight`), and the edges of a node (`edges`).
    Graph,
    /// The form of an operation (`form`), made from its input schema; it runs with the operation's approval.
    Form,
    /// An array of events: the `columns` are the time (seconds since the epoch), the actor, the kind and the text.
    Timeline,
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
            Widget::Stat => "stat",
            Widget::Chart => "chart",
            Widget::Graph => "graph",
            Widget::Form => "form",
            Widget::Timeline => "timeline",
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
    /// The part of the answer to show: `$`, or `$.field.0.field` (a number is an array index; `[field=value]` is the
    /// first item of an array whose field has that value).
    #[serde(default = "root")]
    pub path: String,
    /// The operation answers in pages (`items`, `next`): read every page; the answer is `{items}`.
    #[serde(default)]
    pub all: bool,
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

/// A part of a stack: a view by name, or a view written in place.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Part {
    Placed(Placed),
    Inline(Box<View>),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Refresh {
    /// Event kinds after which the view reads its source again; `*` at the end matches any rest
    /// (`code.tests.*`). A view reads again after each `views.save` too.
    #[serde(default)]
    pub events: Vec<String>,
    /// Read the source again each this many seconds (live values); a refresh reads the data, never the spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<u32>,
}

/// Rows in groups: by a field's value, or by rules on a row's text (the first rule that matches).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Group {
    /// The field whose value is the group.
    #[serde(default)]
    pub field: Option<String>,
    /// Rules on the row's text (a string row, or the field's value): the group of the first rule that matches.
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// The group of a row that no rule matches.
    #[serde(default)]
    pub rest: Option<String>,
    /// The words of a group value (`{"vm": "Virtual machines"}`).
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// The groups first in this order; others after them.
    #[serde(default)]
    pub order: Vec<String>,
    /// A group with more rows than this starts folded (its count shows).
    #[serde(default)]
    pub fold: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub group: String,
    /// The text starts with this.
    #[serde(default)]
    pub prefix: Option<String>,
    /// The text has one of these words (any case).
    #[serde(default)]
    pub any: Vec<String>,
}

/// The state colour of a number column: ok below `warn`, warn below `critical`, critical above.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Level {
    pub column: String,
    pub warn: f64,
    pub critical: f64,
    /// Only rows whose fields have these values (`{"unit": "°C"}`).
    #[serde(default)]
    pub when: BTreeMap<String, Value>,
}

/// A button that runs an operation with its normal approval: no confirm for a read, the plan first when the
/// operation has a dry run, an inline confirm for a change of the session or the workspace, polkit for the machine.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// The words on the button; `{row.field}` and `{item.field}` are a row's and an item's values.
    pub label: String,
    /// The operation (`power.apply`), or `{item.op}`.
    pub op: String,
    /// The operation's input: `{param}`, `{row.field}`, `{item.field}` in strings; a whole `"{item.input}"` is the value.
    #[serde(default)]
    pub input: Value,
    /// A row action: one button per item of this array field of the row.
    #[serde(default)]
    pub each: Option<String>,
}

/// The fields of a graph's node objects (`a|b`: the first that is not null, so that levels may differ).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeFields {
    /// Its id: unique among the nodes; the value `expand` and `select` take.
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: Option<String>,
    /// A number beside it (its children's count when not given).
    #[serde(default)]
    pub count: Option<String>,
    /// Its state, and the colour of each state word (`{"done": "ok", "missing": "critical"}`).
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub states: BTreeMap<String, String>,
    /// The field that holds its children in the data.
    #[serde(default)]
    pub children: Option<String>,
    /// The field that is true for a leaf (no children to read).
    #[serde(default)]
    pub leaf: Option<String>,
}

/// Marks on a graph's nodes: a node whose id is a value of these fields of the second source's items (a folder:
/// a value under it).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Highlight {
    pub source: Source,
    pub fields: Vec<String>,
    /// What a mark means (`{param}` of the view).
    pub label: String,
}

/// The edges of a graph in the same answer.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Edges {
    /// The path of the edges in the answer (`$.edges`).
    pub path: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub kind: Option<String>,
}

/// One view file: `<layer>/views/<name>.jsonc`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct View {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// a-z, 0-9 and '-': the file is `<name>.jsonc`.
    pub name: String,
    /// The tab's and the navigator's words; default the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the view answers, in the user's words ("the machine now"): an intent finds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// The navigator's space that lists it: machine, work or agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// The parameters and their types.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Param>,
    /// A parameter's value when none is given.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defaults: BTreeMap<String, String>,
    pub widget: Widget,
    /// Every widget but `stack` has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// table: the field of each cell (required; `a.b` is a field of a field, `a|b` the first that is not null); list: the
    /// fields of an object item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    /// The words of a column's header (`{"result.label": "Kind"}`) and of a button's label (`{"give-windows": "Give it
    /// to Windows"}`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// How a column's or a stat's value shows: `ago`, `time` (seconds since the epoch), `int`, `1dp`, `count` (an
    /// array's length), `join` (an array or object's values with " / ").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub formats: BTreeMap<String, String>,
    /// table and list: rows in groups, each with its count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Group>,
    /// table: the state colours of number columns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<Level>,
    /// table and list: only the rows whose fields have these values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filter: BTreeMap<String, Value>,
    /// table and list: only the last this many rows, newest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<usize>,
    /// gauge: the value of a full gauge (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// stat: the unit after the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// stat: the limits of its state colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warn: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critical: Option<f64>,
    /// stat: the line under the value; `{field}` is a value of the source's whole answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// stat: the text when the value is missing: its cause, with `{field}` of the answer; another widget: the text
    /// while a parameter is empty (its source is not read: "Select a symbol").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
    /// stat: a path of the answer to an array of numbers: the trend under the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trend: Option<String>,
    /// chart: the x field (seconds since the epoch show as times), the y field, and the series field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// graph: its nodes' fields, the parameter that a click on a node with children to read sets (the same view
    /// read with it gives the children), the parameter that a click on a leaf sets (a stack's other parts read it),
    /// the marks, the edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeFields>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<Highlight>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edges: Option<Edges>,
    /// form: the operation and the values it starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Action>,
    /// stack: the views it holds; an inline view gets the stack's parameters that it declares, a placed view its
    /// `params` with `{param}` of the stack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<Part>,
    /// Buttons of the view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// table and list: buttons of each row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub row_actions: Vec<Action>,
    #[serde(default)]
    pub refresh: Refresh,
}

/// The JSON schema of a view file, as `docs/api/view.schema.json` holds it.
///
/// # Errors
/// The schema does not serialize.
pub fn schema() -> Result<(&'static str, Value)> {
    Ok((
        "docs/api/view.schema.json",
        serde_json::to_value(schemars::schema_for!(View))?,
    ))
}

/// The seeds that legionctl ships: (name, text).
const SEEDS: [(&str, &str); 10] = [
    (
        "machine-now",
        include_str!("../../../config/views/machine-now.jsonc"),
    ),
    ("health", include_str!("../../../config/views/health.jsonc")),
    (
        "resources",
        include_str!("../../../config/views/resources.jsonc"),
    ),
    ("bench", include_str!("../../../config/views/bench.jsonc")),
    (
        "apps-gpu",
        include_str!("../../../config/views/apps-gpu.jsonc"),
    ),
    (
        "operations",
        include_str!("../../../config/views/operations.jsonc"),
    ),
    (
        "code-tree",
        include_str!("../../../config/views/code-tree.jsonc"),
    ),
    (
        "code-graph",
        include_str!("../../../config/views/code-graph.jsonc"),
    ),
    (
        "plan-graph",
        include_str!("../../../config/views/plan-graph.jsonc"),
    ),
    (
        "accounts",
        include_str!("../../../config/views/accounts.jsonc"),
    ),
];

/// The seed of `name`, if legionctl ships one.
#[must_use]
pub fn seed(name: &str) -> Option<&'static str> {
    SEEDS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, text)| *text)
}

/// The `{name}` holes in the strings of `value`.
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

/// The value of a hole: a parameter (`{param}`), or a field of the context's values (`{row.a.b}`).
fn lookup<'a>(name: &str, values: &'a BTreeMap<String, Value>) -> Option<&'a Value> {
    let mut parts = name.split('.');
    let mut at = values.get(parts.next()?)?;
    for part in parts {
        at = at.get(part)?;
    }
    (!at.is_null()).then_some(at)
}

/// `value` with the holes filled from `values`: a whole `"{name}"` is the value itself.
fn fill(value: &Value, values: &BTreeMap<String, Value>) -> Value {
    match value {
        Value::String(s) => {
            if let Some(v) = s
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .filter(|name| !name.contains('{'))
                .and_then(|name| lookup(name, values))
            {
                return v.clone();
            }
            Value::from(text(s, values))
        }
        Value::Array(items) => items.iter().map(|v| fill(v, values)).collect(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), fill(v, values)))
            .collect(),
        other => other.clone(),
    }
}

/// `value` with the parameters' values in its strings (a highlight's input).
#[must_use]
pub fn fill_values(value: &Value, values: &BTreeMap<String, Value>) -> Value {
    fill(value, values)
}

/// `template` with each `{param}` as text (a placed part's parameter, a highlight's words).
#[must_use]
pub fn fill_text(template: &str, values: &BTreeMap<String, Value>) -> String {
    text(template, values)
}

/// `template` with each `{name}` as text; a hole with no value stays as it is.
fn text(template: &str, values: &BTreeMap<String, Value>) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        out.push_str(&rest[..start]);
        let name = &rest[start + 1..start + end];
        match lookup(name, values) {
            Some(v) => out.push_str(&cell(v)),
            None => out.push_str(&rest[start..=start + end]),
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// A path: `$`, then `.field`, `.0` or `.[field=value]` steps; nothing else (no `[]`, `*`, `|` or filters).
fn check_path(path: &str) -> Result<()> {
    let bad = || anyhow::anyhow!("a path is $, then .field, .0 or .[field=value] steps: {path}");
    let rest = path.strip_prefix('$').ok_or_else(bad)?;
    if rest.is_empty() {
        return Ok(());
    }
    let rest = rest.strip_prefix('.').ok_or_else(bad)?;
    for step in rest.split('.') {
        let ok = match step.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            Some(inner) => inner
                .split_once('=')
                .is_some_and(|(k, v)| !k.is_empty() && !v.is_empty()),
            None => !step.is_empty() && !step.contains(['[', ']', '|', '*', ' ']),
        };
        ensure!(ok, bad());
    }
    Ok(())
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
        view.check()?;
        Ok(view)
    }

    fn check(&self) -> Result<()> {
        ensure!(
            crate::dossier::valid_name(&self.name),
            "a view name is a-z, 0-9 and '-': {}",
            self.name
        );
        match (self.widget, &self.source) {
            (Widget::Stack, Some(_)) => bail!("a stack has views, not a source"),
            (Widget::Stack, None) => ensure!(!self.views.is_empty(), "a stack needs views"),
            (Widget::Form, None) => {
                ensure!(self.form.is_some(), "a form needs its form (op, input)");
            }
            (Widget::Form, Some(_)) => bail!("a form has its form, not a source"),
            (_, None) => bail!("a {} needs a source", self.widget.as_str()),
            (_, Some(source)) => {
                ensure!(self.views.is_empty(), "only a stack has views");
                check_path(&source.path)?;
                let mut names = Vec::new();
                holes(&source.input, &mut names);
                if let Some(name) = names.iter().find(|n| !self.params.contains_key(*n)) {
                    bail!("the input names {{{name}}}, which is not a parameter of the view");
                }
            }
        }
        ensure!(
            self.widget != Widget::Table || !self.columns.is_empty(),
            "a table needs columns"
        );
        ensure!(
            self.widget != Widget::Chart || (self.x.is_some() && self.y.is_some()),
            "a chart needs x and y"
        );
        if let Some(h) = &self.highlight {
            check_path(&h.source.path)?;
        }
        if let Some(e) = &self.edges {
            check_path(&e.path)?;
        }
        ensure!(
            self.widget != Widget::Graph || self.node.is_some(),
            "a graph needs node (its fields)"
        );
        ensure!(
            self.widget != Widget::Timeline || self.columns.len() == 4,
            "a timeline's columns are its time, actor, kind and text"
        );
        for name in [&self.expand, &self.select]
            .into_iter()
            .flatten()
            .chain(self.defaults.keys())
        {
            ensure!(
                self.params.contains_key(name),
                "{name} is not a parameter of the view"
            );
        }
        for part in &self.views {
            if let Part::Inline(view) = part {
                view.check()
                    .with_context(|| format!("stack {} holds {}", self.name, view.name))?;
            }
        }
        Ok(())
    }

    /// The parameters' values as JSON.
    ///
    /// # Errors
    /// A parameter is missing or unknown, or an integer parameter is not a number.
    pub fn values(&self, params: &BTreeMap<String, String>) -> Result<BTreeMap<String, Value>> {
        if let Some(name) = params.keys().find(|k| !self.params.contains_key(*k)) {
            bail!("view {} has no parameter {name}", self.name);
        }
        let mut values = BTreeMap::new();
        for (name, kind) in &self.params {
            let given = params
                .get(name)
                .or_else(|| self.defaults.get(name))
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
        Ok(values)
    }

    /// The source's input with the parameters in it.
    ///
    /// # Errors
    /// A parameter is missing or unknown, or an integer parameter is not a number.
    pub fn input(&self, params: &BTreeMap<String, String>) -> Result<Value> {
        let values = self.values(params)?;
        let source = self.source.as_ref().context("a stack has no source")?;
        // no input is an empty one: every operation's input is an object
        Ok(match &source.input {
            Value::Null => Value::Object(serde_json::Map::new()),
            input => fill(input, &values),
        })
    }

    /// The words of the view.
    #[must_use]
    pub fn title(&self) -> String {
        self.title.clone().unwrap_or_else(|| self.name.clone())
    }

    /// The navigator's space: its own, else the space of its first source's group (code, plan, dossier and
    /// workspace: work; agents, sessions, threads, the catalog, views and facts: agents; the rest: machine).
    #[must_use]
    pub fn space(&self) -> String {
        if let Some(space) = &self.space {
            return space.clone();
        }
        let op = self.source.as_ref().map(|s| s.op.clone()).or_else(|| {
            self.views.iter().find_map(|p| match p {
                Part::Inline(v) => v.source.as_ref().map(|s| s.op.clone()),
                Part::Placed(_) => None,
            })
        });
        let group = op
            .as_deref()
            .and_then(|o| o.split('.').next())
            .unwrap_or("");
        match group {
            "code" | "plan" | "dossier" | "workspace" => "work",
            "agents" | "session" | "thread" | "ops" | "views" | "facts" | "skills" | "hooks"
            | "tools" => "agents",
            _ => "machine",
        }
        .into()
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

/// Every view of the layers and the seeds: (layer, file, the view or why it is not one). A name in a higher layer
/// hides the same name below it; a seed's file is `seed:<name>`.
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
    for (name, text) in SEEDS {
        if !out
            .iter()
            .any(|(_, f, _)| f.file_stem().is_some_and(|s| s == name))
        {
            out.push((
                "seed",
                PathBuf::from(format!("seed:{name}.jsonc")),
                View::parse(text),
            ));
        }
    }
    out
}

/// The folder and the file of the view `name` of the highest layer that has it, if any.
fn file_of(name: &str, repo: Option<&Path>) -> Option<(PathBuf, PathBuf)> {
    dirs(repo).into_iter().find_map(|(_, dir)| {
        let file = dir.join(format!("{name}.jsonc"));
        file.exists().then_some((dir, file))
    })
}

/// The history folder of the view `name` in the layer folder `dir`.
fn history(dir: &Path, name: &str) -> PathBuf {
    dir.join(".history").join(name)
}

/// The numbers of the saved versions of `name` in its highest layer (1, 2, ...), oldest first.
#[must_use]
pub fn versions(name: &str, repo: Option<&Path>) -> Vec<u32> {
    let Some((dir, _)) = file_of(name, repo) else {
        return Vec::new();
    };
    let mut out: Vec<u32> = std::fs::read_dir(history(&dir, name))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse().ok())
        })
        .collect();
    out.sort_unstable();
    out
}

/// The text of the view `name`: `version` 0 is the seed, another number a saved version of its highest layer, none
/// the newest file (else the seed).
///
/// # Errors
/// No layer has it, or that version is not there.
pub fn text_of(name: &str, version: Option<u32>, repo: Option<&Path>) -> Result<String> {
    ensure!(crate::dossier::valid_name(name), "no view {name}");
    match version {
        Some(0) => seed(name)
            .map(String::from)
            .with_context(|| format!("view {name} has no seed (version 0)")),
        Some(n) => {
            let (dir, _) = file_of(name, repo).with_context(|| format!("no view {name}"))?;
            let file = history(&dir, name).join(format!("{n}.jsonc"));
            std::fs::read_to_string(&file)
                .with_context(|| format!("view {name} has no version {n} ({})", file.display()))
        }
        None => match file_of(name, repo) {
            Some((_, file)) => Ok(std::fs::read_to_string(file)?),
            None => seed(name)
                .map(String::from)
                .with_context(|| format!("no view {name}; `views list` shows the views")),
        },
    }
}

/// The view `name` at `version` (see [`text_of`]).
///
/// # Errors
/// No layer has it, or its text is not a view.
pub fn find(name: &str, version: Option<u32>, repo: Option<&Path>) -> Result<View> {
    let text = text_of(name, version, repo)?;
    View::parse(&text).with_context(|| format!("view {name}"))
}

/// Write the view of `text` (already checked) into the repository's layer (`repo`) or the user's, as written (its
/// comments stay), as its next version. Returns the file and the version.
///
/// # Errors
/// `repo` is not in a repository, or a file cannot be written.
pub fn save(view: &View, text: &str, repo: Option<&Path>) -> Result<(PathBuf, u32)> {
    let dir = match repo {
        Some(dir) => crate::config::repo_root(dir)
            .with_context(|| format!("{} is not in a repository", dir.display()))?
            .join(".legionctl/views"),
        None => crate::config::Dirs::from_env().user.join("views"),
    };
    let past = history(&dir, &view.name);
    std::fs::create_dir_all(&past)?;
    let file = dir.join(format!("{}.jsonc", view.name));
    let mut numbers: Vec<u32> = std::fs::read_dir(&past)?
        .flatten()
        .filter_map(|e| e.path().file_stem()?.to_str()?.parse().ok())
        .collect();
    numbers.sort_unstable();
    // a file saved before versions were kept is version 1
    if numbers.is_empty() && file.exists() {
        std::fs::copy(&file, past.join("1.jsonc"))?;
        numbers.push(1);
    }
    let version = numbers.last().map_or(1, |n| n + 1);
    for target in [past.join(format!("{version}.jsonc")), file.clone()] {
        let partial = target.with_extension("jsonc.part");
        std::fs::write(&partial, text)?;
        std::fs::rename(&partial, &target)?;
    }
    Ok((file, version))
}

/// One button with its operation and input, filled.
#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub label: String,
    pub op: String,
    pub input: Value,
}

/// One line of a chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    /// (x, y): x in the answer's numbers (seconds since the epoch for times).
    pub points: Vec<(f64, f64)>,
    /// Each point's x as it shows (the x field's format: a time is "10-16 15:18").
    pub labels: Vec<String>,
}

/// A view with its data, in the shape of its widget.
pub struct Rendered {
    pub name: String,
    pub title: String,
    pub intent: Option<String>,
    pub widget: Widget,
    pub columns: Vec<String>,
    /// table: cells; list and tree: one cell per row.
    pub rows: Vec<Vec<String>>,
    /// The group of each row (table, list with a `group`), and the groups in their order.
    pub groups: Vec<String>,
    pub group_order: Vec<String>,
    /// A group with more rows than this starts folded.
    pub fold: Option<usize>,
    /// table: the state of each cell: "", ok, warn, critical.
    pub marks: Vec<Vec<String>>,
    /// gauge: the value; sparkline: the values; stat: its trend.
    pub numbers: Vec<f64>,
    pub max: f64,
    /// markdown and diff; stat: the value as text.
    pub text: String,
    /// stat: the unit, the note and the state.
    pub unit: String,
    pub note: String,
    pub level: String,
    /// chart: its lines, and whether x is a time.
    pub series: Vec<Series>,
    pub x_time: bool,
    /// stack: its views.
    pub parts: Vec<Rendered>,
    /// The view's buttons, and each row's.
    pub actions: Vec<Button>,
    pub row_actions: Vec<Vec<Button>>,
    /// The source failed, or its answer does not fit the widget; the other views of a stack stay.
    pub error: Option<String>,
    /// The view's `refresh.events` and `refresh.every`.
    pub refresh: Vec<String>,
    pub every: Option<u32>,
    /// The version that shows (0: the seed) and the saved versions.
    pub version: u32,
    pub versions: Vec<u32>,
    /// graph: its nodes and edges, the parameters a click sets, and what a mark means.
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub expand: Option<String>,
    pub select: Option<String>,
    pub legend: String,
    /// The parameters it was read with (a graph's click keeps them).
    pub params: BTreeMap<String, String>,
}

/// A node of a graph.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub count: Option<u64>,
    /// ok, warn, critical, or empty.
    pub state: String,
    /// A mark of the highlight.
    pub marked: bool,
    /// No children to read.
    pub leaf: bool,
    pub children: Vec<Node>,
}

/// An edge of a graph.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

/// A JSON value as one cell: a string as it is, anything else as JSON.
fn cell(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), String::from)
}

/// The part of `value` at `path` (`$`, `$.a.0.b`, `$.[name=x].value`).
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
        at = if let Some((field, want)) = step
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .and_then(|s| s.split_once('='))
        {
            at.as_array()
                .and_then(|items| items.iter().find(|i| cell(&i[field]) == want))
                .with_context(|| format!("the answer has no item with {field}={want} at {path}"))?
        } else {
            match step.parse::<usize>() {
                Ok(i) if at.is_array() => &at[i],
                _ => &at[step],
            }
        };
        ensure!(!at.is_null(), "the answer has nothing at {path}");
    }
    Ok(at.clone())
}

/// A field of a row (`a.b`; `a|b`: the first that is not null), or null.
fn field<'a>(row: &'a Value, path: &str) -> &'a Value {
    path.split('|')
        .map(|one| {
            one.split('.')
                .fold(row, |at, step| match step.parse::<usize>() {
                    Ok(i) if at.is_array() => &at[i],
                    _ => &at[step],
                })
        })
        .find(|v| !v.is_null())
        .unwrap_or(&Value::Null)
}

/// A value as its format says.
fn shown(value: &Value, format: Option<&str>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    match (format, value) {
        // an empty value says what it is, never a bare dash
        (_, Value::Null) => "no value".into(),
        (Some("ago"), v) => v.as_i64().map_or_else(|| cell(v), |at| ago(now - at)),
        (Some("time"), v) => v.as_i64().map_or_else(|| cell(v), utc),
        (Some("int"), v) => v.as_f64().map_or_else(|| cell(v), |n| format!("{n:.0}")),
        (Some("1dp"), v) => v.as_f64().map_or_else(|| cell(v), |n| format!("{n:.1}")),
        (Some("count"), Value::Array(items)) => items.len().to_string(),
        (_, Value::Array(items)) if items.is_empty() => "none".into(),
        (_, Value::String(text)) if text.is_empty() => "none".into(),
        (Some("join"), Value::Array(items)) => {
            items.iter().map(cell).collect::<Vec<_>>().join(" / ")
        }
        (Some("join"), Value::Object(map)) => {
            map.values().map(cell).collect::<Vec<_>>().join(" / ")
        }
        (_, v) => cell(v),
    }
}

/// How long ago, in a short word: now, 5 min, 3 h, 2 d.
fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86_400),
    }
}

/// Seconds since the epoch as a UTC date and time: 2026-10-16 15:18.
#[must_use]
pub fn utc(at: i64) -> String {
    // the civil date of a day count (Howard Hinnant's days_from_civil, reversed)
    let z = at.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let secs = at.rem_euclid(86_400);
    format!(
        "{year}-{month:02}-{day:02} {:02}:{:02}",
        secs / 3600,
        secs / 60 % 60
    )
}

/// The state of `value` against its limits.
fn level(value: f64, warn: f64, critical: f64) -> &'static str {
    if value >= critical {
        "critical"
    } else if value >= warn {
        "warn"
    } else {
        "ok"
    }
}

/// The rows' fields have the wanted values.
fn matches(row: &Value, want: &BTreeMap<String, Value>) -> bool {
    want.iter().all(|(k, v)| field(row, k) == v)
}

/// The buttons of `actions` for the context `values` (parameters, and `row`); a label in `labels` shows its words.
fn buttons(
    actions: &[Action],
    labels: &BTreeMap<String, String>,
    values: &BTreeMap<String, Value>,
) -> Vec<Button> {
    let mut out = Vec::new();
    for action in actions {
        let items: Vec<Value> = match &action.each {
            Some(f) => values
                .get("row")
                .map(|row| field(row, f))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            None => vec![Value::Null],
        };
        for item in items {
            let mut values = values.clone();
            values.insert("item".into(), item);
            let label = text(&action.label, &values);
            out.push(Button {
                label: labels.get(&label).cloned().unwrap_or(label),
                op: text(&action.op, &values),
                input: fill(&action.input, &values),
            });
        }
    }
    out
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

/// A graph node of an item, with its children in the data.
fn node(fields: &NodeFields, item: &Value) -> Node {
    let children: Vec<Node> = fields
        .children
        .as_ref()
        .and_then(|c| field(item, c).as_array())
        .map(|items| items.iter().map(|i| node(fields, i)).collect())
        .unwrap_or_default();
    let state = fields
        .state
        .as_ref()
        .map(|s| cell(field(item, s)))
        .unwrap_or_default();
    Node {
        id: cell(field(item, &fields.id)),
        label: cell(field(item, &fields.label)),
        kind: fields
            .kind
            .as_ref()
            .map(|k| cell(field(item, k)))
            .unwrap_or_default(),
        count: fields
            .count
            .as_ref()
            .and_then(|c| field(item, c).as_u64())
            .filter(|c| *c > 0)
            .or_else(|| {
                (!children.is_empty()).then(|| u64::try_from(children.len()).unwrap_or(u64::MAX))
            }),
        state: fields.states.get(&state).cloned().unwrap_or_default(),
        marked: false,
        leaf: fields
            .leaf
            .as_ref()
            .is_some_and(|l| field(item, l) == &Value::Bool(true))
            || (fields.children.is_some() && children.is_empty()),
        children,
    }
}

impl Rendered {
    /// An empty view of `view` with `error`.
    #[must_use]
    pub fn failed(view: &View, error: String) -> Rendered {
        Rendered {
            name: view.name.clone(),
            title: view.title(),
            intent: view.intent.clone(),
            widget: view.widget,
            columns: view
                .columns
                .iter()
                .map(|c| view.labels.get(c).cloned().unwrap_or_else(|| c.clone()))
                .collect(),
            rows: Vec::new(),
            groups: Vec::new(),
            group_order: Vec::new(),
            fold: view.group.as_ref().and_then(|g| g.fold),
            marks: Vec::new(),
            numbers: Vec::new(),
            max: view.max.unwrap_or(100.0),
            text: String::new(),
            unit: view.unit.clone().unwrap_or_default(),
            note: String::new(),
            level: String::new(),
            series: Vec::new(),
            x_time: false,
            parts: Vec::new(),
            actions: Vec::new(),
            row_actions: Vec::new(),
            error: Some(error),
            refresh: view.refresh.events.clone(),
            every: view.refresh.every,
            version: 0,
            versions: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            expand: view.expand.clone(),
            select: view.select.clone(),
            legend: String::new(),
            params: BTreeMap::new(),
        }
    }

    /// The view with the answer of its source (not a stack), for the parameters' `values`.
    #[must_use]
    pub fn of(view: &View, values: &BTreeMap<String, Value>, answer: Result<Value>) -> Rendered {
        let mut out = Rendered::failed(view, String::new());
        out.error = None;
        out.actions = buttons(&view.actions, &view.labels, values);
        if let Some(form) = &view.form {
            out.actions = buttons(std::slice::from_ref(form), &view.labels, values);
        }
        if matches!(view.widget, Widget::Stack | Widget::Form) {
            return out;
        }
        if let Err(e) = answer.and_then(|answer| out.shape(view, values, &answer)) {
            out.error = Some(format!("{e:#}"));
        }
        out
    }

    /// The rows of a table or list: cells, state colours, each row's buttons, groups.
    fn rows_of(&mut self, view: &View, values: &BTreeMap<String, Value>, items: &[Value]) {
        for item in items {
            let format = |c: &String| view.formats.get(c).map(String::as_str);
            self.rows.push(if view.widget == Widget::Table {
                view.columns
                    .iter()
                    .map(|c| shown(field(item, c), format(c)))
                    .collect()
            } else if item.is_object() && !view.columns.is_empty() {
                vec![
                    view.columns
                        .iter()
                        .map(|c| shown(field(item, c), format(c)))
                        .collect::<Vec<_>>()
                        .join(" · "),
                ]
            } else {
                vec![cell(item)]
            });
            if !view.levels.is_empty() {
                self.marks.push(
                    view.columns
                        .iter()
                        .map(|c| {
                            view.levels
                                .iter()
                                .find(|l| l.column == *c && matches(item, &l.when))
                                .and_then(|l| {
                                    field(item, c)
                                        .as_f64()
                                        .map(|v| level(v, l.warn, l.critical).to_string())
                                })
                                .unwrap_or_default()
                        })
                        .collect(),
                );
            }
            if !view.row_actions.is_empty() {
                let mut values = values.clone();
                values.insert("row".into(), item.clone());
                self.row_actions
                    .push(buttons(&view.row_actions, &view.labels, &values));
            }
        }
        if let Some(group) = &view.group {
            self.group(group, items);
            // a table grouped by a column whose cells only say their group: the group's head says it, not each row
            let column = group
                .field
                .as_ref()
                .filter(|_| view.widget == Widget::Table)
                .and_then(|f| view.columns.iter().position(|c| c == f))
                .filter(|k| {
                    group.rules.is_empty()
                        || self
                            .rows
                            .iter()
                            .zip(&self.groups)
                            .all(|(r, g)| r[*k].eq_ignore_ascii_case(g))
                });
            if let Some(k) = column {
                self.columns.remove(k);
                for cells in self.rows.iter_mut().chain(self.marks.iter_mut()) {
                    if k < cells.len() {
                        cells.remove(k);
                    }
                }
            }
        }
    }

    fn shape(
        &mut self,
        view: &View,
        values: &BTreeMap<String, Value>,
        answer: &Value,
    ) -> Result<()> {
        let path = view.source.as_ref().map_or("$", |s| s.path.as_str());
        if view.widget == Widget::Stat {
            return self.stat(view, answer, path);
        }
        let data = pick(answer, path)?;
        let array = || -> Result<Vec<Value>> {
            let items = data
                .as_array()
                .with_context(|| format!("a {} needs an array at {path}", view.widget.as_str()))?;
            let mut items: Vec<Value> = items
                .iter()
                .filter(|i| matches(i, &view.filter))
                .cloned()
                .collect();
            if let Some(n) = view.last {
                let skip = items.len().saturating_sub(n);
                items.drain(..skip);
                items.reverse();
            }
            Ok(items)
        };
        match view.widget {
            // a list of one object: a row per column (all its fields without columns), "field: value"
            Widget::List if data.is_object() => {
                let map = data.as_object().cloned().unwrap_or_default();
                let fields: Vec<String> = if view.columns.is_empty() {
                    map.keys().cloned().collect()
                } else {
                    view.columns.clone()
                };
                self.rows = fields
                    .iter()
                    .map(|c| {
                        let label = view.labels.get(c).unwrap_or(c);
                        vec![format!(
                            "{label}: {}",
                            shown(field(&data, c), view.formats.get(c).map(String::as_str))
                        )]
                    })
                    .collect();
            }
            Widget::Table | Widget::List => self.rows_of(view, values, &array()?),
            Widget::Tree => tree(&data, 0, "", &mut self.rows),
            Widget::Gauge => {
                self.numbers = vec![
                    data.as_f64()
                        .with_context(|| format!("a gauge needs a number at {path}"))?,
                ];
            }
            Widget::Sparkline => {
                self.numbers = array()?
                    .iter()
                    .map(|v| {
                        v.as_f64()
                            .with_context(|| format!("a sparkline needs numbers at {path}"))
                    })
                    .collect::<Result<_>>()?;
            }
            Widget::Markdown | Widget::Diff => {
                self.text = data
                    .as_str()
                    .with_context(|| {
                        format!("a {} needs a string at {path}", view.widget.as_str())
                    })?
                    .to_string();
            }
            Widget::Chart => self.chart(view, &array()?)?,
            Widget::Graph => self.graph(view, answer, &array()?)?,
            Widget::Timeline => {
                for item in &array()? {
                    let at = |i: usize| field(item, &view.columns[i]);
                    self.rows.push(vec![
                        at(0)
                            .as_i64()
                            .map_or_else(|| cell(at(0)), |t| t.to_string()),
                        shown(at(1), None),
                        shown(at(2), None),
                        shown(at(3), None),
                    ]);
                }
            }
            Widget::Stack | Widget::Stat | Widget::Form => {
                bail!("a {} has no source", view.widget.as_str())
            }
        }
        Ok(())
    }

    /// The groups of the rows, in the view's order, then in the order they come.
    fn group(&mut self, group: &Group, items: &[Value]) {
        self.groups = items
            .iter()
            .map(|item| {
                let value = group
                    .field
                    .as_ref()
                    .map_or_else(|| cell(item), |f| cell(field(item, f)));
                let lower = value.to_lowercase();
                let name = group
                    .rules
                    .iter()
                    .find(|r| {
                        r.prefix
                            .as_ref()
                            .is_some_and(|p| value.starts_with(p.as_str()))
                            || r.any.iter().any(|w| lower.contains(&w.to_lowercase()))
                    })
                    .map(|r| r.group.clone())
                    .or_else(|| {
                        (!group.rules.is_empty())
                            .then(|| group.rest.clone())
                            .flatten()
                    })
                    .unwrap_or(value);
                group.labels.get(&name).cloned().unwrap_or(name)
            })
            .collect();
        let mut order: Vec<String> = group
            .order
            .iter()
            .map(|g| group.labels.get(g).cloned().unwrap_or_else(|| g.clone()))
            .filter(|g| self.groups.contains(g))
            .collect();
        for g in &self.groups {
            if !order.contains(g) {
                order.push(g.clone());
            }
        }
        self.group_order = order;
    }

    fn stat(&mut self, view: &View, answer: &Value, path: &str) -> Result<()> {
        let context: BTreeMap<String, Value> = answer
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        self.note = view
            .note
            .as_deref()
            .map(|n| text(n, &context))
            .unwrap_or_default();
        if let Some(trend) = &view.trend {
            self.numbers = pick(answer, trend)
                .ok()
                .and_then(|v| {
                    v.as_array()
                        .map(|a| a.iter().filter_map(Value::as_f64).collect())
                })
                .unwrap_or_default();
        }
        match pick(answer, path) {
            Ok(value) => {
                self.text = shown(&value, view.formats.get("value").map(String::as_str));
                if let (Some(v), Some(warn), Some(critical)) =
                    (value.as_f64(), view.warn, view.critical)
                {
                    self.level = level(v, warn, critical).into();
                }
                Ok(())
            }
            // the cause, not a dash (U8)
            Err(e) => match &view.missing {
                Some(missing) => {
                    self.text = "–".into();
                    self.level = "missing".into();
                    self.note = text(missing, &context);
                    Ok(())
                }
                None => Err(e),
            },
        }
    }

    /// The nodes of a graph (with their children in the data) and its edges.
    fn graph(&mut self, view: &View, answer: &Value, items: &[Value]) -> Result<()> {
        let fields = view
            .node
            .as_ref()
            .context("a graph needs node (its fields)")?;
        self.nodes = items.iter().map(|i| node(fields, i)).collect();
        if let Some(edges) = &view.edges {
            self.edges = pick(answer, &edges.path)?
                .as_array()
                .with_context(|| format!("a graph's edges are an array at {}", edges.path))?
                .iter()
                .map(|e| Edge {
                    from: cell(field(e, &edges.from)),
                    to: cell(field(e, &edges.to)),
                    kind: edges
                        .kind
                        .as_ref()
                        .map(|k| cell(field(e, k)))
                        .unwrap_or_default(),
                })
                .collect();
        }
        Ok(())
    }

    /// Mark the nodes (and their children) whose id is one of `values`, or a folder above one.
    pub fn mark(&mut self, values: &[String], legend: String) {
        fn walk(nodes: &mut [Node], values: &[String]) {
            for n in nodes {
                let folder = format!("{}/", n.id);
                n.marked = values.iter().any(|v| *v == n.id || v.starts_with(&folder));
                walk(&mut n.children, values);
            }
        }
        walk(&mut self.nodes, values);
        self.legend = legend;
    }

    fn chart(&mut self, view: &View, items: &[Value]) -> Result<()> {
        let (x, y) = (
            view.x.as_deref().unwrap_or("x"),
            view.y.as_deref().unwrap_or("y"),
        );
        self.x_time = view.formats.get(x).is_some_and(|f| f == "time");
        let mut series: Vec<Series> = Vec::new();
        for item in items {
            let (Some(px), Some(py)) = (field(item, x).as_f64(), field(item, y).as_f64()) else {
                continue;
            };
            let label = match view.formats.get(x).map(String::as_str) {
                Some("time") => field(item, x)
                    .as_i64()
                    .map_or_else(|| cell(field(item, x)), |at| utc(at)[5..].to_string()),
                format => shown(field(item, x), format),
            };
            let name = view
                .series
                .as_ref()
                .map(|s| shown(field(item, s), None))
                .unwrap_or_default();
            match series.iter_mut().find(|s| s.name == name) {
                Some(s) => {
                    s.points.push((px, py));
                    s.labels.push(label);
                }
                None => series.push(Series {
                    name,
                    points: vec![(px, py)],
                    labels: vec![label],
                }),
            }
        }
        ensure!(
            !series.is_empty() || items.is_empty(),
            "a chart needs numbers at {x} and {y}"
        );
        self.series = series;
        Ok(())
    }
}
