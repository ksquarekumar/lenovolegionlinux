//! Dossiers (note 18): an agent's own space for one scope, outside the product's git tree, in
//! `<dossier.root>/<dossier>/` (default `~/agentWorkspaces`; design 23, E1). Each dossier has a stable id in its
//! `dossier.jsonc` and its own database in `.state/dossier.db`; the rows that span dossiers are in the operator's
//! database. A session binds to one dossier at a time and writes only there; reads work on every dossier. The pattern follows the scope-dossier practice of `safecoder dossier`: hash-anchored line edits, numbered
//! artifacts per class folder, SQLite bindings with a stale rule, and a plan ledger with fixed sections.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use blake2::digest::consts::U2;
use blake2::{Blake2b, Digest};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::DossierConfig;
use crate::db::{Db, Schema};

/// The annotations table: one text for the new file and for the v10 step.
macro_rules! annotations {
    () => {
        "
     -- annotations on stable code keys (design 23, A6): the tree and the symbol's content hash when written
     CREATE TABLE annotations (
        id           INTEGER PRIMARY KEY,
        repo         TEXT NOT NULL,
        key          TEXT NOT NULL,
        kind         TEXT NOT NULL,
        value        TEXT NOT NULL,
        tree         TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        session      TEXT NOT NULL,
        agent        TEXT NOT NULL,
        created      INTEGER NOT NULL
     );
     CREATE INDEX annotations_key ON annotations (repo, key);"
    };
}
pub(crate) use annotations;

/// The rows that span dossiers (design 23, E1, decision 1), in the operator's database: the bindings, the workspaces,
/// the sessions and the dossier registry. Rows name a dossier by its current name; a rename changes them in one
/// transaction, and `dossier_names` keeps each old name for the references outside (planning links, events).
macro_rules! shared_tables {
    () => {
        "
     -- one open binding per dossier and per session; released rows stay as history; place: where the session worked
     -- (repository root, else its folder), for binding the next session there
     CREATE TABLE bindings (
        session  TEXT NOT NULL,
        tool     TEXT NOT NULL,
        dossier  TEXT NOT NULL,
        acquired INTEGER NOT NULL,
        updated  INTEGER NOT NULL,
        released INTEGER,
        place    TEXT
     );
     CREATE UNIQUE INDEX open_dossier ON bindings (dossier) WHERE released IS NULL;
     CREATE UNIQUE INDEX open_session ON bindings (session) WHERE released IS NULL;
     -- workspaces, the operating directories of dossiers (one open per dossier, repository and topic; '' is the
     -- dossier's own workspace)
     CREATE TABLE workspaces (
        dossier TEXT NOT NULL,
        repo    TEXT NOT NULL,
        path    TEXT NOT NULL,
        branch  TEXT NOT NULL,
        base    TEXT NOT NULL,
        opened  INTEGER NOT NULL,
        closed  INTEGER,
        topic   TEXT NOT NULL DEFAULT ''
     );
     CREATE UNIQUE INDEX open_workspace ON workspaces (dossier, repo, topic) WHERE closed IS NULL;
     -- sessions that legionctl started (note 14): the account profile (A1); scoped: in its own systemd scope, which
     -- then tells whether it is still live; inside: host (a bwrap sandbox), container or vm (W5)
     CREATE TABLE sessions (
        id      TEXT PRIMARY KEY,
        agent   TEXT NOT NULL,
        dossier TEXT NOT NULL,
        workdir TEXT NOT NULL,
        started INTEGER NOT NULL,
        account TEXT,
        scoped  INTEGER NOT NULL DEFAULT 0,
        inside  TEXT NOT NULL DEFAULT 'host'
     );
     -- the dossier registry (E1): the stable id of each dossier and its folder name in dossier.root; origin: the
     -- folder that the migration or an import moved it from
     CREATE TABLE dossiers (
        id     TEXT PRIMARY KEY,
        name   TEXT NOT NULL UNIQUE,
        origin TEXT
     );
     -- the names a dossier had before a rename, so that old references still find it
     CREATE TABLE dossier_names (
        id    TEXT NOT NULL,
        name  TEXT NOT NULL,
        until INTEGER NOT NULL
     );"
    };
}
pub(crate) use shared_tables;

/// A dossier's own database, `<home>/.state/dossier.db` (design 23, E1): its writes, status history and nudges, and
/// the repository facts (annotations, test runs, mutants, accepts) that its sessions wrote.
pub(crate) const DOSSIER: &Schema = &Schema {
    version: 2,
    steps: &[(1, crate::checks::table!())],
    sql: concat!(
        "CREATE TABLE writes (
        ts      INTEGER NOT NULL,
        session TEXT NOT NULL,
        kind    TEXT NOT NULL,
        path    TEXT NOT NULL
     );
     -- status history of the work graph's nodes, from writes through legionctl (W4)
     CREATE TABLE status_changes (
        ts          INTEGER NOT NULL,
        session     TEXT NOT NULL,
        node        TEXT NOT NULL,
        from_status TEXT,
        to_status   TEXT
     );
     -- dossier nudges: per session, the workspace content at the last reply, the replies that changed it since the
     -- session last wrote this dossier, and the nudge level the agent was last told
     CREATE TABLE nudges (
        session TEXT PRIMARY KEY,
        work    TEXT,
        turns   INTEGER NOT NULL DEFAULT 0,
        level   TEXT
     );",
        annotations!(),
        crate::codetests::tables!(),
        crate::codetests::keyed!(),
        crate::checks::table!()
    ),
};

/// The tables of a dossier's database that hold repository facts: written into the writer's dossier, read as the
/// union of the dossiers attached to the repository (E1 rule 4).
pub(crate) const FACTS: [&str; 5] = [
    "annotations",
    "test_runs",
    "mutants",
    "test_accepts",
    "check_runs",
];

/// The tables of a dossier's database, for the migration's split and count.
pub(crate) const OWN: [&str; 7] = [
    "writes",
    "status_changes",
    "nudges",
    "annotations",
    "test_runs",
    "mutants",
    "test_accepts",
];

/// Where a session works, for binding the next session there: the repository that contains `dir`, else `dir`.
#[must_use]
pub fn place_of(dir: &Path) -> PathBuf {
    // a worktree's place is its repository's main checkout, so a session in a workspace finds the same dossier
    crate::workspace::main_checkout(dir).unwrap_or_else(|_| dir.to_path_buf())
}
/// A dossier's database, in its home.
const DB_FILE: &str = ".state/dossier.db";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The 4-hex-digit anchor of a line: `BLAKE2b` with a 2-byte digest over the line's UTF-8 text (as `safecoder`).
#[must_use]
pub fn line_hash(text: &str) -> String {
    let out = Blake2b::<U2>::digest(text.as_bytes());
    format!("{:02x}{:02x}", out[0], out[1])
}

/// Lines of a file: `\r\n` and `\r` count as line ends; there is no empty last line.
fn lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(String::from)
        .collect()
}

fn hashline(number: usize, text: &str) -> String {
    format!("{number}:{}|{text}", line_hash(text))
}

/// `12:a3f0` -> (12, "a3f0")
fn parse_anchor(anchor: &str) -> Result<(usize, String)> {
    let parsed = anchor.split_once(':').and_then(|(n, h)| {
        let valid = !n.starts_with('0')
            && h.len() == 4
            && h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        Some((
            n.parse::<usize>().ok().filter(|n| *n > 0 && valid)?,
            h.to_string(),
        ))
    });
    parsed.with_context(|| format!("anchor must look like 12:a3f0, got '{anchor}'"))
}

#[must_use]
pub fn valid_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EditMode {
    /// Replace the lines from start to end (inclusive) with the content; empty content deletes them.
    Replace,
    /// Insert the content after the start line.
    InsertAfter,
    /// Delete the lines from start to end.
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Lock {
    pub dossier: String,
    pub session: String,
    pub tool: String,
    /// Seconds since the epoch.
    pub updated: i64,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierInfo {
    pub name: String,
    /// The stable id in its `dossier.jsonc`; it stays when the folder is renamed.
    pub id: Option<String>,
    pub has_plan: bool,
}

pub struct Dossiers {
    pub root: PathBuf,
    /// The operator's database: the rows that span dossiers.
    db: Db,
    config: DossierConfig,
}

impl Dossiers {
    /// The dossiers in `config.root` (`~/` is the home folder), with the rows that span them in the operator's
    /// database.
    ///
    /// # Errors
    /// The dossiers of `~/.agentworkspace` are not moved yet (`legionctl dossier migrate`), or the root or the
    /// operator's database cannot be created.
    pub fn open(config: &DossierConfig) -> Result<Dossiers> {
        if crate::migrate::pending() {
            return Err(crate::error::coded(
                crate::error::Code::Conflict,
                crate::migrate::NOT_MIGRATED,
            ));
        }
        Self::at(config, &crate::db::default_path())
    }

    /// The dossiers in `config.root` with the operator's database at `operator`.
    ///
    /// # Errors
    /// The root or the database cannot be created.
    pub fn at(config: &DossierConfig, operator: &Path) -> Result<Dossiers> {
        let root = crate::config::expand_home(&config.root)?;
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        Ok(Dossiers {
            db: Db::open(operator, crate::db::OPERATOR)?,
            root,
            config: config.clone(),
        })
    }

    pub(crate) fn db(&self) -> &rusqlite::Connection {
        self.db.conn()
    }

    /// The operator's database (its event log and jobs too).
    pub(crate) fn operator(&self) -> &Db {
        &self.db
    }

    /// The database of dossier `name`, made when it has none.
    ///
    /// # Errors
    /// The name is not valid, or the database cannot be opened.
    pub(crate) fn home(&self, name: &str) -> Result<Db> {
        ensure!(valid_name(name), "'{name}' is not a dossier name");
        Db::open(&self.root.join(name).join(DB_FILE), DOSSIER)
    }

    /// The dossiers attached to the repository with the main checkout `checkout`, oldest first: those bound there or
    /// with a workspace there (by their first use), then those whose `entities` name it.
    ///
    /// # Errors
    /// The database read fails.
    pub(crate) fn attached(&self, checkout: &Path) -> Result<Vec<String>> {
        let repo = checkout.to_string_lossy();
        let mut stmt = self.db.conn().prepare(
            "SELECT dossier FROM (SELECT dossier, opened AS since FROM workspaces WHERE repo = ?1
                                  UNION ALL SELECT dossier, acquired FROM bindings WHERE place = ?1)
             GROUP BY dossier ORDER BY min(since), dossier",
        )?;
        let mut out = stmt
            .query_map([&repo], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let mut named: Vec<String> = self
            .list()
            .into_iter()
            .map(|d| d.name)
            .filter(|d| !out.contains(d) && names_entity(&self.root.join(d), &repo))
            .collect();
        named.sort();
        out.extend(named);
        out.retain(|d| self.root.join(d).is_dir());
        Ok(out)
    }

    /// The database that takes the repository facts written in `dir` (E1 rule 4): the dossier of the open workspace
    /// that holds `dir`, else the oldest dossier attached to its repository.
    ///
    /// # Errors
    /// `dir` is not in a git repository, or no dossier is attached to it.
    pub(crate) fn writer(&self, dir: &Path) -> Result<Db> {
        self.home(&self.writer_name(dir)?)
    }

    /// The name of the dossier that [`Dossiers::writer`] opens.
    ///
    /// # Errors
    /// `dir` is not in a git repository, or no dossier is attached to it.
    pub(crate) fn writer_name(&self, dir: &Path) -> Result<String> {
        let here = std::fs::canonicalize(dir)?;
        let own = self
            .workspaces(None)?
            .into_iter()
            .find(|w| std::fs::canonicalize(&w.path).is_ok_and(|p| here.starts_with(p)));
        let dossier = if let Some(w) = own {
            w.dossier
        } else {
            let checkout = crate::workspace::main_checkout(dir)?;
            self.attached(&checkout)?.into_iter().next().with_context(|| {
                format!(
                    "no dossier is attached to {}: use a dossier there (legionctl dossier use), or name the \
                     repository in a dossier's entities",
                    checkout.display()
                )
            })?
        };
        Ok(dossier)
    }

    /// The repository facts of the repository of `dir` (E1 rule 4): the fact tables of each attached dossier, merged
    /// into one database in memory. One dossier is attached at a time, so their count has no limit.
    ///
    /// # Errors
    /// `dir` is not in a git repository, or a dossier's database cannot be read.
    pub(crate) fn facts(&self, dir: &Path) -> Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open_in_memory()?;
        let attached = self.attached(&crate::workspace::main_checkout(dir)?)?;
        if attached.is_empty() {
            conn.execute_batch(DOSSIER.sql)?;
        }
        for (i, dossier) in attached.iter().enumerate() {
            drop(self.home(dossier)?);
            conn.execute(
                "ATTACH DATABASE ?1 AS d",
                [self.root.join(dossier).join(DB_FILE).to_string_lossy()],
            )?;
            for table in FACTS {
                conn.execute_batch(&if i == 0 {
                    format!("CREATE TABLE main.{table} AS SELECT * FROM d.{table}")
                } else {
                    format!("INSERT INTO main.{table} SELECT * FROM d.{table}")
                })?;
            }
            conn.execute_batch("DETACH DATABASE d")?;
        }
        Ok(conn)
    }

    pub(crate) fn stale_after(&self) -> i64 {
        i64::try_from(self.config.stale_hours.saturating_mul(3600)).unwrap_or(i64::MAX)
    }

    /// The dossier this session may write, if any.
    ///
    /// # Errors
    /// The database read fails.
    pub fn active(&self, session: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .conn()
            .query_row(
                "SELECT dossier FROM bindings WHERE session = ?1 AND released IS NULL",
                [session],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Bind `session` to `name`, made from the pattern when it does not exist, releasing its previous dossier. A
    /// dossier held by another live session is refused unless `steal`; a stale binding is taken over.
    ///
    /// # Errors
    /// `name` is not valid, or is held by another session.
    pub fn bind(
        &self,
        session: &str,
        tool: &str,
        name: &str,
        steal: bool,
        place: Option<&Path>,
    ) -> Result<Vec<String>> {
        ensure!(
            valid_name(name),
            "dossier name must be a-z, 0-9 and '-', starting with a letter or digit"
        );
        // the first use in a directory links the dossier home there (E1a); a failed import binds nothing
        let mut out = match place {
            Some(place) => crate::home::link(self, place)?,
            None => Vec::new(),
        };
        let dir = self.root.join(name);
        if !dir.is_dir() {
            out.extend(self.create(name)?);
        }
        // IMMEDIATE: two sessions binding at once see each other's row
        let tx = Transaction::new_unchecked(self.db.conn(), TransactionBehavior::Immediate)?;
        let holder: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT session, tool, updated FROM bindings WHERE dossier = ?1 AND released IS NULL",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let now = now();
        match holder {
            Some((holder, _, _)) if holder == session => {
                tx.execute(
                    "UPDATE bindings SET updated = ?1, place = coalesce(?3, place) WHERE session = ?2 AND released IS NULL",
                    params![now, session, place.map(|p| p.to_string_lossy().into_owned())],
                )?;
                tx.commit()?;
                out.push(format!("active dossier: {name} ({})", dir.display()));
                return Ok(out);
            }
            Some((holder, holder_tool, updated)) => {
                let stale = !self.live(&holder, updated, now);
                if !(stale || steal) {
                    return Err(crate::error::coded(
                        crate::error::Code::Conflict,
                        format!(
                            "dossier '{name}' is in use by session {holder} ({holder_tool}), last active {} min ago; use \
                             another dossier (--dossier <name>; a new name makes one), or take this one over with --steal",
                            (now - updated) / 60
                        ),
                    ));
                }
                tx.execute(
                    "UPDATE bindings SET released = ?1 WHERE dossier = ?2 AND released IS NULL",
                    params![now, name],
                )?;
                out.push(format!(
                    "{name} was held by session {holder} ({})",
                    if stale {
                        "stale"
                    } else {
                        "taken over with steal"
                    }
                ));
            }
            None => {}
        }
        let previous: Option<String> = tx
            .query_row(
                "SELECT dossier FROM bindings WHERE session = ?1 AND released IS NULL",
                [session],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            tx.execute(
                "UPDATE bindings SET released = ?1 WHERE session = ?2 AND released IS NULL",
                params![now, session],
            )?;
            out.insert(0, format!("released {previous}"));
        }
        tx.execute(
            "INSERT INTO bindings (session, tool, dossier, acquired, updated, place) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![session, tool, name, now, place.map(|p| p.to_string_lossy().into_owned())],
        )?;
        tx.commit()?;
        out.push(format!("active dossier: {name} ({})", dir.display()));
        let memory = self.recent_memory(name, 3);
        if !memory.is_empty() {
            out.push("newest memory:".into());
            out.extend(memory.into_iter().map(|m| format!("  {m}")));
        }
        Ok(out)
    }

    /// The dossier the last session in `place` bound, if it still exists.
    ///
    /// # Errors
    /// The database read fails.
    pub fn last_in(&self, place: &Path) -> Result<Option<String>> {
        let last: Option<String> = self
            .db
            .conn()
            .query_row(
                "SELECT dossier FROM bindings WHERE place = ?1 ORDER BY acquired DESC LIMIT 1",
                [place.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(last.filter(|d| self.root.join(d).is_dir()))
    }

    /// # Errors
    /// The database write fails.
    pub fn release(&self, session: &str) -> Result<Option<String>> {
        let active = self.active(session)?;
        self.db.conn().execute(
            "UPDATE bindings SET released = ?1 WHERE session = ?2 AND released IS NULL",
            params![now(), session],
        )?;
        Ok(active)
    }

    /// # Errors
    /// The database read fails.
    pub fn locks(&self) -> Result<Vec<Lock>> {
        let now = now();
        let mut stmt = self
            .db
            .conn()
            .prepare("SELECT dossier, session, tool, updated FROM bindings WHERE released IS NULL ORDER BY dossier")?;
        let rows = stmt.query_map([], |r| {
            Ok(Lock {
                dossier: r.get(0)?,
                session: r.get(1)?,
                tool: r.get(2)?,
                updated: r.get(3)?,
                stale: false,
            })
        })?;
        let mut locks: Vec<Lock> = rows.collect::<Result<_, _>>()?;
        for lock in &mut locks {
            lock.stale = !self.live(&lock.session, lock.updated, now);
        }
        Ok(locks)
    }

    fn live(&self, session: &str, updated: i64, now: i64) -> bool {
        let scoped: Option<(bool, String)> = self
            .db
            .conn()
            .query_row(
                "SELECT scoped, inside FROM sessions WHERE id = ?1",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        live(self.stale_after(), (session, updated, scoped), now)
    }

    /// Count a reply of `session` on `dossier` that ended with the workspace content `work`: the reply counts when
    /// the content changed since the last reply. Returns the replies counted since the session last wrote its
    /// dossier, and the nudge level it was last told.
    ///
    /// # Errors
    /// The database write fails.
    pub fn turn(&self, session: &str, dossier: &str, work: &str) -> Result<(u32, Option<String>)> {
        self.home(dossier)?.conn().execute(
            "INSERT INTO nudges (session, work) VALUES (?1, ?2)
             ON CONFLICT (session) DO UPDATE SET turns = turns + (work IS NOT excluded.work), work = excluded.work",
            params![session, work],
        )?;
        self.nudge_state(session, dossier)
    }

    /// The replies counted since `session` last wrote `dossier`, and the nudge level it was last told.
    ///
    /// # Errors
    /// The database read fails.
    pub fn nudge_state(&self, session: &str, dossier: &str) -> Result<(u32, Option<String>)> {
        Ok(self
            .home(dossier)?
            .conn()
            .query_row(
                "SELECT turns, level FROM nudges WHERE session = ?1",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, None)))
    }

    /// Remember the nudge level `session` was told on `dossier` (none after its answer).
    ///
    /// # Errors
    /// The database write fails.
    pub fn told(&self, session: &str, dossier: &str, level: Option<&str>) -> Result<()> {
        self.home(dossier)?.conn().execute(
            "INSERT INTO nudges (session, level) VALUES (?1, ?2)
             ON CONFLICT (session) DO UPDATE SET level = excluded.level",
            params![session, level],
        )?;
        Ok(())
    }

    /// Mark `session`'s binding as active now (the hooks call it on each prompt and stop).
    ///
    /// # Errors
    /// The database write fails.
    pub fn touch(&self, session: &str) -> Result<()> {
        self.db.conn().execute(
            "UPDATE bindings SET updated = ?1 WHERE session = ?2 AND released IS NULL",
            params![now(), session],
        )?;
        Ok(())
    }

    /// The class folders, `plan.md` with every ledger section, `dossier.jsonc` with a new id and the title, and the
    /// dossier's database; the id goes into the registry.
    fn create(&self, name: &str) -> Result<Vec<String>> {
        let dir = self.root.join(name);
        let mut made = vec![format!("created {}", dir.display())];
        for class in self.config.classes.keys() {
            std::fs::create_dir_all(dir.join(class))?;
        }
        let mut plan = format!("# {name}\n");
        for section in &self.config.plan_sections {
            let _ = write!(plan, "\n## {section}\n");
        }
        atomic_write(&dir.join("plan.md"), &plan)?;
        let id = new_id()?;
        atomic_write(&dir.join("dossier.jsonc"), &jsonc(&id, name)?)?;
        self.home(name)?;
        self.register(&id, name, None)?;
        made.push("plan.md, dossier.jsonc and the class folders".into());
        Ok(made)
    }

    /// Put dossier `id` into the registry as `name` (a known id keeps its row).
    pub(crate) fn register(&self, id: &str, name: &str, origin: Option<&str>) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO dossiers (id, name, origin) VALUES (?1, ?2, ?3) ON CONFLICT (id) DO NOTHING",
            params![id, name, origin],
        )?;
        Ok(())
    }

    /// The registered id and name of the dossier that `origin` was moved or imported from.
    pub(crate) fn by_origin(&self, origin: &Path) -> Result<Option<(String, String)>> {
        Ok(self
            .db
            .conn()
            .query_row(
                "SELECT id, name FROM dossiers WHERE origin = ?1",
                [origin.to_string_lossy()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Whether a dossier has the name `name`: a folder in the root or a registered one.
    pub(crate) fn name_used(&self, name: &str) -> Result<bool> {
        let registered: bool = self.db.conn().query_row(
            "SELECT count(*) > 0 FROM dossiers WHERE name = ?1",
            [name],
            |r| r.get(0),
        )?;
        Ok(registered || std::fs::symlink_metadata(self.root.join(name)).is_ok())
    }

    /// Read the registry from the folders (E1 rule 1): a folder without an id gets one, a folder that another name
    /// had (renamed by hand) takes over its rows. Returns a finding for each folder with the id of another.
    ///
    /// # Errors
    /// A `dossier.jsonc` cannot be read or written, or the database write fails.
    pub fn sync_registry(&self) -> Result<Vec<String>> {
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut findings = Vec::new();
        let mut names: Vec<String> = self.list().into_iter().map(|d| d.name).collect();
        names.sort();
        for name in names {
            let dir = self.root.join(&name);
            let id = if let Some(id) = read_id(&dir)? {
                id
            } else {
                let id = new_id()?;
                write_id(&dir, &id, &name)?;
                id
            };
            if let Some((_, first)) = seen.iter().find(|(i, _)| *i == id) {
                findings.push(format!(
                    "dossier {name} has the id {id} of dossier {first}, so legionctl does not use it: remove \"id\" \
                     from {name}/dossier.jsonc to give it a new one"
                ));
                continue;
            }
            seen.push((id, name));
        }
        let tx = Transaction::new_unchecked(self.db.conn(), TransactionBehavior::Immediate)?;
        for (id, name) in &seen {
            let registered: Option<String> = tx
                .query_row("SELECT name FROM dossiers WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            if registered.as_ref() == Some(name) {
                continue;
            }
            // the dossier that had this name is gone or renamed: its row gives way
            tx.execute(
                "DELETE FROM dossiers WHERE name = ?1 AND id != ?2",
                params![name, id],
            )?;
            match registered {
                Some(old) => rename_rows(&tx, id, &old, name)?,
                None => {
                    tx.execute(
                        "INSERT INTO dossiers (id, name) VALUES (?1, ?2)",
                        params![id, name],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(findings)
    }

    /// Rename dossier `name` to `to`: its folder, the registry, and its bindings, workspaces, sessions and threads;
    /// the id stays. A name in use is refused.
    ///
    /// # Errors
    /// `to` is not valid or in use, `name` does not exist, or the rename fails.
    pub fn rename(&self, name: &str, to: &str) -> Result<Vec<String>> {
        ensure!(
            valid_name(to),
            "dossier name must be a-z, 0-9 and '-', starting with a letter or digit"
        );
        let from = self.root.join(name);
        if !valid_name(name) || !from.is_dir() {
            return Err(crate::error::coded(
                crate::error::Code::NotFound,
                format!("no dossier {name}; see legionctl dossier list"),
            ));
        }
        self.sync_registry()?;
        if self.name_used(to)? {
            return Err(crate::error::coded(
                crate::error::Code::Conflict,
                format!("the dossier name {to} is in use; choose another name"),
            ));
        }
        let id = read_id(&from)?.context("the dossier has no id")?;
        std::fs::rename(&from, self.root.join(to))?;
        let tx = Transaction::new_unchecked(self.db.conn(), TransactionBehavior::Immediate)?;
        rename_rows(&tx, &id, name, to)?;
        tx.commit()?;
        Ok(vec![format!(
            "renamed dossier {name} to {to} (id {id}); its bindings, workspaces, sessions and threads follow"
        )])
    }

    /// The names of `dossier`: its name now and each name it had before.
    ///
    /// # Errors
    /// The database read fails.
    pub fn names_of(&self, dossier: &str) -> Result<Vec<String>> {
        let mut stmt = self.db.conn().prepare(
            "SELECT n.name FROM dossier_names n JOIN dossiers d ON d.id = n.id WHERE d.name = ?1 ORDER BY n.until",
        )?;
        let mut names = vec![dossier.to_string()];
        names.extend(
            stmt.query_map([dossier], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?,
        );
        Ok(names)
    }

    /// Dossiers, most recently changed first.
    #[must_use]
    pub fn list(&self) -> Vec<DossierInfo> {
        let mut dirs: Vec<(SystemTime, DossierInfo)> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir() && valid_name(&e.file_name().to_string_lossy()))
            .map(|e| {
                let modified = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                let info = DossierInfo {
                    name: e.file_name().to_string_lossy().into_owned(),
                    id: read_id(&e.path()).ok().flatten(),
                    has_plan: e.path().join("plan.md").is_file(),
                };
                (modified, info)
            })
            .collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.0));
        dirs.into_iter().map(|(_, d)| d).collect()
    }

    /// `MEMORY/NN-<topic>.md  <title>`, newest first.
    fn recent_memory(&self, name: &str, limit: usize) -> Vec<String> {
        let dir = self.root.join(name).join("MEMORY");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();
        files
            .iter()
            .rev()
            .take(limit)
            .map(|p| {
                let text = std::fs::read_to_string(p).unwrap_or_default();
                let title = text
                    .lines()
                    .find_map(|l| l.strip_prefix("# "))
                    .unwrap_or_default()
                    .to_string();
                format!(
                    "MEMORY/{}  {title}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                )
            })
            .collect()
    }

    /// The absolute path of `rel` (`<dossier>/...`), refusing anything that leaves the dossier.
    fn resolve(&self, rel: &str) -> Result<(String, PathBuf)> {
        let path = Path::new(rel);
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => {
                    let part = part.to_string_lossy();
                    ensure!(
                        !part.starts_with('.'),
                        "{rel}: hidden paths are not dossier files"
                    );
                    parts.push(part.into_owned());
                }
                _ => bail!(
                    "{rel}: use a path relative to the workspace root, such as <dossier>/plan.md"
                ),
            }
        }
        let dossier = parts.first().cloned().context("empty path")?;
        ensure!(
            valid_name(&dossier),
            "{rel}: '{dossier}' is not a dossier name"
        );
        let full = self.root.join(rel);
        // a symlink inside the dossier must not lead out of it
        if let Ok(real) = full.canonicalize() {
            let base = self.root.join(&dossier).canonicalize()?;
            ensure!(real.starts_with(&base), "{rel} leads out of the dossier");
        }
        Ok((dossier, full))
    }

    fn guard_write(&self, session: &str, rel: &str) -> Result<PathBuf> {
        let (dossier, full) = self.resolve(rel)?;
        let active = self.active(session)?;
        if active.as_deref() != Some(&dossier) {
            let holder: Option<String> = self
                .db
                .conn()
                .query_row(
                    "SELECT session FROM bindings WHERE dossier = ?1 AND released IS NULL",
                    [&dossier],
                    |r| r.get(0),
                )
                .optional()?;
            bail!(
                "{dossier} is not this session's active dossier{}. Run `legionctl dossier use {dossier}` first.",
                holder.map_or(String::new(), |h| format!("; it is active in session {h}"))
            );
        }
        self.touch(session)?;
        // a write into its own dossier is the answer to a nudge
        self.home(&dossier)?
            .conn()
            .execute("UPDATE nudges SET turns = 0 WHERE session = ?1", [session])?;
        Ok(full)
    }

    /// Record a write of `rel` (`<dossier>/<file>`) from `old` to `new`, and each node status it changed, in the
    /// dossier's database.
    fn log(&self, session: &str, kind: &str, rel: &str, (old, new): (&str, &str)) -> Result<()> {
        let (dossier, file) = rel.split_once('/').context("not a dossier file")?;
        let db = self.home(dossier)?;
        let conn = db.conn();
        let ts = now();
        conn.execute(
            "INSERT INTO writes (ts, session, kind, path) VALUES (?1, ?2, ?3, ?4)",
            params![ts, session, kind, rel],
        )?;
        for (node, from, to) in crate::graph::status_changes(file, old, new) {
            conn.execute(
                "INSERT INTO status_changes (ts, session, node, from_status, to_status) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![ts, session, node, from, to],
            )?;
        }
        Ok(())
    }

    /// Whether `session` may write the absolute `path`: anything outside the workspace root passes; inside, only
    /// the session's active dossier. Returns the reason for a refusal.
    ///
    /// # Errors
    /// The database read fails.
    pub fn write_refusal(&self, session: &str, path: &Path) -> Result<Option<String>> {
        // through the links: `<repo>/agentWorkspaces/<other dossier>/...` is in the root too
        let (path, root) = (real(path), real(&self.root));
        let Ok(rel) = path.strip_prefix(&root) else {
            return Ok(None);
        };
        let rel = rel.to_string_lossy();
        Ok(self.guard_write(session, &rel).err().map(|e| e.to_string()))
    }

    /// Lines `first..=last` (1-based; `last` defaults to the end) as `<line>:<hash>|<text>`.
    ///
    /// # Errors
    /// The path is not a dossier file or cannot be read.
    pub fn read(&self, rel: &str, first: usize, last: Option<usize>) -> Result<Vec<String>> {
        let (_, full) = self.resolve(rel)?;
        let text = std::fs::read_to_string(&full).with_context(|| format!("reading {rel}"))?;
        Ok(lines(&text)
            .iter()
            .enumerate()
            .map(|(i, l)| (i + 1, l))
            .filter(|(n, _)| *n >= first.max(1) && last.is_none_or(|last| *n <= last))
            .map(|(n, l)| hashline(n, l))
            .collect())
    }

    /// Lines that contain `pattern` (case-insensitive with `ignore_case`) in every dossier, or in `dossier`, as
    /// `<path>:<line>:<hash>|<text>`.
    #[must_use]
    pub fn search(
        &self,
        pattern: &str,
        dossier: Option<&str>,
        ignore_case: bool,
        limit: usize,
    ) -> Vec<String> {
        let needle = if ignore_case {
            pattern.to_lowercase()
        } else {
            pattern.to_string()
        };
        let mut files = Vec::new();
        let dossiers: Vec<String> = match dossier {
            Some(d) => vec![d.to_string()],
            None => self.list().into_iter().map(|d| d.name).collect(),
        };
        for d in dossiers {
            collect_files(&self.root.join(&d), &mut files);
        }
        files.sort();
        let mut out = Vec::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let rel = file
                .strip_prefix(&self.root)
                .unwrap_or(&file)
                .display()
                .to_string();
            for (i, line) in lines(&text).iter().enumerate() {
                let hay = if ignore_case {
                    line.to_lowercase()
                } else {
                    line.clone()
                };
                if hay.contains(&needle) {
                    out.push(format!("{rel}:{}", hashline(i + 1, line)));
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Write a new file (or replace one with `overwrite`) in the session's active dossier.
    ///
    /// # Errors
    /// The path is not in the session's active dossier, exists without `overwrite`, or cannot be written.
    pub fn write(
        &self,
        session: &str,
        rel: &str,
        content: &str,
        overwrite: bool,
    ) -> Result<PathBuf> {
        let full = self.guard_write(session, rel)?;
        if !overwrite && full.exists() {
            return Err(crate::error::coded(
                crate::error::Code::Conflict,
                format!("{rel} exists; use `legionctl dossier edit`, or add --overwrite"),
            ));
        }
        let old = std::fs::read_to_string(&full).unwrap_or_default();
        let new = with_newline(content);
        atomic_write(&full, &new)?;
        self.log(session, "write", rel, (&old, &new))?;
        Ok(full)
    }

    /// # Errors
    /// The path is not in the session's active dossier, does not exist, or cannot be written.
    pub fn append(&self, session: &str, rel: &str, content: &str) -> Result<PathBuf> {
        let full = self.guard_write(session, rel)?;
        let old = std::fs::read_to_string(&full)
            .with_context(|| format!("{rel} does not exist; use write"))?;
        let sep = if old.is_empty() || old.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let new = format!("{old}{sep}{}", with_newline(content));
        atomic_write(&full, &new)?;
        self.log(session, "append", rel, (&old, &new))?;
        Ok(full)
    }

    /// A member's write into another session's dossier (design 23, E6 backfill): one line at the end of `rel`, which
    /// must exist; recorded as `session`'s write. The caller checks the right to write.
    ///
    /// # Errors
    /// The path leaves the dossier, the file does not exist, or it cannot be written.
    pub(crate) fn append_line(&self, session: &str, rel: &str, line: &str) -> Result<()> {
        let (_, full) = self.resolve(rel)?;
        let old = std::fs::read_to_string(&full)
            .with_context(|| format!("{rel} does not exist; a backfill only appends"))?;
        let sep = if old.is_empty() || old.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let new = format!("{old}{sep}{line}\n");
        atomic_write(&full, &new)?;
        self.log(session, "append", rel, (&old, &new))
    }

    /// Remove the last line of `rel` that is exactly `line` (the compensation of a backfill).
    ///
    /// # Errors
    /// The line is no longer in the file, or the file cannot be written.
    pub(crate) fn remove_line(&self, session: &str, rel: &str, line: &str) -> Result<()> {
        let (_, full) = self.resolve(rel)?;
        let old = std::fs::read_to_string(&full)?;
        let mut lines: Vec<&str> = old.lines().collect();
        let at = lines
            .iter()
            .rposition(|l| *l == line)
            .with_context(|| format!("{rel} no longer has the line '{line}'"))?;
        lines.remove(at);
        let new = with_newline(&lines.join("\n"));
        atomic_write(&full, &new)?;
        self.log(session, "edit", rel, (&old, &new))
    }

    /// Change anchored lines. Both anchors must match the current file, so an edit from an outdated read is refused.
    /// Returns the changed lines as hashlines.
    ///
    /// # Errors
    /// The path is not in the session's active dossier, an anchor does not match, or the file cannot be written.
    pub fn edit(
        &self,
        session: &str,
        rel: &str,
        mode: EditMode,
        start: &str,
        end: Option<&str>,
        content: &str,
    ) -> Result<Vec<String>> {
        let full = self.guard_write(session, rel)?;
        let old = std::fs::read_to_string(&full).with_context(|| format!("reading {rel}"))?;
        let mut file = lines(&old);
        let check = |anchor: &str| -> Result<usize> {
            let (line, hash) = parse_anchor(anchor)?;
            ensure!(
                line <= file.len() && line_hash(&file[line - 1]) == hash,
                "anchor {line}:{hash} no longer matches; read the file again"
            );
            Ok(line)
        };
        let start = check(start)?;
        let end = match end {
            Some(end) if mode != EditMode::InsertAfter => check(end)?,
            _ => start,
        };
        ensure!(end >= start, "the end anchor comes before the start anchor");
        let new = lines(content);
        let (from, changed) = match mode {
            EditMode::Replace => {
                file.splice(start - 1..end, new.clone());
                (start, new.len())
            }
            EditMode::InsertAfter => {
                file.splice(start..start, new.clone());
                (start + 1, new.len())
            }
            EditMode::Delete => {
                file.drain(start - 1..end);
                (start, 0)
            }
        };
        let text: String = file.iter().flat_map(|l| [l.as_str(), "\n"]).collect();
        atomic_write(&full, &text)?;
        self.log(session, "edit", rel, (&old, &text))?;
        Ok(file
            .iter()
            .enumerate()
            .skip(from - 1)
            .take(changed)
            .map(|(i, l)| hashline(i + 1, l))
            .collect())
    }

    /// Create the next numbered artifact `<CLASS>[/sub]/NN-<slug>.md` in the session's active dossier. Returns its
    /// path relative to the workspace root.
    ///
    /// # Errors
    /// The class or slug is not valid, the session has no active dossier, or the file cannot be created.
    pub fn add(&self, session: &str, class: &str, slug: &str, content: &str) -> Result<String> {
        let dossier = self.active(session)?.context(
            "this session has no active dossier; run `legionctl dossier use <name>` first",
        )?;
        let top = class.split('/').next().unwrap_or_default();
        ensure!(
            self.config.classes.contains_key(top),
            "unknown class {top}; classes: {}",
            self.config
                .classes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
        ensure!(
            valid_name(slug),
            "slug must be a-z, 0-9 and '-', starting with a letter or digit"
        );
        let folder_rel = format!("{dossier}/{class}");
        let folder = self.guard_write(session, &folder_rel)?;
        std::fs::create_dir_all(&folder)?;
        let mut next = std::fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let (digits, _) = name.split_once('-')?;
                digits.parse::<u32>().ok()
            })
            .max()
            .unwrap_or(0)
            + 1;
        // another agent may take the same number at the same time: create exclusively and move on
        loop {
            let name = format!("{next:02}-{slug}.md");
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(folder.join(&name))
            {
                Ok(mut file) => {
                    file.write_all(with_newline(content).as_bytes())?;
                    let rel = format!("{folder_rel}/{name}");
                    self.log(session, "add", &rel, ("", &with_newline(content)))?;
                    return Ok(rel);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => next += 1,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Ledger findings: a missing `plan.md` or a missing section.
    #[must_use]
    pub fn check(&self, dossier: &str) -> Vec<String> {
        let Ok(plan) = std::fs::read_to_string(self.root.join(dossier).join("plan.md")) else {
            return vec![format!("{dossier}/plan.md is missing")];
        };
        let present: Vec<&str> = plan
            .lines()
            .filter_map(|l| l.trim().strip_prefix("## "))
            .collect();
        let mut findings: Vec<String> = self
            .config
            .plan_sections
            .iter()
            .filter(|s| !present.contains(&s.as_str()))
            .map(|s| format!("plan.md has no '## {s}' section"))
            .collect();
        // code paths of EVIDENCE are relative to the dossier's repository (its open workspace's main checkout)
        let workspace = self
            .workspaces(Some(dossier))
            .ok()
            .and_then(|w| w.into_iter().next());
        let repo = workspace.as_ref().map(|w| w.repo.clone());
        let changed_after = |path: &str, written: std::time::SystemTime| {
            let Some(repo) = &repo else { return false };
            let last = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["log", "-1", "--format=%ct", "--", path])
                .output()
                .ok()
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<u64>()
                        .ok()
                });
            let written = written
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            last.is_some_and(|t| t > written)
        };
        let dir = self.root.join(dossier);
        let graph = crate::graph::build(&dir);
        findings.extend(graph.findings(&dir, &changed_after));
        if let Some(ws) = workspace.filter(|w| !w.is_folder()) {
            findings.extend(crate::planning::row_findings(
                self, dossier, &graph, &ws.path,
            ));
            // the checks of the repository (design 23, E2 rule 2), as the workspace's branch has them
            match self.check_findings(&ws.path) {
                Ok(found) => findings.extend(found),
                Err(e) => findings.push(format!(
                    "the checks of {} do not resolve: {e:#}",
                    ws.path.display()
                )),
            }
        }
        findings
    }

    /// The work graph of `dossier`, or the part around `node` with its status history.
    ///
    /// # Errors
    /// The history cannot be read.
    pub fn graph(&self, dossier: &str, node: Option<&str>) -> Result<crate::graph::Graph> {
        let graph = crate::graph::build(&self.root.join(dossier));
        let Some(id) = node else {
            return Ok(graph);
        };
        let mut around = graph.around(id);
        let db = self.home(dossier)?;
        let mut stmt = db.conn().prepare(
            "SELECT ts, session, node, from_status, to_status FROM status_changes WHERE node = ?1 ORDER BY ts, rowid",
        )?;
        around.history = stmt
            .query_map([id], |r| {
                Ok(crate::graph::StatusChange {
                    ts: r.get(0)?,
                    session: r.get(1)?,
                    node: r.get(2)?,
                    from: r.get(3)?,
                    to: r.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(around)
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if hidden || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_files(&path, out);
        } else if entry.metadata().is_ok_and(|m| m.len() <= 2 << 20) {
            out.push(path);
        }
    }
}

/// Whether the `entities` of the dossier in `dir` name the folder `repo` (a text, or an object with that `path`).
fn names_entity(dir: &Path, repo: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("dossier.jsonc")) else {
        return false;
    };
    let value: serde_json::Value =
        serde_json::from_reader(json_comments::StripComments::new(text.as_bytes()))
            .unwrap_or_default();
    value["entities"].as_array().is_some_and(|list| {
        list.iter()
            .any(|e| e.as_str() == Some(repo) || e["path"].as_str() == Some(repo))
    })
}

/// `path` with each link of its longest existing part resolved.
fn real(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(found) = at.canonicalize() {
            return rest.iter().rev().fold(found, |p, part| p.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Whether the session that holds a binding last refreshed at `updated` is still live: active within `stale_after`
/// seconds (each hook call and dossier write refreshes it), and for a session legionctl started in a scope
/// (`scoped`: the session row's scoped and inside), that scope still runs. A minute of grace covers the time between
/// the bind and the client's `systemd-run`.
pub(crate) fn live(
    stale_after: i64,
    (session, updated, scoped): (&str, i64, Option<(bool, String)>),
    now: i64,
) -> bool {
    if now - updated > stale_after {
        return false;
    }
    match scoped {
        Some((true, inside)) => {
            now - updated <= 60
                || crate::session::active(session, crate::session::Inside::from_db(&inside))
        }
        _ => true,
    }
}

/// A new dossier id: `dsr-` and a ULID (48 bits of milliseconds, 80 random bits, Crockford base32), so ids sort by
/// their time.
pub(crate) fn new_id() -> Result<String> {
    use std::io::Read as _;
    const DIGITS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let mut random = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut random[6..])?;
    let value = ((millis & ((1 << 48) - 1)) << 80) | u128::from_be_bytes(random);
    let ulid: String = (0..26)
        .map(|i| char::from(DIGITS[usize::try_from((value >> (125 - 5 * i)) & 31).unwrap_or(0)]))
        .collect();
    Ok(format!("dsr-{ulid}"))
}

/// The id in `<dir>/dossier.jsonc`, if it has one.
///
/// # Errors
/// The file exists and is not JSON with comments.
pub(crate) fn read_id(dir: &Path) -> Result<Option<String>> {
    let path = dir.join("dossier.jsonc");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_reader(json_comments::StripComments::new(text.as_bytes()))
            .with_context(|| format!("reading {}", path.display()))?;
    Ok(value["id"].as_str().map(String::from))
}

/// A new `dossier.jsonc` with `id` and the title `name`.
fn jsonc(id: &str, name: &str) -> Result<String> {
    Ok(format!(
        "// This dossier's data: its id (stays when the folder is renamed), title, entities (repos, folders) and\n\
         // objectives (Y1, Y2, ...).\n{}\n",
        serde_json::to_string_pretty(
            &serde_json::json!({"id": id, "title": name, "entities": [], "objectives": []})
        )?
    ))
}

/// Put `id` into `<dir>/dossier.jsonc` as its first key, keeping the rest of the text; a missing file is made with
/// the title `name`.
///
/// # Errors
/// The file is not a JSON object, or it cannot be written.
pub(crate) fn write_id(dir: &Path, id: &str, name: &str) -> Result<()> {
    let path = dir.join("dossier.jsonc");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return atomic_write(&path, &jsonc(id, name)?);
    };
    // the comments become spaces of the same length, so a position in the stripped text is one in the file
    let mut stripped = String::new();
    std::io::Read::read_to_string(
        &mut json_comments::StripComments::new(text.as_bytes()),
        &mut stripped,
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&stripped).with_context(|| format!("reading {}", path.display()))?;
    let open = stripped
        .find('{')
        .filter(|_| value.is_object())
        .with_context(|| format!("{} is not a JSON object", path.display()))?;
    let comma = if value.as_object().is_some_and(serde_json::Map::is_empty) {
        ""
    } else {
        ","
    };
    let id = serde_json::to_string(id)?;
    let text = format!(
        "{}\n  \"id\": {id}{comma}{}",
        &text[..=open],
        &text[open + 1..]
    );
    atomic_write(&path, &text)
}

/// Point every row that names dossier `old` at `new`, and keep `old` as a former name of `id`.
fn rename_rows(tx: &Transaction, id: &str, old: &str, new: &str) -> Result<()> {
    tx.execute(
        "UPDATE dossiers SET name = ?2 WHERE id = ?1",
        params![id, new],
    )?;
    tx.execute(
        "INSERT INTO dossier_names (id, name, until) VALUES (?1, ?2, ?3)",
        params![id, old, now()],
    )?;
    for table in ["bindings", "workspaces", "sessions", "threads"] {
        tx.execute(
            &format!("UPDATE {table} SET dossier = ?2 WHERE dossier = ?1"),
            params![old, new],
        )?;
    }
    Ok(())
}

fn with_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

/// Write through a temporary file in the same folder and rename, so a reader never sees half a file.
fn atomic_write(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("no parent folder")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_match_safecoder_and_refuse_outdated_edits() {
        // the hashes safecoder (and Python's blake2b(digest_size=2)) give, so anchors move between the tools
        assert_eq!(line_hash(""), "b1fe");
        assert_eq!(line_hash("hello"), "accd");
        assert_eq!(line_hash("alpha"), "0cf8");

        let dir = crate::scratch::Scratch::new(
            std::env::temp_dir().join(format!("legionctl-dossier-{}", std::process::id())),
        );
        let config: crate::config::Config = crate::config::resolve(
            &crate::config::Dirs {
                system: dir.join("etc"),
                user: dir.join("user"),
            },
            None,
        )
        .unwrap()
        .config;
        let mut dossier = config.dossier;
        dossier.root = dir.join("ws").display().to_string();
        let ws = Dossiers::at(&dossier, &dir.join("operator.db")).unwrap();
        ws.bind("s1", "test", "port", false, None).unwrap();
        ws.write("s1", "port/MEMORY/01-a.md", "alpha\nbeta\ngamma", false)
            .unwrap();
        let read = ws.read("port/MEMORY/01-a.md", 1, None).unwrap();
        assert_eq!(read[0], "1:0cf8|alpha");
        let beta = read[1].split('|').next().unwrap().to_string();
        assert_eq!(
            ws.edit(
                "s1",
                "port/MEMORY/01-a.md",
                EditMode::Replace,
                &beta,
                None,
                "BETA\nbeta2"
            )
            .unwrap()
            .len(),
            2
        );
        let err = ws
            .edit(
                "s1",
                "port/MEMORY/01-a.md",
                EditMode::Delete,
                &beta,
                None,
                "",
            )
            .unwrap_err();
        assert!(err.to_string().contains("no longer matches"), "{err}");

        // a second session cannot write or bind the held dossier; numbers continue per folder
        assert!(ws.write("s2", "port/MEMORY/02-b.md", "x", false).is_err());
        assert!(ws.bind("s2", "test", "port", false, None).is_err());
        assert_eq!(
            ws.add("s1", "MEMORY", "b", "# B").unwrap(),
            "port/MEMORY/02-b.md"
        );
        assert!(
            ws.check("port").is_empty(),
            "a new dossier has every ledger section"
        );
    }
}
