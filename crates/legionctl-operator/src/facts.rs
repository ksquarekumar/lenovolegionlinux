//! The `facts` operations (DESIGN-10 T1): the store is `legionctl_core::facts`; here the caller's place becomes the
//! scopes, its refs become commits, and its token becomes the fact's source.

use anyhow::{Context, Result};
use legionctl_api::{
    Ctx, Fact, FactChange, FactSetIn, FactsAtIn, FactsDiffIn, FactsHistoryIn, IntoWire, Lines,
};
use legionctl_core::facts::{self, At, New, Scope};

use crate::engine::dossiers;

/// The scopes of a place besides global: its project and workspace (in git), its dossier and its thread.
fn scopes(
    repo: &std::path::Path,
    dossier: Option<&String>,
    thread: Option<i64>,
) -> Vec<(Scope, String)> {
    let mut out = Vec::new();
    if let Some((root, top)) = facts::repository(repo) {
        out.push((Scope::Project, root.to_string_lossy().into_owned()));
        out.push((Scope::Workspace, top.to_string_lossy().into_owned()));
    }
    if let Some(d) = dossier {
        out.push((Scope::Dossier, d.clone()));
    }
    if let Some(t) = thread {
        out.push((Scope::Thread, t.to_string()));
    }
    out
}

/// A query's point: its times, scopes and commit (the ref in the caller's repository, default HEAD).
fn at(
    repo: &std::path::Path,
    (dossier, thread): (Option<&String>, Option<i64>),
    (time, recorded, rev): (Option<i64>, Option<i64>, Option<&str>),
) -> Result<At> {
    let rev = if let Some((root, _)) = facts::repository(repo) {
        Some((root, facts::commit(repo, rev.unwrap_or("HEAD"))?))
    } else {
        anyhow::ensure!(
            rev.is_none(),
            "{} is not in a repository: no refs",
            repo.display()
        );
        None
    };
    Ok(At {
        time,
        recorded,
        scopes: scopes(repo, dossier, thread),
        rev,
    })
}

pub fn set(ctx: &Ctx, input: &FactSetIn) -> Result<Lines> {
    let session = ctx.principal.agent();
    let source = facts::source(&ctx.actor, session);
    let value: serde_json::Value =
        serde_json::from_str(&input.value).context("the value must be JSON")?;
    let evidence = serde_json::json!({
        "actor": ctx.actor,
        "why": input.evidence,
    });
    let store = dossiers()?;
    if let Some(id) = input.corrects {
        let added = store.fact_correct(id, value, &source, evidence)?;
        return Ok(Lines {
            lines: vec![format!("fact {added} corrects fact {id}")],
        });
    }
    let dir = input.repo.get()?;
    let place = facts::repository(dir);
    let scope: Scope = input.scope.into();
    let scope_id = match scope {
        Scope::Global => String::new(),
        Scope::Project | Scope::Workspace => {
            let (root, top) = place
                .clone()
                .with_context(|| format!("{} is not in a repository", dir.display()))?;
            if scope == Scope::Project { root } else { top }
                .to_string_lossy()
                .into_owned()
        }
        Scope::Dossier => input
            .dossier
            .clone()
            .context("a dossier fact needs --dossier")?,
        Scope::Thread => input
            .thread
            .context("a thread fact needs --thread")?
            .to_string(),
    };
    let bound = input.from_ref.is_some() || input.to_ref.is_some() || input.anchor.is_some();
    let repo = if bound {
        Some(
            place
                .as_ref()
                .with_context(|| format!("{} is not in a repository: no refs", dir.display()))?
                .0
                .clone(),
        )
    } else {
        None
    };
    let resolve = |r: &Option<String>| r.as_deref().map(|r| facts::commit(dir, r)).transpose();
    let from_ref = resolve(&input.from_ref)?;
    let anchor = match &input.anchor {
        Some(key) => {
            let rev = from_ref.as_deref().unwrap_or("HEAD");
            Some((
                key.clone(),
                legionctl_core::codegraph::anchor(dir, rev, key)?.1,
            ))
        }
        None => None,
    };
    let id = store.fact_add(&New {
        entity: input.entity.clone(),
        attribute: input.attribute.clone(),
        value,
        scope: (scope, scope_id),
        source,
        evidence,
        confidence: facts::confidence(input.confidence)?,
        valid_from: input.valid_from.unwrap_or_else(facts::now_ms),
        valid_to: input.valid_to,
        repo,
        from_ref,
        to_ref: resolve(&input.to_ref)?,
        anchor,
    })?;
    Ok(Lines {
        lines: vec![format!(
            "fact {id}: {} {} ({})",
            input.entity,
            input.attribute,
            scope.as_str()
        )],
    })
}

pub fn at_op(input: &FactsAtIn) -> Result<Vec<Fact>> {
    let point = at(
        input.repo.get()?,
        (input.dossier.as_ref(), input.thread),
        (input.time, input.recorded, input.rev.as_deref()),
    )?;
    let mut rows = dossiers()?.facts_at(&point, input.entity.as_deref())?;
    if input.winners {
        rows.retain(|f| f.wins);
    }
    Ok(rows.into_wire())
}

pub fn stale(input: &FactsAtIn) -> Result<Vec<Fact>> {
    let point = at(
        input.repo.get()?,
        (input.dossier.as_ref(), input.thread),
        (input.time, input.recorded, input.rev.as_deref()),
    )?;
    Ok(dossiers()?.facts_stale(&point)?.into_wire())
}

pub fn history(input: &FactsHistoryIn) -> Result<Vec<Fact>> {
    Ok(dossiers()?.facts_history(&input.entity)?.into_wire())
}

pub fn diff(input: &FactsDiffIn) -> Result<Vec<FactChange>> {
    let dir = input.repo.get()?;
    let place = (input.dossier.as_ref(), input.thread);
    let a = at(
        dir,
        place,
        (input.from_time, None, input.from_ref.as_deref()),
    )?;
    let b = at(dir, place, (input.to_time, None, input.to_ref.as_deref()))?;
    Ok(dossiers()?
        .facts_diff(&a, &b, input.entity.as_deref())?
        .into_wire())
}
