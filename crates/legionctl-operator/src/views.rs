//! The `views` operations (design 23 C4, DESIGN-09): the format and the files are in `legionctl_core::views`; here a
//! view's source is checked against the registry (a read operation only, so a view cannot run a change; its buttons
//! name public operations, which run with their own approval) and called as the caller, with the same input checks as
//! a call from outside. Which version of a view shows is the fact `view:<name>` `version` (DESIGN-10).

use std::collections::BTreeMap;
use std::path::Path;

use std::fmt::Write as _;

use anyhow::{Context, Result, bail, ensure};
use legionctl_api::{
    Class, Ctx, Engine, IntoWire, Lines, OPS, Principal, ViewData, ViewInfo, ViewPlaceIn,
    ViewRenderIn, ViewSaveIn, ViewTextIn, ViewVersionIn, ViewsListIn,
};
use legionctl_core::facts::{self, New, Scope};
use legionctl_core::views::{Part, Placed, Rendered, Source, View, Widget};
use serde_json::Value;

use crate::engine::dossiers;

/// How deep stacks may hold stacks: a stack that holds itself stops here.
const DEPTH: usize = 4;

/// `name=value` words as parameters.
fn params(words: &[String]) -> Result<BTreeMap<String, String>> {
    words
        .iter()
        .map(|w| {
            let (k, v) = w
                .split_once('=')
                .with_context(|| format!("a parameter is name=value: {w}"))?;
            Ok((k.to_string(), v.to_string()))
        })
        .collect()
}

/// The checks that need the registry and the other views: a stack's views exist, a source is a read operation, a
/// button names a public operation.
fn check(view: &View, repo: Option<&Path>) -> Result<()> {
    for action in view
        .actions
        .iter()
        .chain(&view.row_actions)
        .chain(&view.form)
    {
        // `{item.op}`: the operation comes with the data (a resource's actions)
        if !action.op.starts_with('{') {
            OPS.iter()
                .find(|op| op.name == action.op && !op.internal())
                .with_context(|| {
                    format!(
                        "the button {} names no operation {}",
                        action.label, action.op
                    )
                })?;
        }
    }
    if let Some(h) = &view.highlight {
        let op = OPS
            .iter()
            .find(|op| op.name == h.source.op && !op.internal())
            .with_context(|| format!("no operation {}", h.source.op))?;
        ensure!(
            op.class == Class::Read,
            "{} is not a read operation: a highlight reads only",
            op.name
        );
    }
    if view.widget == Widget::Form {
        return Ok(());
    }
    if view.widget == Widget::Stack {
        for part in &view.views {
            match part {
                Part::Placed(placed) => {
                    legionctl_core::views::find(&placed.name, None, repo)
                        .with_context(|| format!("stack {} holds {}", view.name, placed.name))?;
                }
                Part::Inline(inline) => check(inline, repo)?,
            }
        }
        return Ok(());
    }
    let source = view.source.as_ref().context("a view needs a source")?;
    let op = OPS
        .iter()
        .find(|op| op.name == source.op && !op.internal())
        .with_context(|| format!("no operation {}", source.op))?;
    ensure!(
        op.class == Class::Read,
        "{} is a {} operation: a view reads only (read operations)",
        op.name,
        format!("{:?}", op.class).to_lowercase()
    );
    Ok(())
}

fn parsed(text: &str, repo: Option<&Path>) -> Result<View> {
    let view = View::parse(text)?;
    check(&view, repo)?;
    Ok(view)
}

/// The scopes of a view's version facts: the user's (global) and the repository's (project).
fn scopes(repo: &Path) -> Vec<(Scope, String)> {
    facts::repository(repo)
        .map(|(root, _)| vec![(Scope::Project, root.to_string_lossy().into_owned())])
        .unwrap_or_default()
}

/// The version of `name` that shows: its fact, else none (the newest file, else the seed).
fn version(name: &str, repo: &Path) -> Result<Option<u32>> {
    let value = dossiers()?.fact_value(&format!("view:{name}"), "version", &scopes(repo))?;
    value
        .map(|v| {
            serde_json::from_value::<u32>(v).context("the version fact of the view is not a number")
        })
        .transpose()
}

pub fn list(input: &ViewsListIn) -> Result<Vec<ViewInfo>> {
    Ok(legionctl_core::views::list(Some(input.repo.get()?))
        .into_iter()
        .map(|(layer, file, view)| {
            let name = file
                .file_stem()
                .map(|s| s.to_string_lossy().trim_start_matches("seed:").to_string())
                .unwrap_or_default();
            let (widget, title, intent, space, error) = match view {
                Ok(v) => (
                    Some(v.widget.into_wire()),
                    Some(v.title()),
                    v.intent.clone(),
                    Some(v.space()),
                    None,
                ),
                Err(e) => (None, None, None, None, Some(format!("{e:#}"))),
            };
            ViewInfo {
                name,
                layer: layer.to_string(),
                file,
                widget,
                title,
                intent,
                space,
                error,
            }
        })
        .collect())
}

pub fn check_text(input: &ViewTextIn) -> Result<Lines> {
    let view = parsed(&input.content.0, None)?;
    Ok(Lines {
        lines: vec![format!(
            "view {} is valid: {}{}",
            view.name,
            view.widget.as_str(),
            view.source
                .as_ref()
                .map_or_else(String::new, |s| format!(" of {}", s.op))
        )],
    })
}

/// A fact's source from the caller.
fn source(ctx: &Ctx) -> String {
    let session = match &ctx.principal {
        Principal::Session(s) => Some(s.as_str()),
        Principal::User => None,
    };
    facts::source(&ctx.actor, session)
}

/// Write the fact that `name` shows `version`, in the scope of the layer (project for the repository's).
fn show_version(
    ctx: &Ctx,
    name: &str,
    version: u32,
    project: Option<&Path>,
    why: &str,
) -> Result<()> {
    let scope = match project.and_then(facts::repository) {
        Some((root, _)) => (Scope::Project, root.to_string_lossy().into_owned()),
        None => (Scope::Global, String::new()),
    };
    dossiers()?.fact_add(&New {
        entity: format!("view:{name}"),
        attribute: "version".into(),
        value: Value::from(version),
        scope,
        source: source(ctx),
        evidence: serde_json::json!({"actor": ctx.actor, "why": why}),
        confidence: 1.0,
        valid_from: facts::now_ms(),
        valid_to: None,
        repo: None,
        from_ref: None,
        to_ref: None,
        anchor: None,
    })?;
    Ok(())
}

pub fn save(ctx: &Ctx, input: &ViewSaveIn) -> Result<Lines> {
    let repo = input.repo.as_ref().map(|r| r.get()).transpose()?;
    let view = parsed(&input.content.0, repo)?;
    let (file, version) = legionctl_core::views::save(&view, &input.content.0, repo)?;
    show_version(ctx, &view.name, version, repo, "views save")?;
    Ok(Lines {
        lines: vec![format!(
            "view {} saved as version {version}: {}",
            view.name,
            file.display()
        )],
    })
}

/// Undo, redo, reset (to the pinned version, else the seed), pin (the version that shows is the one reset goes back
/// to) or a version number.
pub fn version_op(ctx: &Ctx, input: &ViewVersionIn) -> Result<Lines> {
    let repo = input.repo.get()?;
    let name = &input.name;
    let saved = legionctl_core::views::versions(name, Some(repo));
    let seed = legionctl_core::views::seed(name).is_some();
    let all: Vec<u32> = seed
        .then_some(0)
        .into_iter()
        .chain(saved.iter().copied())
        .collect();
    ensure!(!all.is_empty(), "no view {name}");
    let now = version(name, repo)?.unwrap_or_else(|| *all.last().unwrap_or(&0));
    let store = dossiers()?;
    let pinned = store
        .fact_value(&format!("view:{name}"), "pinned", &scopes(repo))?
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok());
    let to = match input.to.as_str() {
        "undo" => *all
            .iter()
            .rev()
            .find(|v| **v < now)
            .with_context(|| format!("view {name} is at its first version ({now})"))?,
        "redo" => *all
            .iter()
            .find(|v| **v > now)
            .with_context(|| format!("view {name} is at its newest version ({now})"))?,
        "reset" => match pinned {
            Some(p) => p,
            None if seed => 0,
            None => bail!("view {name} has no seed and no pinned version to reset to"),
        },
        "pin" => {
            store.fact_add(&New {
                entity: format!("view:{name}"),
                attribute: "pinned".into(),
                value: Value::from(now),
                scope: (Scope::Global, String::new()),
                source: source(ctx),
                evidence: serde_json::json!({"actor": ctx.actor, "why": "views version pin"}),
                confidence: 1.0,
                valid_from: facts::now_ms(),
                valid_to: None,
                repo: None,
                from_ref: None,
                to_ref: None,
                anchor: None,
            })?;
            return Ok(Lines {
                lines: vec![format!(
                    "view {name}: version {now} is pinned; reset goes back to it"
                )],
            });
        }
        number => {
            let n: u32 = number
                .parse()
                .with_context(|| format!("undo, redo, reset, pin or a version, not {number}"))?;
            ensure!(
                all.contains(&n),
                "view {name} has no version {n} (it has {all:?})"
            );
            n
        }
    };
    // the version must still be a valid view
    parsed(
        &legionctl_core::views::text_of(name, Some(to), Some(repo))?,
        Some(repo),
    )?;
    let saved_in = (to > 0
        && legionctl_core::views::dirs(Some(repo))
            .first()
            .is_some_and(|(layer, dir)| {
                *layer == "repository" && dir.join(format!("{name}.jsonc")).exists()
            }))
    .then_some(repo);
    show_version(
        ctx,
        name,
        to,
        saved_in,
        &format!("views version {}", input.to),
    )?;
    Ok(Lines {
        lines: vec![format!("view {name}: version {to} of {all:?}")],
    })
}

/// One render pass: each source answers once (a stack of stats reads `sensors` once).
struct Pass<'a> {
    engine: &'a dyn Engine,
    ctx: &'a Ctx,
    repo: &'a Path,
    answers: BTreeMap<String, Result<Value, String>>,
}

impl Pass<'_> {
    /// The answer of `source` with `input`; every page when the source asks for all.
    fn answer(&mut self, source: &Source, mut input: Value) -> Result<Value> {
        let op = OPS
            .iter()
            .find(|op| op.name == source.op)
            .with_context(|| format!("no operation {}", source.op))?;
        resolve_paths(op, &mut input, self.repo);
        // the same checks as a call from outside: known fields, and a session's own id in its session fields
        crate::server::known_fields(op, &input).map_err(|e| anyhow::anyhow!(e.message))?;
        crate::server::own_session(op, &self.ctx.principal, &mut input);
        let key = format!("{} {input} {}", op.name, source.all);
        if let Some(known) = self.answers.get(&key) {
            return known.clone().map_err(anyhow::Error::msg);
        }
        let call = |input: Value| {
            legionctl_api::dispatch(self.engine, self.ctx, op.name, input)
                .map_err(|e| anyhow::anyhow!(e.api_error(op.name).message))
        };
        let answer = if source.all {
            let mut items = Vec::new();
            let mut page = call(input.clone());
            loop {
                let value = match page {
                    Ok(v) => v,
                    Err(e) => break Err(e),
                };
                items.extend(value["items"].as_array().cloned().unwrap_or_default());
                match value["next"].as_str() {
                    Some(next) => {
                        input["cursor"] = Value::from(next);
                        page = call(input.clone());
                    }
                    None => break Ok(serde_json::json!({ "items": items })),
                }
            }
        } else {
            call(input)
        };
        self.answers.insert(
            key,
            answer
                .as_ref()
                .map(Clone::clone)
                .map_err(|e| format!("{e:#}")),
        );
        answer
    }

    /// `view` with its data: its source's answer, or each view of a stack.
    fn render(&mut self, view: &View, given: &BTreeMap<String, String>, depth: usize) -> Rendered {
        if depth >= DEPTH {
            return Rendered::failed(view, format!("stacks hold stacks more than {DEPTH} deep"));
        }
        if let Err(e) = check(view, Some(self.repo)) {
            return Rendered::failed(view, format!("{e:#}"));
        }
        let values = match view.values(given) {
            Ok(v) => v,
            Err(e) => return Rendered::failed(view, format!("{e:#}")),
        };
        if view.widget == Widget::Stack {
            let mut out = Rendered::of(view, &values, Ok(Value::Null));
            out.error = None;
            for part in &view.views {
                out.parts.push(match part {
                    // an inline part gets the stack's parameters that it declares
                    Part::Inline(inline) => {
                        let inherited = values
                            .iter()
                            .filter(|(k, _)| inline.params.contains_key(*k))
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    v.as_str().map_or_else(|| v.to_string(), String::from),
                                )
                            })
                            .collect();
                        self.render(inline, &inherited, depth + 1)
                    }
                    Part::Placed(placed) => match version(&placed.name, self.repo)
                        .and_then(|v| legionctl_core::views::find(&placed.name, v, Some(self.repo)))
                    {
                        Ok(inner) => {
                            // a placed part's parameters may name the stack's: `{since}`
                            let filled = placed
                                .params
                                .iter()
                                .map(|(k, v)| {
                                    (k.clone(), legionctl_core::views::fill_text(v, &values))
                                })
                                .collect();
                            self.render(&inner, &filled, depth + 1)
                        }
                        Err(e) => {
                            let mut failed = Rendered::failed(view, format!("{e:#}"));
                            failed.name.clone_from(&placed.name);
                            failed.title.clone_from(&placed.name);
                            failed
                        }
                    },
                });
            }
            return out;
        }
        let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), String::from);
        let given: BTreeMap<String, String> =
            values.iter().map(|(k, v)| (k.clone(), text(v))).collect();
        // a parameter that is empty: the part says what it waits for, and reads nothing
        if let Some(waits) = view
            .missing
            .as_ref()
            .filter(|_| view.widget != Widget::Stat && given.values().any(String::is_empty))
        {
            let mut out = Rendered::of(view, &values, Ok(Value::Null));
            out.error = None;
            out.note.clone_from(waits);
            out.params = given;
            return out;
        }
        let answer = match &view.source {
            Some(source) => view
                .input(&given)
                .and_then(|input| self.answer(source, input)),
            None => Ok(Value::Null),
        };
        let mut out = Rendered::of(view, &values, answer);
        out.params.clone_from(&given);
        if let (Some(h), None) = (&view.highlight, &out.error) {
            let input = legionctl_core::views::fill_values(&h.source.input, &values);
            match self
                .answer(&h.source, input)
                .and_then(|a| legionctl_core::views::pick(&a, &h.source.path))
            {
                Ok(items) => {
                    let marks: Vec<String> = items
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|i| {
                            h.fields
                                .iter()
                                .filter_map(|f| i[f].as_str().map(String::from))
                        })
                        .collect();
                    out.mark(&marks, legionctl_core::views::fill_text(&h.label, &values));
                }
                Err(e) => out.legend = format!("no marks: {e:#}"),
            }
        }
        out
    }
}

pub fn render(engine: &dyn Engine, ctx: &Ctx, input: &ViewRenderIn) -> Result<ViewData> {
    let repo = input.repo.get()?;
    let given = params(&input.params)?;
    let shown = version(&input.name, repo)?;
    let view = legionctl_core::views::find(&input.name, shown, Some(repo))?;
    let mut pass = Pass {
        engine,
        ctx,
        repo,
        answers: BTreeMap::new(),
    };
    let mut out = pass.render(&view, &given, 0);
    out.versions = legionctl_core::views::seed(&input.name)
        .map(|_| 0)
        .into_iter()
        .chain(legionctl_core::views::versions(&input.name, Some(repo)))
        .collect();
    out.version = shown.unwrap_or_else(|| out.versions.last().copied().unwrap_or(0));
    Ok(out.into_wire())
}

pub fn place(input: &ViewPlaceIn) -> Result<Lines> {
    let repo = input.repo.get()?;
    let placed = Placed {
        name: input.name.clone(),
        params: params(&input.params)?,
    };
    let view = legionctl_core::views::find(&placed.name, version(&placed.name, repo)?, Some(repo))?;
    if view.widget != Widget::Stack {
        // a placed view must render: its parameters are complete
        view.input(&placed.params)?;
    } else if !placed.params.is_empty() {
        bail!("a stack takes no parameters; its views have their own");
    }
    dossiers()?.thread_place(input.thread, &placed, repo)?;
    Ok(Lines {
        lines: vec![format!(
            "thread {} shows view {} in a tab",
            input.thread, placed.name
        )],
    })
}

/// An intent in one form: lower case, single spaces, no end punctuation.
fn normal(intent: &str) -> String {
    intent
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '?', '!'])
        .to_lowercase()
}

/// The stored view of `intent`: the fact `intent:<intent>` `view`, else a view whose own intent it is.
fn stored(intent: &str, repo: &Path) -> Result<Option<String>> {
    let fact = dossiers()?.fact_value(&format!("intent:{intent}"), "view", &scopes(repo))?;
    if let Some(name) = fact.as_ref().and_then(Value::as_str) {
        return Ok(Some(name.to_string()));
    }
    Ok(legionctl_core::views::list(Some(repo))
        .into_iter()
        .filter_map(|(_, _, v)| v.ok())
        .find(|v| v.intent.as_deref().map(normal).as_deref() == Some(intent))
        .map(|v| v.name))
}

/// rlcd's pick among the stored views (config `views.rlcd`): a choice with each view's intent as its meaning, and
/// "none"; a pick under `views.rlcd_min` is none. A failed call fails the ask (no other route runs).
fn rlcd_pick(
    engine: &dyn Engine,
    ctx: &Ctx,
    intent: &str,
    repo: &Path,
    min: f64,
) -> Result<Option<String>> {
    let clean = |t: &str| t.replace([',', ':', '='], " ");
    let options: Vec<String> = legionctl_core::views::list(Some(repo))
        .into_iter()
        .filter_map(|(_, _, v)| v.ok())
        .map(|v| {
            format!(
                "{}={}",
                v.name,
                clean(&v.intent.clone().unwrap_or_else(|| v.title()))
            )
        })
        .chain(std::iter::once("none=no view answers it".to_string()))
        .collect();
    let question = format!(
        "view:choice:{}:Which stored view answers what the user asks for? none when no view does.",
        options.join(",")
    );
    let decision = engine.rlcd_ask(
        ctx,
        legionctl_api::RlcdAskIn {
            state: legionctl_api::StdinText(format!("The user asks: {intent}")),
            questions: vec![question],
            backend: None,
            key: None,
            repo: legionctl_api::ClientPath(repo.to_path_buf()),
        },
    )?;
    let answer = &decision.answers.unwrap_or_default()["view"];
    let choice = answer["choice"].as_str().unwrap_or("none");
    let p = answer["probabilities"][choice].as_f64().unwrap_or(0.0);
    Ok((choice != "none" && p >= min).then(|| choice.to_string()))
}

/// A JSON schema in a few characters for a model: `string`, `[T]`, `{a: T, b: T}` (with `$defs` of `root`), at most
/// `depth` objects deep.
fn shape(schema: &Value, root: &Value, depth: usize) -> String {
    let schema = match schema["$ref"].as_str() {
        Some(r) => &root["$defs"][r.trim_start_matches("#/$defs/")],
        None => schema,
    };
    if let Some(any) = schema["anyOf"]
        .as_array()
        .or_else(|| schema["oneOf"].as_array())
    {
        let parts: Vec<String> = any
            .iter()
            .filter(|p| p["type"] != "null")
            .map(|p| {
                p["const"]
                    .as_str()
                    .map_or_else(|| shape(p, root, depth), |c| format!("\"{c}\""))
            })
            .collect();
        return parts.join("|");
    }
    if let Some(words) = schema["enum"].as_array() {
        return words
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|");
    }
    let kind = match &schema["type"] {
        Value::String(t) => t.clone(),
        Value::Array(t) => t
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("any")
            .to_string(),
        _ => "any".into(),
    };
    match kind.as_str() {
        "array" => format!("[{}]", shape(&schema["items"], root, depth)),
        "object" if depth == 0 => "{...}".into(),
        "object" => match schema["properties"].as_object() {
            Some(props) => format!(
                "{{{}}}",
                props
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", shape(v, root, depth - 1)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None if schema["additionalProperties"].is_object() => {
                format!(
                    "{{name: {}}}",
                    shape(&schema["additionalProperties"], root, depth - 1)
                )
            }
            None => "{...}".into(),
        },
        "integer" | "number" => "number".into(),
        "boolean" => "bool".into(),
        other => other.into(),
    }
}

/// The words of the view format for a model: each widget, each field with its shape and first sentence (from the
/// view schema, so they never drift), and one small view as an example.
fn format_lines() -> Result<Vec<String>> {
    let (_, schema) = legionctl_core::views::schema()?;
    let first = |v: &Value| {
        let text = v["description"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', " ");
        text.split_once(". ")
            .map_or(text.clone(), |(a, _)| a.to_string())
    };
    let widgets = schema["$defs"]["Widget"]["oneOf"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| {
            format!(
                "- {}: {}",
                w["const"].as_str().unwrap_or_default(),
                first(w)
            )
        });
    let fields = schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| *k != "$schema")
        .map(|(k, v)| format!("- {k}: {}. {}", shape(v, &schema, 2), first(v)));
    let example = r#"Example: {"name": "cpu-heat", "title": "CPU heat", "intent": "how hot is the cpu", "widget": "stack",
 "views": [{"name": "temp", "title": "CPU temperature", "widget": "stat", "source": {"op": "sensors", "path": "$.[name=Package id 0].value"}, "unit": "°C", "warn": 85, "critical": 95},
 {"name": "readings", "title": "Temperatures", "widget": "table", "source": {"op": "sensors"}, "filter": {"unit": "°C"}, "columns": ["name", "value"], "levels": [{"column": "value", "warn": 85, "critical": 95}]}]}"#;
    Ok(std::iter::once("Widgets:".to_string())
        .chain(widgets)
        .chain(std::iter::once(
            "Fields of a view (a stack's parts are views). A path picks one value or one array, never more: $, \
             $.field, $.0 (an index), $.[field=value] (the first item with that value), joined by dots, as in \
             $.0.entities; no [], *, | or filters: a table, list, chart or graph takes the array itself and shows \
             every item."
                .to_string(),
        ))
        .chain(fields)
        .chain(std::iter::once(example.to_string()))
        .collect())
}

/// The operations an intent is likely about: each public operation scored by the intent's words in its name and
/// summary, and the operations of the groups the tab's view reads; the best `n` reads and `m` changes, one line each
/// (MEMORY-14: the operations relevant to the tab, not all of them).
fn relevant(
    intent: &str,
    view: Option<&View>,
    (n, m): (usize, usize),
) -> (
    Vec<&'static legionctl_api::OpInfo>,
    Vec<&'static legionctl_api::OpInfo>,
) {
    const STOP: [&str; 12] = [
        "the", "and", "what", "why", "show", "this", "that", "with", "for", "add", "how", "are",
    ];
    let words: Vec<String> = intent
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !STOP.contains(w))
        .map(str::to_lowercase)
        .collect();
    let mut groups: Vec<String> = Vec::new();
    let mut collect = |v: &View| {
        if let Some(s) = &v.source {
            groups.push(s.op.split('.').next().unwrap_or_default().to_string());
        }
    };
    if let Some(v) = view {
        collect(v);
        for p in &v.views {
            if let Part::Inline(inner) = p {
                collect(inner);
            }
        }
    }
    let mut scored: Vec<(usize, &'static legionctl_api::OpInfo)> = OPS
        .iter()
        .filter(|op| !op.internal())
        .map(|op| {
            let hay = format!("{} {}", op.name, op.summary()).to_lowercase();
            let hits = words.iter().filter(|w| hay.contains(w.as_str())).count();
            let group = usize::from(groups.iter().any(|g| op.name.starts_with(g.as_str())));
            (hits * 2 + group * 3, op)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(b.1.name)));
    let reads = scored
        .iter()
        .filter(|(_, o)| o.class == Class::Read)
        .take(n)
        .map(|(_, o)| *o)
        .collect();
    let changes = scored
        .iter()
        .filter(|(_, o)| o.class != Class::Read)
        .take(m)
        .map(|(_, o)| *o)
        .collect();
    (reads, changes)
}

/// `text` cut to `n` lines, saying how many it left out.
fn sample(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = lines.iter().take(n).copied().collect::<Vec<_>>().join("\n");
    if lines.len() > n {
        let _ = write!(out, "\n({} more lines left out)", lines.len() - n);
    }
    legionctl_core::rlcd::redact(&out, &[])
}

/// The ui-composer's request: the intent, the view to change with its data, the format, the relevant operations
/// and a data sample of the best read (secrets removed).
fn composer_prompt(
    engine: &dyn Engine,
    ctx: &Ctx,
    intent: &str,
    current: Option<(&str, Option<&View>, &str, &str)>,
    repo: &Path,
) -> Result<String> {
    let mut out = vec![
        "You are the ui-composer of legionctl (DESIGN-09): you answer an intent with one view, a declarative spec \
         that legionctl draws in its app and CLI. Answer with one short sentence for the user, then the view as \
         one ```json block. Use only the widgets and fields below; a source is a read operation of the list; a \
         button may name an operation of either list (it runs with its own approval). Keep the view small: the \
         point first, the detail on demand. The view and each part have a title in the user's words (a card's \
         heading; a name is an id). A table's first column names the row (its name, key or id)."
            .to_string(),
        format!("Intent: {intent}"),
    ];
    if let Some((name, _, text, data)) = current {
        out.push(format!(
            "Change this view: keep its name \"{}\" (your answer is its next version).\n```json\n{text}\n```\nIts \
             data now (a sample):\n{}",
            name,
            sample(data, 30)
        ));
    } else {
        out.push(
            "Give the view a new name (a-z, 0-9, -), the intent above as its intent, and a title."
                .into(),
        );
    }
    out.extend(format_lines()?);
    let (reads, changes) = relevant(intent, current.and_then(|c| c.1), (20, 8));
    out.push("Read operations (sources):".into());
    out.extend(reads.iter().map(|op| {
        let fields: Vec<String> = legionctl_api::form::fields(op)
            .into_iter()
            .map(|f| f.name)
            .collect();
        let output = (op.output)();
        format!(
            "- {}({}) -> {}: {}",
            op.name,
            fields.join(", "),
            shape(&output, &output, 2),
            op.summary()
        )
    }));
    out.push("Actions (buttons):".into());
    out.extend(changes.iter().map(|op| {
        format!(
            "- {} [{}]: {}",
            op.name,
            format!("{:?}", op.class).to_lowercase(),
            op.summary()
        )
    }));
    // a data sample of the best read that needs no input
    if let Some(op) = reads
        .iter()
        .find(|op| legionctl_api::form::fields(op).iter().all(|f| !f.required))
    {
        let mut input = Value::Object(serde_json::Map::new());
        resolve_paths(op, &mut input, repo);
        let answer = legionctl_api::dispatch(engine, ctx, op.name, input)
            .map_err(|e| anyhow::anyhow!(e.api_error(op.name).message))
            .and_then(|v| legionctl_api::render(op.name, v).map(|(t, _)| t));
        if let Ok(text) = answer {
            out.push(format!(
                "Data of {} now (a sample):\n{}",
                op.name,
                sample(&text, 25)
            ));
        }
    }
    Ok(out.join("\n"))
}

/// A path field of `op`'s input resolves against `repo`, as a client resolves it against its folder: a given one,
/// and one whose default is the client's folder (".").
fn resolve_paths(op: &legionctl_api::OpInfo, input: &mut Value, repo: &Path) {
    if let Some(map) = input.as_object_mut() {
        let schema = (op.input)();
        for (name, prop) in schema["properties"].as_object().into_iter().flatten() {
            let given = map.get(name).and_then(Value::as_str).map(String::from);
            if prop["format"] == "path" && (given.is_some() || prop["default"] == ".") {
                let path = repo.join(given.unwrap_or_default());
                map.insert(
                    name.clone(),
                    Value::from(path.to_string_lossy().into_owned()),
                );
            }
        }
    }
}

/// A composed view has a title, and so has each part of a stack: a card's heading is words, not an id.
fn titled(view: View) -> Result<View> {
    let mut untitled: Vec<&str> = view
        .views
        .iter()
        .filter_map(|p| match p {
            Part::Inline(v) if v.title.is_none() => Some(v.name.as_str()),
            _ => None,
        })
        .collect();
    if view.title.is_none() {
        untitled.insert(0, &view.name);
    }
    ensure!(
        untitled.is_empty(),
        "each view and each part needs a title in the user's words: {}",
        untitled.join(", ")
    );
    Ok(view)
}

/// The answer's words, and the spec of its last fenced `json` block.
fn split(answer: &str) -> (String, Option<String>) {
    let Some(start) = answer.rfind("```json") else {
        return (answer.trim().to_string(), None);
    };
    let body = &answer[start..];
    let open = body.find('\n').unwrap_or(body.len());
    let Some(close) = body[open..].find("```") else {
        return (answer.trim().to_string(), None);
    };
    let spec = body[open..open + close].trim().to_string();
    let words = format!("{}{}", &answer[..start], &body[open + close + 3..]);
    (words.trim().to_string(), Some(spec))
}

/// A view name that no layer has yet: `name`, else `name-2`, `name-3`, ...
fn unique(name: &str, repo: &Path) -> String {
    let taken: Vec<String> = legionctl_core::views::list(Some(repo))
        .into_iter()
        .filter_map(|(_, f, _)| {
            f.file_stem()
                .map(|s| s.to_string_lossy().trim_start_matches("seed:").to_string())
        })
        .collect();
    // shortcut: at most 999 views of one name; widen the range if someone keeps more
    (1..1000)
        .map(|n| {
            if n == 1 {
                name.to_string()
            } else {
                format!("{name}-{n}")
            }
        })
        .find(|n| !taken.contains(n))
        .unwrap_or_else(|| format!("{name}-{}", taken.len()))
}

/// Save the composer's view: a change as the next version of the tab's view (in its layer), a new view under a name no
/// layer has with the intent as its intent; the version shows, and a new view is the fact of its intent. Its name.
fn keep(
    ctx: &Ctx,
    view: &mut View,
    current: Option<&str>,
    (intent, agent): (&str, &str),
    repo: &Path,
) -> Result<String> {
    // a change is the next version of the tab's view; a new view gets a name no layer has
    let (name, layer) = match current {
        Some(old) => {
            let in_repo = legionctl_core::views::dirs(Some(repo))
                .first()
                .is_some_and(|(l, d)| {
                    *l == "repository" && d.join(format!("{old}.jsonc")).exists()
                });
            (old.to_string(), in_repo.then_some(repo))
        }
        None => (unique(&view.name, repo), None),
    };
    view.name.clone_from(&name);
    // its space follows its sources (View::space), not the model's word
    view.space = None;
    if view.intent.is_none() && current.is_none() {
        view.intent = Some(intent.to_string());
    }
    let text = serde_json::to_string_pretty(&*view)?;
    let (_, saved) = legionctl_core::views::save(view, &text, layer)?;
    show_version(ctx, &name, saved, layer, &format!("ask: {intent}"))?;
    if current.is_none() {
        dossiers()?.fact_add(&New {
            entity: format!("intent:{intent}"),
            attribute: "view".into(),
            value: Value::from(name.clone()),
            scope: (Scope::Global, String::new()),
            source: source(ctx),
            evidence: serde_json::json!({"actor": ctx.actor, "why": "ask", "agent": agent}),
            confidence: 1.0,
            valid_from: facts::now_ms(),
            valid_to: None,
            repo: None,
            from_ref: None,
            to_ref: None,
            anchor: None,
        })?;
    }
    Ok(name)
}

/// The composer's run (`session.run` with its model).
fn composer_run(
    engine: &dyn Engine,
    ctx: &Ctx,
    input: &legionctl_api::AskIn,
    (agent, model): (&str, Option<String>),
    prompt: &str,
    repo: &Path,
) -> Result<legionctl_api::RunOutput> {
    engine.session_run(
        ctx,
        legionctl_api::SessionRunIn {
            agent: agent.to_string(),
            prompt: prompt.to_string(),
            dossier: input.dossier.clone(),
            steal: false,
            repo: legionctl_api::ClientPath(repo.to_path_buf()),
            account: input.account.clone(),
            inside: legionctl_api::Inside::Host,
            gpu: false,
            tag: None,
            acp: input.acp.clone(),
            model,
            thread: None,
        },
    )
}

/// The view that `--view` changes: its name, the view (none when it fails its check now), its text and its data
/// (or why it fails).
type TabView = (String, Option<View>, String, String);

fn tab_view(
    engine: &dyn Engine,
    ctx: &Ctx,
    input: &legionctl_api::AskIn,
    repo: &Path,
) -> Result<Option<TabView>> {
    let Some(name) = &input.view else {
        return Ok(None);
    };
    let text = legionctl_core::views::text_of(name, version(name, repo)?, Some(repo))?;
    // a view that fails its check now is changed too: the composer gets why
    let (view, data) = match View::parse(&text) {
        Ok(view) => {
            let rendered = render(
                engine,
                ctx,
                &ViewRenderIn {
                    name: name.clone(),
                    params: input.params.clone(),
                    repo: legionctl_api::ClientPath(repo.to_path_buf()),
                },
            );
            (
                Some(view),
                rendered.map_or_else(|e| format!("{e:#}"), |d| legionctl_api::Render::text(&d)),
            )
        }
        Err(e) => (None, format!("it fails its check: {e:#}")),
    };
    Ok(Some((name.clone(), view, text, data)))
}

pub fn ask(
    engine: &dyn Engine,
    ctx: &Ctx,
    input: &legionctl_api::AskIn,
) -> Result<legionctl_api::ViewAnswer> {
    let repo = input.repo.get()?;
    let resolved =
        legionctl_core::config::resolve(&legionctl_core::config::Dirs::from_env(), Some(repo))?;
    let cfg = &resolved.config;
    let intent = normal(&input.intent);
    ensure!(!intent.is_empty(), "an intent is the words of what to see");
    let render_view = |name: &str| {
        render(
            engine,
            ctx,
            &ViewRenderIn {
                name: name.to_string(),
                params: input.params.clone(),
                repo: legionctl_api::ClientPath(repo.to_path_buf()),
            },
        )
    };
    let found = |route: &str, name: String| -> Result<legionctl_api::ViewAnswer> {
        Ok(legionctl_api::ViewAnswer {
            route: route.into(),
            data: Some(render_view(&name)?),
            view: Some(name),
            text: String::new(),
            error: None,
            run: None,
            prompt_tokens: 0,
            answer_tokens: 0,
        })
    };
    if input.view.is_none() {
        if let Some(name) = stored(&intent, repo)? {
            return found("stored", name);
        }
        if cfg.views.rlcd
            && let Some(name) = rlcd_pick(engine, ctx, &intent, repo, cfg.views.rlcd_min)?
        {
            return found("rlcd", name);
        }
    }
    ensure!(
        !input.stored,
        "no stored view answers \"{intent}\"; a new view needs an agent run in a dossier (dossier use <name>)"
    );
    let agent = input
        .agent
        .clone()
        .or_else(|| cfg.app.agent.clone())
        .context("no agent composes: give --agent, or set config app.agent")?;
    let current = tab_view(engine, ctx, input, repo)?;
    let prompt = composer_prompt(
        engine,
        ctx,
        &intent,
        current
            .as_ref()
            .map(|(n, v, t, d)| (n.as_str(), v.as_ref(), t.as_str(), d.as_str())),
        repo,
    )?;
    let model = cfg.views.composer_models.get(&agent).cloned();
    let run = composer_run(engine, ctx, input, (&agent, model), &prompt, repo)?;

    let (text, spec) = split(&run.text);
    let mut answer = legionctl_api::ViewAnswer {
        route: "composer".into(),
        view: None,
        text,
        error: None,
        data: None,
        prompt_tokens: legionctl_core::pool::tokens(&prompt),
        answer_tokens: legionctl_core::pool::tokens(&run.text),
        run: Some(run),
    };
    let Some(spec) = spec else {
        return Ok(answer);
    };
    let mut view = match parsed(&spec, Some(repo)).and_then(titled) {
        Ok(v) => v,
        Err(e) => {
            answer.error = Some(format!("{e:#}"));
            return Ok(answer);
        }
    };
    let name = keep(
        ctx,
        &mut view,
        current.as_ref().map(|c| c.0.as_str()),
        (&intent, &agent),
        repo,
    )?;
    answer.data = Some(render_view(&name)?);
    answer.view = Some(name);
    Ok(answer)
}
