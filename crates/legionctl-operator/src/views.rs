//! The `views` operations (design 23 C4, DESIGN-09): the format and the files are in `legionctl_core::views`; here a
//! view's source is checked against the registry (a read operation only, so a view cannot run a change; its buttons
//! name public operations, which run with their own approval) and called as the caller, with the same input checks as
//! a call from outside. Which version of a view shows is the fact `view:<name>` `version` (DESIGN-10).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use legionctl_api::{
    Class, Ctx, Engine, IntoWire, Lines, OPS, Principal, ViewData, ViewInfo, ViewPlaceIn,
    ViewRenderIn, ViewSaveIn, ViewTextIn, ViewVersionIn, ViewsListIn,
};
use legionctl_core::facts::{self, New, Scope};
use legionctl_core::views::{Part, Placed, Rendered, View, Widget};
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
    for action in view.actions.iter().chain(&view.row_actions) {
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
                    v.intent,
                    v.space,
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
    fn answer(&mut self, view: &View, mut input: Value) -> Result<Value> {
        let source = view.source.as_ref().context("a view needs a source")?;
        let op = OPS
            .iter()
            .find(|op| op.name == source.op)
            .with_context(|| format!("no operation {}", source.op))?;
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
                    Part::Inline(inline) => self.render(inline, &BTreeMap::new(), depth + 1),
                    Part::Placed(placed) => match version(&placed.name, self.repo)
                        .and_then(|v| legionctl_core::views::find(&placed.name, v, Some(self.repo)))
                    {
                        Ok(inner) => self.render(&inner, &placed.params, depth + 1),
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
        let answer = view.input(given).and_then(|input| self.answer(view, input));
        Rendered::of(view, &values, answer)
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
