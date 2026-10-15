//! The `views` operations (design 23, C4): the format and the files are in `legionctl_core::views`; here a view's
//! source is checked against the registry (a read operation only, so a view cannot run a change) and called as the
//! caller, with the same input checks as a call from outside.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use legionctl_api::{
    Class, Ctx, Engine, IntoWire, Lines, OPS, ViewData, ViewInfo, ViewPlaceIn, ViewRenderIn,
    ViewSaveIn, ViewTextIn, ViewsListIn,
};
use legionctl_core::views::{Placed, Rendered, View, Widget};

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

/// The checks that need the registry and the other views: a stack's views exist, and a source is a read operation.
fn check(view: &View, repo: Option<&Path>) -> Result<()> {
    if view.widget == Widget::Stack {
        for placed in &view.views {
            legionctl_core::views::find(&placed.name, repo)
                .with_context(|| format!("stack {} holds {}", view.name, placed.name))?;
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

pub fn list(input: &ViewsListIn) -> Result<Vec<ViewInfo>> {
    Ok(legionctl_core::views::list(Some(input.repo.get()?))
        .into_iter()
        .map(|(layer, file, view)| {
            let name = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (widget, error) = match view {
                Ok(v) => (Some(v.widget.into_wire()), None),
                Err(e) => (None, Some(format!("{e:#}"))),
            };
            ViewInfo {
                name,
                layer: layer.to_string(),
                file,
                widget,
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

pub fn save(input: &ViewSaveIn) -> Result<Lines> {
    let repo = input.repo.as_ref().map(|r| r.get()).transpose()?;
    let view = parsed(&input.content.0, repo)?;
    let file = legionctl_core::views::save(&view, &input.content.0, repo)?;
    Ok(Lines {
        lines: vec![format!("view {} saved: {}", view.name, file.display())],
    })
}

/// The view `name` with its data: its source called as `ctx`, or each view of a stack.
fn rendered(
    engine: &dyn Engine,
    ctx: &Ctx,
    placed: &Placed,
    repo: &Path,
    depth: usize,
) -> Result<Rendered> {
    ensure!(depth < DEPTH, "stacks hold stacks more than {DEPTH} deep");
    let view = legionctl_core::views::find(&placed.name, Some(repo))?;
    if let Err(e) = check(&view, Some(repo)) {
        return Ok(Rendered::failed(&view, format!("{e:#}")));
    }
    if view.widget == Widget::Stack {
        let mut out = Rendered::failed(&view, String::new());
        out.error = None;
        for part in &view.views {
            out.parts.push(
                rendered(engine, ctx, part, repo, depth + 1).unwrap_or_else(|e| {
                    let mut failed = Rendered::failed(&view, format!("{e:#}"));
                    failed.name.clone_from(&part.name);
                    failed
                }),
            );
        }
        return Ok(out);
    }
    let answer = view.input(&placed.params).and_then(|mut input| {
        let source = view.source.as_ref().context("a view needs a source")?;
        let op = OPS
            .iter()
            .find(|op| op.name == source.op)
            .with_context(|| format!("no operation {}", source.op))?;
        // the same checks as a call from outside: known fields, and a session's own id in its session fields
        crate::server::known_fields(op, &input).map_err(|e| anyhow::anyhow!(e.message))?;
        crate::server::own_session(op, &ctx.principal, &mut input);
        legionctl_api::dispatch(engine, ctx, op.name, input)
            .map_err(|e| anyhow::anyhow!(e.api_error(op.name).message))
    });
    Ok(Rendered::of(&view, answer))
}

pub fn render(engine: &dyn Engine, ctx: &Ctx, input: &ViewRenderIn) -> Result<ViewData> {
    let placed = Placed {
        name: input.name.clone(),
        params: params(&input.params)?,
    };
    Ok(rendered(engine, ctx, &placed, input.repo.get()?, 0)?.into_wire())
}

pub fn place(input: &ViewPlaceIn) -> Result<Lines> {
    let repo = input.repo.get()?;
    let placed = Placed {
        name: input.name.clone(),
        params: params(&input.params)?,
    };
    let view = legionctl_core::views::find(&placed.name, Some(repo))?;
    if view.widget != Widget::Stack {
        // a placed view must render: its parameters are complete
        view.input(&placed.params)?;
    } else if !placed.params.is_empty() {
        bail!("a stack takes no parameters; its views have their own");
    }
    dossiers()?.thread_place(input.thread, &placed, repo)?;
    Ok(Lines {
        lines: vec![format!(
            "thread {} shows view {} in pane 3",
            input.thread, placed.name
        )],
    })
}
