//! The fact store (DESIGN-10 T1): one append-only table of facts on four axes. Valid time: when a fact is true in the
//! world. Recorded time: when the store knew it (a correction closes the old row's recorded time and adds a row, so
//! "what did we believe at T" stays a query). Ref range: the commits where it holds (git ancestry decides). Content
//! anchor: the code graph's stable key and content hash of the entity it is about (valid, stale, absent, as for
//! annotations). Each row has a scope (global, project, dossier, workspace, thread), a source (user, agent:<who>,
//! seed, rule) with its evidence and a confidence. Views and the app's preferences are its first users (DESIGN-09
//! Rule 1). Times are milliseconds since the epoch, UTC.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::params;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dossier::Dossiers;

/// The facts table (operator.db v14).
macro_rules! tables {
    () => {
        "
     -- facts (DESIGN-10 T1): append-only; only recorded_to of an open row is ever set (a correction or a supersede
     -- closes it and adds rows); times in ms; from_ref/to_ref are commits of repo; anchor_key/anchor_hash a code
     -- graph key and its content hash
     CREATE TABLE facts (
        id            INTEGER PRIMARY KEY,
        entity        TEXT NOT NULL,
        attribute     TEXT NOT NULL,
        value         TEXT NOT NULL,
        scope         TEXT NOT NULL,
        scope_id      TEXT NOT NULL,
        source        TEXT NOT NULL,
        evidence      TEXT NOT NULL,
        confidence    REAL NOT NULL,
        valid_from    INTEGER NOT NULL,
        valid_to      INTEGER,
        recorded_from INTEGER NOT NULL,
        recorded_to   INTEGER,
        repo          TEXT,
        from_ref      TEXT,
        to_ref        TEXT,
        anchor_key    TEXT,
        anchor_hash   TEXT,
        corrects      INTEGER
     );
     CREATE INDEX facts_entity ON facts (entity, attribute);"
    };
}
pub(crate) use tables;

/// Where a fact holds; the nearest scope wins: thread, workspace, dossier, project, global.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    Project,
    Dossier,
    Workspace,
    Thread,
}

impl Scope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Project => "project",
            Scope::Dossier => "dossier",
            Scope::Workspace => "workspace",
            Scope::Thread => "thread",
        }
    }

    fn parse(s: &str) -> Result<Scope> {
        Ok(match s {
            "global" => Scope::Global,
            "project" => Scope::Project,
            "dossier" => Scope::Dossier,
            "workspace" => Scope::Workspace,
            "thread" => Scope::Thread,
            other => bail!("no scope {other}"),
        })
    }
}

/// A fact to add.
#[derive(Debug, Clone)]
pub struct New {
    pub entity: String,
    pub attribute: String,
    pub value: Value,
    /// The scope and its id: none for global, the repository root, the dossier, the workspace path, the thread id.
    pub scope: (Scope, String),
    /// user, agent:<who>, seed or rule.
    pub source: String,
    pub evidence: Value,
    pub confidence: f64,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    /// The repository of the ref range and the anchor, with the commits (resolved).
    pub repo: Option<PathBuf>,
    pub from_ref: Option<String>,
    pub to_ref: Option<String>,
    pub anchor: Option<(String, String)>,
}

/// A stored fact; `state` and `wins` are the answer of a query.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Fact {
    pub id: i64,
    pub entity: String,
    pub attribute: String,
    pub value: Value,
    pub scope: Scope,
    pub scope_id: String,
    pub source: String,
    pub evidence: Value,
    pub confidence: f64,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub recorded_from: i64,
    pub recorded_to: Option<i64>,
    pub repo: Option<PathBuf>,
    pub from_ref: Option<String>,
    pub to_ref: Option<String>,
    pub anchor_key: Option<String>,
    pub anchor_hash: Option<String>,
    /// The row this one corrects.
    pub corrects: Option<i64>,
    /// holds; or why not at the query's ref: before (its first commit is not there yet), ended (its last commit is
    /// there), unreachable (its first commit is on no branch any more: a rebase), stale, absent or no graph (its
    /// anchor).
    pub state: String,
    /// The fact that answers for its entity and attribute: the nearest scope, then user over rule over agent over
    /// seed, then the newest.
    pub wins: bool,
}

/// What a query looks at: the times, the scopes that apply, and the ref.
#[derive(Debug, Clone, Default)]
pub struct At {
    /// Valid time and recorded time (ms); now when not given.
    pub time: Option<i64>,
    pub recorded: Option<i64>,
    /// The scopes that apply besides global: (scope, id).
    pub scopes: Vec<(Scope, String)>,
    /// The commit of ref-bound and anchored facts of one repository (its main checkout); the facts of another
    /// repository are looked at at its HEAD.
    pub rev: Option<(PathBuf, String)>,
}

/// One change between two points of a diff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Change {
    pub entity: String,
    pub attribute: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// Milliseconds since the epoch now.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The rank of the user's own facts.
const USER: u8 = 3;

/// The rank of a source: a user fact always wins over an agent fact.
fn rank(source: &str) -> u8 {
    match source.split(':').next().unwrap_or_default() {
        "user" => USER,
        "rule" => 2,
        "seed" => 0,
        _ => 1,
    }
}

const COLUMNS: &str = "id, entity, attribute, value, scope, scope_id, source, evidence, confidence, valid_from, \
     valid_to, recorded_from, recorded_to, repo, from_ref, to_ref, anchor_key, anchor_hash, corrects";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Fact> {
    let json = |i: usize| -> rusqlite::Result<Value> {
        let text: String = r.get(i)?;
        serde_json::from_str(&text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
        })
    };
    let scope: String = r.get(4)?;
    Ok(Fact {
        id: r.get(0)?,
        entity: r.get(1)?,
        attribute: r.get(2)?,
        value: json(3)?,
        scope: Scope::parse(&scope).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, e.into())
        })?,
        scope_id: r.get(5)?,
        source: r.get(6)?,
        evidence: json(7)?,
        confidence: r.get(8)?,
        valid_from: r.get(9)?,
        valid_to: r.get(10)?,
        recorded_from: r.get(11)?,
        recorded_to: r.get(12)?,
        repo: r.get::<_, Option<String>>(13)?.map(PathBuf::from),
        from_ref: r.get(14)?,
        to_ref: r.get(15)?,
        anchor_key: r.get(16)?,
        anchor_hash: r.get(17)?,
        corrects: r.get(18)?,
        state: String::new(),
        wins: false,
    })
}

/// Git ancestry for one query: whether `a` is an ancestor of `b` in `repo`, and whether a commit is on a branch,
/// a remote branch or a tag; each answer once per query.
#[derive(Default)]
struct Ancestry {
    seen: BTreeMap<(PathBuf, String, String), bool>,
}

impl Ancestry {
    fn git_status(repo: &Path, args: &[&str]) -> Result<Option<String>> {
        crate::git::found(repo, args)
    }

    fn ancestor(&mut self, repo: &Path, a: &str, b: &str) -> Result<bool> {
        let key = (repo.to_path_buf(), a.to_string(), b.to_string());
        if let Some(known) = self.seen.get(&key) {
            return Ok(*known);
        }
        let known = Self::git_status(repo, &["merge-base", "--is-ancestor", a, b])?.is_some();
        self.seen.insert(key, known);
        Ok(known)
    }

    /// The commit is still on a branch, a remote branch or a tag (a rebase leaves its old commits on none).
    fn reachable(&mut self, repo: &Path, commit: &str) -> Result<bool> {
        let key = (repo.to_path_buf(), commit.to_string(), String::new());
        if let Some(known) = self.seen.get(&key) {
            return Ok(*known);
        }
        let there = Self::git_status(repo, &["cat-file", "-e", &format!("{commit}^{{commit}}")])?
            .is_some()
            && Self::git_status(
                repo,
                &[
                    "for-each-ref",
                    "--count=1",
                    "--contains",
                    commit,
                    "refs/heads",
                    "refs/remotes",
                    "refs/tags",
                ],
            )?
            .is_some_and(|out| !out.is_empty());
        self.seen.insert(key, there);
        Ok(there)
    }

    /// The state of a ref-bound or anchored fact at `rev`.
    fn state(&mut self, f: &Fact, rev: &str) -> Result<String> {
        let Some(repo) = &f.repo else {
            return Ok("holds".into());
        };
        let commit = crate::git::text(
            repo,
            &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
        )
        .with_context(|| format!("no commit '{rev}' in {}", repo.display()))?;
        if let Some(from) = &f.from_ref {
            if !self.ancestor(repo, from, &commit)? {
                return Ok(if self.reachable(repo, from)? {
                    "before"
                } else {
                    "unreachable"
                }
                .into());
            }
            if let Some(to) = &f.to_ref
                && self.ancestor(repo, to, &commit)?
            {
                return Ok("ended".into());
            }
        }
        if let (Some(key), Some(hash)) = (&f.anchor_key, &f.anchor_hash) {
            let state = crate::codegraph::anchor_state(repo, &commit, key, hash)?;
            if state != "valid" {
                return Ok(state.into());
            }
        }
        Ok("holds".into())
    }
}

impl Dossiers {
    fn facts_where(&self, filter: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Fact>> {
        let sql = format!("SELECT {COLUMNS} FROM facts WHERE {filter} ORDER BY id");
        let mut stmt = self.db().prepare(&sql)?;
        Ok(stmt
            .query_map(args, row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn fact_insert(&self, f: &New, recorded: i64, corrects: Option<i64>) -> Result<i64> {
        self.db().execute(
            "INSERT INTO facts (entity, attribute, value, scope, scope_id, source, evidence, confidence, valid_from,
             valid_to, recorded_from, repo, from_ref, to_ref, anchor_key, anchor_hash, corrects)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                f.entity,
                f.attribute,
                f.value.to_string(),
                f.scope.0.as_str(),
                f.scope.1,
                f.source,
                f.evidence.to_string(),
                f.confidence,
                f.valid_from,
                f.valid_to,
                recorded,
                f.repo.as_ref().map(|r| r.to_string_lossy().into_owned()),
                f.from_ref,
                f.to_ref,
                f.anchor.as_ref().map(|a| &a.0),
                f.anchor.as_ref().map(|a| &a.1),
                corrects
            ],
        )?;
        Ok(self.db().last_insert_rowid())
    }

    /// Add a fact. It supersedes the open facts of the same entity, attribute, scope and source that started before
    /// it: each ends where the new one starts (at its first commit when both are ref-bound, else at its valid time),
    /// as a new row, and the old row's recorded time closes. A fact that comes after a newer one (a late write) ends
    /// where that one starts. Facts of other sources stay: the query ranks them. Returns its id.
    ///
    /// # Errors
    /// The database write fails.
    pub fn fact_add(&self, f: &New) -> Result<i64> {
        let recorded = now_ms();
        // IMMEDIATE: it reads the open facts, then writes; a deferred one fails at once (SQLITE_BUSY) when another
        // writer (a run's output events) commits between its read and its write
        let tx = self.begin()?;
        let open = self.facts_where(
            "entity = ?1 AND attribute = ?2 AND scope = ?3 AND scope_id = ?4 AND source = ?5 \
             AND recorded_to IS NULL AND to_ref IS NULL AND (valid_to IS NULL OR valid_to > ?6)",
            &[
                &f.entity,
                &f.attribute,
                &f.scope.0.as_str(),
                &f.scope.1,
                &f.source,
                &f.valid_from,
            ],
        )?;
        let mut new = f.clone();
        for old in open {
            if old.valid_from > f.valid_from {
                new.valid_to = Some(
                    new.valid_to
                        .map_or(old.valid_from, |t| t.min(old.valid_from)),
                );
                continue;
            }
            self.db().execute(
                "UPDATE facts SET recorded_to = ?2 WHERE id = ?1",
                params![old.id, recorded],
            )?;
            let mut ended = to_new(&old);
            if old.from_ref.is_some() && f.from_ref.is_some() && old.repo == f.repo {
                ended.to_ref.clone_from(&f.from_ref);
            } else {
                ended.valid_to = Some(f.valid_from);
            }
            self.fact_insert(&ended, recorded, Some(old.id))?;
        }
        let id = self.fact_insert(&new, recorded, None)?;
        tx.commit()?;
        Ok(id)
    }

    /// Correct the fact `id` (bitemporal), as its own source or the user: its recorded time closes and a row with the new value and the same valid
    /// times, refs and anchor takes its place, so a query of an earlier recorded time still sees the old value.
    ///
    /// # Errors
    /// No open fact `id`, or the write fails.
    pub fn fact_correct(
        &self,
        id: i64,
        value: Value,
        source: &str,
        evidence: Value,
    ) -> Result<i64> {
        let old = self
            .facts_where("id = ?1 AND recorded_to IS NULL", &[&id])?
            .pop()
            .with_context(|| {
                format!("no fact {id} that is still recorded (`facts history` shows its rows)")
            })?;
        ensure!(
            source == old.source || rank(source) == USER,
            "fact {id} is from {}: only it or the user corrects it; set a new fact instead",
            old.source
        );
        let recorded = now_ms();
        // IMMEDIATE: it reads the open facts, then writes; a deferred one fails at once (SQLITE_BUSY) when another
        // writer (a run's output events) commits between its read and its write
        let tx = self.begin()?;
        self.db().execute(
            "UPDATE facts SET recorded_to = ?2 WHERE id = ?1",
            params![id, recorded],
        )?;
        let mut new = to_new(&old);
        new.value = value;
        new.source = source.to_string();
        new.evidence = evidence;
        let added = self.fact_insert(&new, recorded, Some(id))?;
        tx.commit()?;
        Ok(added)
    }

    /// The facts that hold at `at` (valid and recorded time, scopes, ref), with the winner of each entity and
    /// attribute marked; rows that are in time and scope but not at the ref come too, with their state. `entity` is
    /// an entity, or a prefix with `*` at its end.
    ///
    /// # Errors
    /// A read fails, or a ref does not resolve.
    pub fn facts_at(&self, at: &At, entity: Option<&str>) -> Result<Vec<Fact>> {
        let time = at.time.unwrap_or_else(now_ms);
        let recorded = at.recorded.unwrap_or_else(now_ms);
        let (filter, pattern) = entity_filter(entity);
        let mut rows = self.facts_where(
            &format!(
                "{filter} AND recorded_from <= ?2 AND (recorded_to IS NULL OR recorded_to > ?2) \
                 AND valid_from <= ?3 AND (valid_to IS NULL OR valid_to > ?3)"
            ),
            &[&pattern, &recorded, &time],
        )?;
        rows.retain(|f| {
            f.scope == Scope::Global
                || at
                    .scopes
                    .iter()
                    .any(|(s, id)| *s == f.scope && *id == f.scope_id)
        });
        let mut ancestry = Ancestry::default();
        for f in &mut rows {
            let rev = match &at.rev {
                Some((repo, commit)) if f.repo.as_ref() == Some(repo) => commit.as_str(),
                _ => "HEAD",
            };
            f.state = ancestry.state(f, rev)?;
        }
        let mut best: BTreeMap<(String, String), usize> = BTreeMap::new();
        for (i, f) in rows.iter().enumerate().filter(|(_, f)| f.state == "holds") {
            let key = (f.entity.clone(), f.attribute.clone());
            let better = best.get(&key).is_none_or(|&j| {
                let o = &rows[j];
                // a user fact wins over every other; among the rest the nearest scope, then the source
                let key = |f: &Fact| {
                    (
                        rank(&f.source) == USER,
                        f.scope,
                        rank(&f.source),
                        f.valid_from,
                        f.id,
                    )
                };
                key(f) > key(o)
            });
            if better {
                best.insert(key, i);
            }
        }
        for i in best.into_values() {
            rows[i].wins = true;
        }
        Ok(rows)
    }

    /// Every row of `entity` (or a prefix with `*`) on all axes, oldest first.
    ///
    /// # Errors
    /// A read fails.
    pub fn facts_history(&self, entity: &str) -> Result<Vec<Fact>> {
        let (filter, pattern) = entity_filter(Some(entity));
        self.facts_where(&filter, &[&pattern])
    }

    /// What the winners of `entity` changed from `a` to `b`.
    ///
    /// # Errors
    /// A read fails, or a ref does not resolve.
    pub fn facts_diff(&self, a: &At, b: &At, entity: Option<&str>) -> Result<Vec<Change>> {
        let winners = |at: &At| -> Result<BTreeMap<(String, String), Value>> {
            Ok(self
                .facts_at(at, entity)?
                .into_iter()
                .filter(|f| f.wins)
                .map(|f| ((f.entity, f.attribute), f.value))
                .collect())
        };
        let (before, after) = (winners(a)?, winners(b)?);
        let mut keys: Vec<&(String, String)> = before.keys().chain(after.keys()).collect();
        keys.sort();
        keys.dedup();
        Ok(keys
            .into_iter()
            .filter(|k| before.get(*k) != after.get(*k))
            .map(|k| Change {
                entity: k.0.clone(),
                attribute: k.1.clone(),
                before: before.get(k).cloned(),
                after: after.get(k).cloned(),
            })
            .collect())
    }

    /// The anchored facts in time and scope whose anchor is not valid at `at`'s ref: stale (other content) or absent.
    ///
    /// # Errors
    /// A read fails, or a ref does not resolve.
    pub fn facts_stale(&self, at: &At) -> Result<Vec<Fact>> {
        let mut rows = self.facts_at(at, None)?;
        rows.retain(|f| f.anchor_key.is_some() && matches!(f.state.as_str(), "stale" | "absent"));
        Ok(rows)
    }

    /// The winning value of `entity`'s `attribute` now in `scopes`, when one holds.
    ///
    /// # Errors
    /// A read fails.
    pub fn fact_value(
        &self,
        entity: &str,
        attribute: &str,
        scopes: &[(Scope, String)],
    ) -> Result<Option<Value>> {
        let at = At {
            scopes: scopes.to_vec(),
            ..At::default()
        };
        Ok(self
            .facts_at(&at, Some(entity))?
            .into_iter()
            .find(|f| f.wins && f.attribute == attribute)
            .map(|f| f.value))
    }
}

/// The SQL filter of an entity or a prefix (`view:*`), and its parameter.
fn entity_filter(entity: Option<&str>) -> (String, String) {
    match entity {
        None => ("?1 = ?1".into(), String::new()),
        Some(e) => match e.strip_suffix('*') {
            Some(prefix) => (
                "substr(entity, 1, length(?1)) = ?1".into(),
                prefix.to_string(),
            ),
            None => ("entity = ?1".into(), e.to_string()),
        },
    }
}

fn to_new(f: &Fact) -> New {
    New {
        entity: f.entity.clone(),
        attribute: f.attribute.clone(),
        value: f.value.clone(),
        scope: (f.scope, f.scope_id.clone()),
        source: f.source.clone(),
        evidence: f.evidence.clone(),
        confidence: f.confidence,
        valid_from: f.valid_from,
        valid_to: f.valid_to,
        repo: f.repo.clone(),
        from_ref: f.from_ref.clone(),
        to_ref: f.to_ref.clone(),
        anchor: f.anchor_key.clone().zip(f.anchor_hash.clone()),
    }
}

/// A fact's source from the caller: the user on the CLI and app; an agent through MCP (`agent:mcp`) or in a
/// session (`agent:<session>`).
#[must_use]
pub fn source(actor: &str, session: Option<&str>) -> String {
    match (session, actor) {
        (Some(s), _) => format!("agent:{s}"),
        (None, "cli" | "app") => "user".into(),
        (None, other) => format!("agent:{other}"),
    }
}

/// Check a confidence: 0 to 1.
///
/// # Errors
/// It is outside 0 to 1.
pub fn confidence(c: f64) -> Result<f64> {
    ensure!((0.0..=1.0).contains(&c), "a confidence is 0 to 1: {c}");
    Ok(c)
}

/// The repository root (main checkout) and the worktree of `dir`; none outside git.
#[must_use]
pub fn repository(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let root = crate::workspace::main_checkout(dir).ok()?;
    let top = crate::git::text(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    Some((root, PathBuf::from(top)))
}

/// The commit of `rev` in `dir`.
///
/// # Errors
/// It does not resolve.
pub fn commit(dir: &Path, rev: &str) -> Result<String> {
    ensure!(!rev.starts_with('-'), "a ref must not start with '-'");
    crate::git::text(
        dir,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
    .with_context(|| format!("no commit '{rev}' in {}", dir.display()))
}
