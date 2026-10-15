//! The one-time move of the dossiers to their homes (design 23, E1): operator job `dossier.migrate`, at each operator
//! start while `~/.agentworkspace` exists. Each step can run again, so a cut-off job does them all again at the next
//! start:
//!
//! 1. Wait until no session holds a live binding (`doctor` says which).
//! 2. Back up `workspace.db` to `~/.local/state/legionctl/backup/workspace.db.<time>`, then take it to its last schema.
//! 3. Give each dossier folder an id and move it to `<dossier.root>/<name>` (rename, else copy with a hash compare).
//! 4. Copy the rows that span dossiers into the operator's database, and each dossier's rows into its `dossier.db`
//!    (a repository row goes by its session's binding, else to the oldest dossier attached to the repository).
//! 5. In each main checkout and open workspace: remove the `.agentworkspace` link and its ignore line (staged), then
//!    link the home there (E1a).
//! 6. Compare the row counts per table, then move what is left of `~/.agentworkspace` to the backup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params_from_iter};
use serde_json::{Value, json};

use crate::config::DossierConfig;
use crate::db::{Db, Schema, now};
use crate::dossier::{Dossiers, OWN, read_id, valid_name, write_id};
use crate::home::{backup_root, copy_verified};

/// The kind of the migration job.
pub const JOB: &str = "dossier.migrate";

/// One row of the old database by column name.
type Row = BTreeMap<String, Sql>;

/// The tables that the migration copies into the operator's database.
const SHARED: [&str; 8] = [
    "bindings",
    "workspaces",
    "sessions",
    "threads",
    "thread_log",
    "members",
    "messages",
    "watch_rules",
];

/// The dossier root before the homes: `~/.agentworkspace`.
///
/// # Errors
/// `HOME` is not set.
pub fn legacy_root() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".agentworkspace"))
}

/// Whether the migration has work to do.
#[must_use]
pub fn pending() -> bool {
    legacy_root().is_ok_and(|r| r.exists())
}

/// What `doctor` says while the migration is pending: the job's wait, its failure, or that it runs.
#[must_use]
pub fn state(ops: &Db) -> String {
    let last = ops
        .jobs_page(0, u32::MAX)
        .map(|(jobs, _)| jobs.into_iter().rev().find(|j| j.kind == JOB));
    match last {
        Ok(Some(job)) if job.state == "failed" => format!(
            "the dossier migration (job {}) failed: {}; it runs again at the next operator start",
            job.id,
            job.error.unwrap_or_default()
        ),
        Ok(Some(job)) => match job.input["waiting"].as_array() {
            Some(live) => format!(
                "the dossier migration (job {}) waits until these sessions end or go stale: {}",
                job.id,
                live.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            None => format!("the dossier migration (job {}) runs", job.id),
        },
        _ => "the dossier migration starts at the next operator start".into(),
    }
}

/// `~/.agentworkspace/workspace.db` at v14, its last version: the steps of v1-v9 merged into one (todo-68), then the topic workspaces, then
/// the code graph annotations, then the test records (design 23, A7), then the threads (design 23, B2, v13), then the kills
/// and accepts keyed by the test's token hash.
const LEGACY: &Schema = &Schema {
    version: 14,
    steps: &[
        (
            9,
            "ALTER TABLE workspaces ADD COLUMN topic TEXT NOT NULL DEFAULT '';
             DROP INDEX open_workspace;
             CREATE UNIQUE INDEX open_workspace ON workspaces (dossier, repo, topic) WHERE closed IS NULL;",
        ),
        (10, crate::dossier::annotations!()),
        (11, crate::codetests::tables!()),
        (12, crate::thread::tables!()),
        (13, crate::codetests::keyed!()),
    ],
    sql: concat!(
        "-- one open binding per dossier and per session; released rows stay as history; place: where the session worked
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
     CREATE TABLE writes (
        ts      INTEGER NOT NULL,
        session TEXT NOT NULL,
        kind    TEXT NOT NULL,
        path    TEXT NOT NULL
     );
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
     -- status history of the work graph's nodes, from writes through legionctl (W4)
     CREATE TABLE status_changes (
        ts          INTEGER NOT NULL,
        session     TEXT NOT NULL,
        dossier     TEXT NOT NULL,
        node        TEXT NOT NULL,
        from_status TEXT,
        to_status   TEXT
     );
     -- dossier nudges: per session, the workspace content at the last reply, the replies that changed it since the
     -- session last wrote its dossier, and the nudge level the agent was last told
     CREATE TABLE nudges (
        session TEXT PRIMARY KEY,
        work    TEXT,
        turns   INTEGER NOT NULL DEFAULT 0,
        level   TEXT
     );",
        crate::dossier::annotations!(),
        crate::codetests::tables!(),
        crate::thread::tables!(),
        crate::codetests::keyed!()
    ),
};

/// Run the migration as job `job` of `ops`; returns the moved dossiers, the row counts and the link lines.
///
/// # Errors
/// A step fails; `~/.agentworkspace` stays, and the next operator start does the steps again.
pub fn run(config: &DossierConfig, ops: &Db, job: i64) -> Result<Value> {
    let legacy = legacy_root()?;
    let old_db = legacy.join("workspace.db");
    let stale = i64::try_from(config.stale_hours.saturating_mul(3600)).unwrap_or(i64::MAX);
    // 1. no live binding
    loop {
        let live = live_bindings(&old_db, stale)?;
        if live.is_empty() {
            break;
        }
        ops.job_step(job, &json!({"waiting": live}))?;
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    // 2. the backup (the first run's: the file as it was, before its schema steps), then the last schema
    let time = now();
    let backed_up = std::fs::read_dir(backup_root()).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("workspace.db."))
    });
    if old_db.exists() && !backed_up {
        let backup = backup_root().join(format!("workspace.db.{time}"));
        std::fs::create_dir_all(backup_root())
            .map_err(anyhow::Error::from)
            .and_then(|()| {
                Connection::open(&old_db)?.execute("VACUUM INTO ?1", [backup.to_string_lossy()])?;
                Ok(())
            })
            .with_context(|| format!("backing up {} to {}", old_db.display(), backup.display()))?;
    }
    if old_db.exists() {
        drop(Db::open(&old_db, LEGACY)?);
    }
    // 3. ids and moves
    let store = Dossiers::at(config, &crate::db::default_path())?;
    let moved = move_folders(&store, &legacy)?;
    // 4. the split
    let (rows, dropped) = if old_db.exists() {
        split(&store, &old_db, &moved)?
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };
    // 5. the links
    let mut lines = Vec::new();
    for dir in places(&store)? {
        lines.extend(relink(&store, &dir)?);
    }
    // 6. the counts, then the old root goes to the backup
    if old_db.exists() {
        compare(&store, &old_db, &moved, &dropped)?;
    }
    let rest = backup_root().join(format!("agentworkspace-{time}"));
    std::fs::create_dir_all(backup_root())
        .and_then(|()| std::fs::rename(&legacy, &rest))
        .with_context(|| format!("moving {} to {}", legacy.display(), rest.display()))?;
    let result = json!({"dossiers": moved, "rows": rows, "dropped": dropped, "lines": lines, "backup": rest});
    ops.record("operator", JOB, &result)?;
    Ok(result)
}

/// `<session> holds <dossier>` for each live binding in the old database.
fn live_bindings(old_db: &Path, stale: i64) -> Result<Vec<String>> {
    if !old_db.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open(old_db)?;
    let mut stmt = conn.prepare(
        "SELECT b.session, b.dossier, b.updated, s.scoped, s.inside FROM bindings b
         LEFT JOIN sessions s ON s.id = b.session WHERE b.released IS NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<bool>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    let now = now();
    let mut live = Vec::new();
    for row in rows {
        let (session, dossier, updated, scoped, inside) = row?;
        let scoped = scoped.zip(inside);
        if crate::dossier::live(stale, (&session, updated, scoped), now) {
            live.push(format!("session {session} holds dossier {dossier}"));
        }
    }
    Ok(live)
}

/// Give each old dossier folder an id and move it to the root; returns the names of every dossier moved now or
/// before (the registry keeps the old folder as its origin).
fn move_folders(store: &Dossiers, legacy: &Path) -> Result<Vec<String>> {
    for entry in std::fs::read_dir(legacy)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let from = entry.path();
        if !from.is_dir() || !valid_name(&name) {
            continue;
        }
        let id = if let Some(id) = read_id(&from)? {
            id
        } else {
            let id = crate::dossier::new_id()?;
            write_id(&from, &id, &name)?;
            id
        };
        let to = store.root.join(&name);
        // a copy of a cut-off run (same id, the source still here) is made again
        if read_id(&to).ok().flatten().as_deref() == Some(id.as_str()) {
            std::fs::remove_dir_all(&to)?;
        }
        ensure!(
            std::fs::symlink_metadata(&to).is_err(),
            "{} exists, so {} cannot move there: rename one of them",
            to.display(),
            from.display()
        );
        // registered before the move, so a cut after the move still knows the folder's origin
        store.register(&id, &name, Some(&from.to_string_lossy()))?;
        match std::fs::rename(&from, &to) {
            Err(e) if e.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) => {
                copy_verified(&from, &to)?;
                std::fs::remove_dir_all(&from)?;
            }
            other => {
                other.with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
            }
        }
    }
    let prefix = format!("{}/", legacy.display());
    let mut stmt = store.db().prepare(
        "SELECT name FROM dossiers WHERE substr(origin, 1, length(?1)) = ?1 ORDER BY name",
    )?;
    let moved = stmt
        .query_map([&prefix], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(moved)
}

/// The column names of `table` in the database `schema` of `conn`.
fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1, ?2)")?;
    let names = stmt
        .query_map([table, schema], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    ensure!(!names.is_empty(), "{schema}.{table} has no columns");
    Ok(names)
}

/// Copy the shared rows into the operator's database, then each dossier's rows into its own; returns the rows
/// placed and the rows with no dossier (of a dossier folder that is gone), per table.
fn split(
    store: &Dossiers,
    old_db: &Path,
    moved: &[String],
) -> Result<(BTreeMap<String, i64>, BTreeMap<String, i64>)> {
    let conn = store.db();
    conn.execute("ATTACH DATABASE ?1 AS old", [old_db.to_string_lossy()])?;
    let copied = (|| -> Result<()> {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        for table in SHARED {
            let cols = columns(&tx, "main", table)?.join(", ");
            tx.execute_batch(&format!(
                "DELETE FROM main.{table}; INSERT INTO main.{table} ({cols}) SELECT {cols} FROM old.{table};"
            ))?;
        }
        tx.commit()?;
        Ok(())
    })();
    conn.execute_batch("DETACH DATABASE old")?;
    copied?;

    let old = Connection::open(old_db)?;
    // a session's dossier: its newest binding
    let mut by_session: BTreeMap<String, String> = BTreeMap::new();
    for row in old
        .prepare("SELECT session, dossier FROM bindings ORDER BY acquired, rowid")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    {
        let (session, dossier) = row?;
        by_session.insert(session, dossier);
    }
    let mut by_repo: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut buckets: BTreeMap<String, BTreeMap<&str, Vec<Row>>> = BTreeMap::new();
    let (mut rows, mut dropped) = (BTreeMap::new(), BTreeMap::new());
    for table in OWN {
        let names = columns(&old, "main", table)?;
        let mut stmt = old.prepare(&format!("SELECT * FROM {table}"))?;
        let mut cursor = stmt.query([])?;
        while let Some(r) = cursor.next()? {
            let mut row = BTreeMap::new();
            for (i, name) in names.iter().enumerate() {
                row.insert(name.clone(), r.get::<_, Sql>(i)?);
            }
            let text = |name: &str| match row.get(name) {
                Some(Sql::Text(t)) => Some(t.clone()),
                _ => None,
            };
            let session = text("session").and_then(|s| by_session.get(&s).cloned());
            let owner = match table {
                "writes" => text("path").and_then(|p| p.split('/').next().map(String::from)),
                "status_changes" => text("dossier"),
                "nudges" => session,
                _ => session.or_else(|| {
                    let repo = text("repo").unwrap_or_default();
                    by_repo
                        .entry(repo.clone())
                        .or_insert_with(|| oldest(store, &repo, moved))
                        .clone()
                }),
            }
            .filter(|d| moved.contains(d));
            match owner {
                Some(d) => buckets
                    .entry(d)
                    .or_default()
                    .entry(table)
                    .or_default()
                    .push(row),
                None => *dropped.entry(table.to_string()).or_insert(0) += 1,
            }
        }
    }
    for dossier in moved {
        let db = store.home(dossier)?;
        let tx = rusqlite::Transaction::new_unchecked(
            db.conn(),
            rusqlite::TransactionBehavior::Immediate,
        )?;
        for table in OWN {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
            let cols = columns(&tx, "main", table)?;
            let marks = vec!["?"; cols.len()].join(", ");
            let mut insert = tx.prepare(&format!(
                "INSERT INTO {table} ({}) VALUES ({marks})",
                cols.join(", ")
            ))?;
            let list = buckets
                .get(dossier)
                .and_then(|b| b.get(table))
                .map_or(&[][..], Vec::as_slice);
            for row in list {
                insert.execute(params_from_iter(
                    cols.iter()
                        .map(|c| row.get(c).cloned().unwrap_or(Sql::Null)),
                ))?;
            }
            *rows.entry(table.to_string()).or_insert(0) += i64::try_from(list.len())?;
        }
        tx.commit()?;
    }
    Ok((rows, dropped))
}

/// The oldest moved dossier attached to the repository `repo` (a main checkout, or a git common folder).
fn oldest(store: &Dossiers, repo: &str, moved: &[String]) -> Option<String> {
    let checkout = repo.strip_suffix("/.git").unwrap_or(repo);
    store
        .attached(Path::new(checkout))
        .ok()?
        .into_iter()
        .find(|d| moved.contains(d))
}

/// The main checkouts and open workspaces of the moved rows.
fn places(store: &Dossiers) -> Result<Vec<Place>> {
    let mut stmt = store.db().prepare(
        "SELECT DISTINCT repo, NULL FROM workspaces
         UNION SELECT path, branch FROM workspaces WHERE closed IS NULL ORDER BY 1",
    )?;
    let rows = stmt.query_map([], |r| {
        let path = PathBuf::from(r.get::<_, String>(0)?);
        // a plain folder's workspace has no branch; the main checkouts come with none (NULL)
        Ok(match r.get::<_, Option<String>>(1)? {
            Some(b) if b.is_empty() => Place::Folder(path),
            _ => Place::Checkout(path),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

enum Place {
    /// A main checkout, a worktree or a plain folder that a dossier used.
    Checkout(PathBuf),
    /// A plain folder's workspace: its old link is in its upper layer.
    Folder(PathBuf),
}

/// Remove the old link and its ignore line in a place, then link the home there.
fn relink(store: &Dossiers, place: &Place) -> Result<Vec<String>> {
    let dir = match place {
        Place::Folder(path) => {
            let _ = std::fs::remove_file(crate::folder::upper(path).join(".agentworkspace"));
            return Ok(Vec::new());
        }
        Place::Checkout(dir) if !dir.is_dir() => {
            return Ok(vec![format!("{} is gone: nothing to link", dir.display())]);
        }
        Place::Checkout(dir) => dir,
    };
    let old = dir.join(".agentworkspace");
    if std::fs::symlink_metadata(&old).is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(&old)?;
    }
    let ignore = dir.join(".gitignore");
    if let Ok(text) = std::fs::read_to_string(&ignore)
        && text.lines().any(|l| l.trim() == "/.agentworkspace")
    {
        let kept: String = text
            .lines()
            .filter(|l| l.trim() != "/.agentworkspace")
            .flat_map(|l| [l, "\n"])
            .collect();
        std::fs::write(&ignore, kept)?;
        crate::workspace::git(dir, &["add", ".gitignore"])?;
    }
    crate::home::link(store, dir)
}

/// Each table's rows in the new places (and the ones with no dossier) against the old database.
fn compare(
    store: &Dossiers,
    old_db: &Path,
    moved: &[String],
    dropped: &BTreeMap<String, i64>,
) -> Result<()> {
    let old = Connection::open(old_db)?;
    let count = |conn: &Connection, table: &str| -> Result<i64> {
        Ok(conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
    };
    for table in SHARED {
        let (was, now) = (count(&old, table)?, count(store.db(), table)?);
        ensure!(
            was == now,
            "{table}: {was} rows in workspace.db, {now} in the operator's database"
        );
    }
    for table in OWN {
        let mut now = dropped.get(table).copied().unwrap_or(0);
        for dossier in moved {
            now += count(store.home(dossier)?.conn(), table)?;
        }
        let was = count(&old, table)?;
        if was != now {
            bail!(
                "{table}: {was} rows in workspace.db, {now} in the dossiers' databases and without a dossier"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A v12 file (before P5's threads) and a v13 file (with them) reach v14: the threads table is there, and the
    /// kills and accepts stay as history without a file and a hash, so no test has them and the gate asks for
    /// evidence again; the kills keep their test names.
    #[test]
    fn v12_and_v13_kills_and_accepts_stay_without_a_key() {
        let dir = crate::scratch::Scratch::new(
            std::env::temp_dir().join(format!("legionctl-ws-v13-{}", std::process::id())),
        );
        std::fs::create_dir_all(&dir).unwrap();
        for version in [12, 13] {
            let path = dir.join(format!("v{version}.db"));
            let old = rusqlite::Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE workspaces (dossier TEXT NOT NULL, repo TEXT NOT NULL, path TEXT NOT NULL,
                    branch TEXT NOT NULL, base TEXT NOT NULL, opened INTEGER NOT NULL, closed INTEGER);
                 CREATE UNIQUE INDEX open_workspace ON workspaces (dossier, repo) WHERE closed IS NULL;",
            )
            .unwrap();
            for (_, sql) in LEGACY.steps.iter().filter(|(from, _)| *from < version) {
                old.execute_batch(sql).unwrap();
            }
            old.execute_batch(&format!(
                "INSERT INTO mutants (repo, ref, file, line, description, killed, source, ts)
                     VALUES ('/r', 'h', 'src/lib.rs', 2, 'm', '[\"a\",\"t::b\"]', 'agent', 1),
                            ('/r', 'h', 'src/lib.rs', 3, 'missed', '[]', 'tool', 1);
                 INSERT INTO test_accepts (repo, test, reason, session, ts) VALUES ('/r', 'a', 'why', 's1', 1);
                 PRAGMA user_version = {version};"
            ))
            .unwrap();
            drop(old);
            let db = Db::open(&path, LEGACY).unwrap();
            assert_eq!(db.schema_version().unwrap(), 14);
            let threads: i64 = db
                .conn()
                .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
                .unwrap();
            assert_eq!(threads, 0, "v{version}: P5's tables are there");
            let killed: Vec<String> = db
                .conn()
                .prepare("SELECT killed FROM mutants ORDER BY id")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(
                killed,
                [
                    r#"[{"test":"a","file":null,"hash":null},{"test":"t::b","file":null,"hash":null}]"#,
                    "[]"
                ],
                "v{version}"
            );
            let accept: (String, Option<String>, Option<String>) = db
                .conn()
                .query_row("SELECT test, file, hash FROM test_accepts", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap();
            assert_eq!(accept, ("a".into(), None, None), "v{version}");
        }
    }

    /// The user's v9 file (one open workspace per dossier and repository) becomes the current version in place: its
    /// rows stay as the dossiers' own workspaces, a topic workspace can open beside one, and the table and index equal
    /// a new file's.
    #[test]
    fn a_v9_workspace_database_takes_topic_workspaces_and_keeps_its_rows() {
        let dir = crate::scratch::Scratch::new(
            std::env::temp_dir().join(format!("legionctl-ws-v9-{}", std::process::id())),
        );
        std::fs::create_dir_all(&dir).unwrap();
        let (old, new) = (dir.join("old.db"), dir.join("new.db"));
        let v9 = rusqlite::Connection::open(&old).unwrap();
        v9.execute_batch(
            "CREATE TABLE workspaces (
                dossier TEXT NOT NULL,
                repo    TEXT NOT NULL,
                path    TEXT NOT NULL,
                branch  TEXT NOT NULL,
                base    TEXT NOT NULL,
                opened  INTEGER NOT NULL,
                closed  INTEGER
             );
             CREATE UNIQUE INDEX open_workspace ON workspaces (dossier, repo) WHERE closed IS NULL;
             INSERT INTO workspaces VALUES ('port', '/r', '/w/r/port', 'legionctl/port', 'main', 1, NULL);
             PRAGMA user_version = 9;",
        )
        .unwrap();
        drop(v9);
        let db = Db::open(&old, LEGACY).unwrap();
        assert_eq!(db.schema_version().unwrap(), LEGACY.version);
        let conn = db.conn();
        assert_eq!(
            conn.query_row(
                "SELECT topic FROM workspaces WHERE path = '/w/r/port'",
                [],
                |r| { r.get::<_, String>(0) }
            )
            .unwrap(),
            ""
        );
        let open = |path: &str, topic: &str| {
            conn.execute(
                "INSERT INTO workspaces (dossier, repo, path, branch, base, opened, topic)
                 VALUES ('port', '/r', ?1, '', 'main', 2, ?2)",
                [path, topic],
            )
        };
        open("/w/r/port--a", "a").unwrap();
        assert!(
            open("/w/r/port-again", "").is_err(),
            "one own workspace per repository"
        );
        assert!(
            open("/w/r/port--a2", "a").is_err(),
            "one workspace per topic"
        );
        let shape = |conn: &rusqlite::Connection| {
            let mut columns: Vec<String> = conn
                .prepare("SELECT name, type, \"notnull\", dflt_value FROM pragma_table_info('workspaces')")
                .unwrap()
                .query_map([], |r| {
                    Ok(format!(
                        "{} {} {} {:?}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<String>>(3)?
                    ))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            columns.push(
                conn.query_row(
                    "SELECT sql FROM sqlite_master WHERE name = 'open_workspace'",
                    [],
                    |r| r.get(0),
                )
                .unwrap(),
            );
            columns
        };
        assert_eq!(shape(conn), shape(Db::open(&new, LEGACY).unwrap().conn()));
    }
}
