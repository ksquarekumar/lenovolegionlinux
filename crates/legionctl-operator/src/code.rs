//! The `code` operations (design 23, Part A): code graph snapshots per tree, the change graph, annotations and the
//! plan. The work is in `legionctl_core::codegraph` and `legionctl_core::planning`; here are the jobs and the triggers.

use std::path::{Path, PathBuf};

use anyhow::Result;
use legionctl_api::{
    AnnotateIn, Annotation, AnnotationsIn, ChangeSet, CodeNodes, Ctx, GraphChangesIn, GraphHookIn,
    GraphImpactIn, GraphPruneIn, GraphQueryIn, GraphRefreshIn, GraphRefs, HookAction, IntoWire,
    Lines, PlanCheck, PlanCheckIn, PlanStatus, PlanStatusIn, RepoIn,
};
use legionctl_core::db::Db;
use legionctl_core::{codegraph, planning};
use serde_json::{Value, json};

use crate::engine::{dossiers, end_job};

pub const REFRESH: &str = "code.graph.refresh";

/// A `code.graph.refresh` job's work: the graph of its ref, the checkout's link, prune.
pub fn run_refresh(input: &Value) -> Result<Lines> {
    let dir = PathBuf::from(input["repo"].as_str().unwrap_or_default());
    let rev = input["ref"].as_str().unwrap_or("HEAD");
    Ok(Lines {
        lines: codegraph::refresh(&dossiers()?, &dir, rev)?,
    })
}

/// Start a refresh job in the background; its id.
fn spawn(dir: &Path, rev: &str) -> Result<i64> {
    let input = json!({"repo": dir, "ref": rev});
    let job = Db::open_default()?.job_begin(REFRESH, &input)?;
    std::thread::spawn(move || {
        let out = run_refresh(&input);
        if let Err(e) = Db::open_default().and_then(|db| end_job(&db, job, &out)) {
            eprintln!("operator: job {job} not recorded: {e:#}");
        }
    });
    Ok(job)
}

/// A trigger (checkpoint, merge, session start): a background refresh of HEAD when the repository keeps code graphs
/// (a refresh of a tree that has its graph only moves the checkout's link and prunes).
pub fn trigger(dir: &Path) {
    if codegraph::enabled(dir)
        && let Err(e) = spawn(dir, "HEAD")
    {
        eprintln!(
            "operator: no code graph refresh for {}: {e:#}",
            dir.display()
        );
    }
}

pub fn refresh(input: &GraphRefreshIn) -> Result<Lines> {
    let dir = input.repo.get()?;
    if input.background {
        let job = spawn(dir, &input.rev)?;
        return Ok(Lines {
            lines: vec![format!("refresh of {} started: job {job}", input.rev)],
        });
    }
    let db = Db::open_default()?;
    let job_input = json!({"repo": dir, "ref": input.rev});
    let job = db.job_begin(REFRESH, &job_input)?;
    let out = run_refresh(&job_input);
    end_job(&db, job, &out)?;
    out
}

pub fn refs(input: &RepoIn) -> Result<GraphRefs> {
    Ok(codegraph::refs(&dossiers()?, input.repo.get()?)?.into_wire())
}

pub fn query(input: &GraphQueryIn) -> Result<CodeNodes> {
    Ok(codegraph::query(input.repo.get()?, &input.text, &input.rev, input.build)?.into_wire())
}

pub fn impact(input: &GraphImpactIn) -> Result<CodeNodes> {
    Ok(codegraph::impact(
        input.repo.get()?,
        &input.key,
        &input.rev,
        input.depth,
        input.build,
    )?
    .into_wire())
}

pub fn changes(input: &GraphChangesIn) -> Result<Vec<ChangeSet>> {
    let mut sets = codegraph::changes(input.repo.get()?, &input.refs, input.build)?;
    for set in &mut sets {
        set.detail = input.detail;
        if let Some(kind) = &input.kind {
            set.entities.retain(|e| &e.kind == kind);
        }
    }
    Ok(sets.into_wire())
}

pub fn annotate(ctx: &Ctx, input: &AnnotateIn) -> Result<Lines> {
    let id = codegraph::annotate(
        &dossiers()?,
        input.repo.get()?,
        (&input.key, &input.kind, &input.value),
        &input.rev,
        (&input.session.0, &ctx.actor),
    )?;
    Ok(Lines {
        lines: vec![format!("annotation {id} on {} at {}", input.key, input.rev)],
    })
}

pub fn annotations(input: &AnnotationsIn) -> Result<Vec<Annotation>> {
    Ok(codegraph::annotations(
        &dossiers()?,
        input.repo.get()?,
        input.key.as_deref(),
        &input.refs,
    )?
    .into_wire())
}

pub fn prune(input: &GraphPruneIn) -> Result<Lines> {
    Ok(Lines {
        lines: codegraph::prune(&dossiers()?, input.repo.get()?, input.dry_run)?,
    })
}

pub fn hook(input: &GraphHookIn) -> Result<Lines> {
    // the client beside this operator: the hook runs it with the checkout as its working directory
    let legionctl = std::env::current_exe()?.with_file_name("legionctl");
    Ok(Lines {
        lines: codegraph::hook(
            input.repo.get()?,
            &legionctl,
            input.action == HookAction::Install,
        )?,
    })
}

pub fn plan_check(input: &PlanCheckIn) -> Result<PlanCheck> {
    Ok(planning::check(input.repo.get()?, &input.rev, input.build)?.into_wire())
}

pub fn plan_status(input: &PlanStatusIn) -> Result<PlanStatus> {
    Ok(planning::status(
        &dossiers()?,
        input.repo.get()?,
        input.feature.as_deref(),
        &input.rev,
        input.since.as_deref(),
        input.build,
    )?
    .into_wire())
}
