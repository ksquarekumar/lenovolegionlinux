//! The operator database: one SQLite file in WAL mode, shared by every legionctl process of this user.
//!
//! - Bootstrap: `open` applies pending schema migrations in order, each in its own transaction, tracked by
//!   `PRAGMA user_version`. Installs, upgrades and plain starts all go through the same path.
//! - Concurrency: WAL lets readers run while one writer commits; a busy timeout makes concurrent hooks wait
//!   instead of failing.
//! - Event log: every state change appends a row to `events` (who, what, payload). The daemon (Phase 1b) turns this
//!   into the shared log that consumers follow; see docs/design/13-daemon.md.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use std::fmt::Write as _;

use rusqlite::{Connection, OptionalExtension, params};

/// A database's schema: the tables of `version`, made at once in a new file, and the steps that bring a file of an
/// older version to it.
pub struct Schema {
    pub version: i64,
    pub sql: &'static str,
    /// `(from, sql)`: the SQL that takes a file at version `from` to `from + 1`.
    pub steps: &'static [(i64, &'static str)],
}

/// The operator database at v10 (the steps of v1-v10 merged into one; todo-68, after the first full release install).
const OPERATOR: &Schema = &Schema {
    version: 10,
    steps: &[],
    sql: "CREATE TABLE events (
        id      INTEGER PRIMARY KEY,
        ts      INTEGER NOT NULL,
        actor   TEXT NOT NULL,
        kind    TEXT NOT NULL,
        payload TEXT NOT NULL
     );
     -- pages of the log by kind (DESIGN/03): `kind = ? AND id > ?` uses this
     CREATE INDEX events_kind ON events (kind, id);
     -- the model gateway: one usage row per call, with its cost from gateway.prices (note 19)
     CREATE TABLE gateway_usage (
        id         INTEGER PRIMARY KEY,
        ts         INTEGER NOT NULL,
        account    TEXT NOT NULL,
        model      TEXT NOT NULL,
        tokens_in  INTEGER NOT NULL,
        tokens_out INTEGER NOT NULL,
        cached     INTEGER NOT NULL,
        status     INTEGER NOT NULL,
        ms         INTEGER NOT NULL,
        cost_usd   REAL NOT NULL DEFAULT 0
     );
     -- session tokens (DESIGN/03): the hash of each sandbox's token and its session; the user token is a file
     CREATE TABLE tokens (
        hash    TEXT PRIMARY KEY,
        session TEXT NOT NULL,
        created INTEGER NOT NULL
     );
     -- jobs, the long operations (DESIGN/02): what ran, its input, and how it ended; the operator resumes the ones a
     -- restart cut off
     CREATE TABLE jobs (
        id       INTEGER PRIMARY KEY,
        kind     TEXT NOT NULL,
        input    TEXT NOT NULL,
        state    TEXT NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 1,
        created  INTEGER NOT NULL,
        updated  INTEGER NOT NULL,
        result   TEXT,
        error    TEXT
     );",
};

pub struct Db {
    conn: Connection,
}

pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let state =
        std::env::var_os("XDG_STATE_HOME").map_or_else(|| home.join(".local/state"), PathBuf::from);
    state.join("legionctl/operator.db")
}

/// BLAKE2b-256 of a token, as hex.
#[must_use]
pub fn token_hash(token: &str) -> String {
    use blake2::{Blake2b, Digest, digest::consts::U32};
    format!("{:x}", Blake2b::<U32>::digest(token.as_bytes()))
}

/// A long operation and how it ended (`state`: running, offloaded, done, failed).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Job {
    pub id: i64,
    /// The operation: `session.run`, `images.build`.
    pub kind: String,
    pub input: serde_json::Value,
    pub state: String,
    /// 1, and one more each time the operator resumed it after a restart.
    pub attempts: u32,
    pub created: i64,
    pub updated: i64,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

const JOB_COLUMNS: &str = "id, kind, input, state, attempts, created, updated, result, error";

fn job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let json = |s: Option<String>| s.and_then(|s| serde_json::from_str(&s).ok());
    Ok(Job {
        id: r.get(0)?,
        kind: r.get(1)?,
        input: json(Some(r.get(2)?)).unwrap_or_default(),
        state: r.get(3)?,
        attempts: r.get(4)?,
        created: r.get(5)?,
        updated: r.get(6)?,
        result: json(r.get(7)?),
        error: r.get(8)?,
    })
}

impl Db {
    /// A new running job; its id.
    ///
    /// # Errors
    /// The database write fails.
    pub fn job_begin(&self, kind: &str, input: &serde_json::Value) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO jobs (kind, input, state, created, updated) VALUES (?1, ?2, 'running', ?3, ?3)",
            params![kind, input.to_string(), now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The job ended: its result, or why it failed.
    ///
    /// # Errors
    /// The database write fails.
    pub fn job_end(&self, id: i64, outcome: &Result<serde_json::Value, String>) -> Result<()> {
        let (state, result, error) = match outcome {
            Ok(value) => ("done", Some(value.to_string()), None),
            Err(e) => ("failed", None, Some(e.clone())),
        };
        self.conn.execute(
            "UPDATE jobs SET state = ?2, result = ?3, error = ?4, updated = ?5 WHERE id = ?1 AND state = 'running'",
            params![id, state, result, error, now()],
        )?;
        Ok(())
    }

    /// The job's input, as it is now (a headless run adds its ACP session id, to load it after a restart).
    ///
    /// # Errors
    /// The database write fails.
    pub fn job_input(&self, id: i64, input: &serde_json::Value) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET input = ?2, updated = ?3 WHERE id = ?1",
            params![id, input.to_string(), now()],
        )?;
        Ok(())
    }

    /// A running job made a step: its input as it is now, and its count of restarts again from 1 (a series that
    /// restarts the machine at each step is not cut off). False when the job does not run (any more).
    ///
    /// # Errors
    /// The database write fails.
    pub fn job_step(&self, id: i64, input: &serde_json::Value) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE jobs SET input = ?2, attempts = 1, updated = ?3 WHERE id = ?1 AND state = 'running'",
            params![id, input.to_string(), now()],
        )? == 1)
    }

    /// The job stopped to free memory (`offloaded`); the session policy runs it again when memory eases.
    ///
    /// # Errors
    /// The database write fails.
    pub fn job_offload(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET state = 'offloaded', updated = ?2 WHERE id = ?1 AND state = 'running'",
            params![id, now()],
        )?;
        Ok(())
    }

    /// Run the job once more (after a restart cut it off, or an offload).
    ///
    /// # Errors
    /// The database write fails.
    pub fn job_again(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET attempts = attempts + 1, state = 'running', updated = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
        Ok(())
    }

    /// The jobs in `state`, oldest first.
    ///
    /// # Errors
    /// The database read fails.
    pub fn jobs_in(&self, state: &str) -> Result<Vec<Job>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE state = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([state], job_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One job.
    ///
    /// # Errors
    /// The database read fails.
    pub fn job(&self, id: i64) -> Result<Option<Job>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
                [id],
                job_row,
            )
            .optional()?)
    }

    /// Jobs after the id `after`, oldest first, at most `limit`; and the id to go on after when more follow.
    ///
    /// # Errors
    /// The database read fails.
    pub fn jobs_page(&self, after: i64, limit: u32) -> Result<(Vec<Job>, Option<i64>)> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE id > ?1 ORDER BY id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![after, i64::from(limit) + 1], job_row)?;
        let rows = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(cut(rows, limit, |job| job.id))
    }
}

/// Which events a page holds.
#[derive(Debug, Default)]
pub struct EventFilter<'a> {
    /// Only these kinds; every kind when empty.
    pub kinds: &'a [&'a str],
    /// Only events whose payload has this text value at this JSON path (`$.session`).
    pub field: Option<(&'a str, &'a str)>,
    /// Only events whose payload contains this text (`*` matches any text, for per-CPU files).
    pub naming: Option<&'a str>,
}

/// The first `limit` rows, and the id of the last one when the query found more (it asks for one more).
fn cut<T>(mut rows: Vec<T>, limit: u32, id: impl Fn(&T) -> i64) -> (Vec<T>, Option<i64>) {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if rows.len() > limit {
        rows.truncate(limit);
        let next = rows.last().map(&id);
        (rows, next)
    } else {
        (rows, None)
    }
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl Db {
    /// Open the database at `path`, made with `schema` when there is none.
    ///
    /// # Errors
    /// The folder or database cannot be created or opened, or the file has another schema version.
    pub fn open(path: &Path, schema: &Schema) -> Result<Db> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        if !path.exists() {
            make(path, schema)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < schema.version {
            version = upgrade(&conn, schema)?;
        }
        anyhow::ensure!(
            version == schema.version,
            "{} has schema v{version}, and this legionctl reads only v{}: it is from another release. Move the \
             file away to start with an empty one (its data is not read), or install the release that made it",
            path.display(),
            schema.version
        );
        Ok(Db { conn })
    }

    /// # Errors
    /// As [`Db::open`].
    pub fn open_default() -> Result<Db> {
        Self::open(&default_path(), OPERATOR)
    }

    /// A new token for `session`, which replaces its earlier one; only its hash is kept.
    ///
    /// # Errors
    /// The database write fails.
    pub fn issue_token(&self, session: &str, token: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tokens WHERE session = ?1", [session])?;
        self.conn.execute(
            "INSERT INTO tokens (hash, session, created) VALUES (?1, ?2, ?3)",
            params![token_hash(token), session, now()],
        )?;
        Ok(())
    }

    /// The session whose sandbox holds `token`, if any.
    ///
    /// # Errors
    /// The database read fails.
    pub fn token_session(&self, token: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT session FROM tokens WHERE hash = ?1",
                [token_hash(token)],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// # Errors
    /// The database read fails.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// Append to the event log. `actor` says who caused it (cli, mcp, hook, daemon).
    ///
    /// # Errors
    /// The database write fails.
    pub fn record(&self, actor: &str, kind: &str, payload: &serde_json::Value) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO events (ts, actor, kind, payload) VALUES (?1, ?2, ?3, ?4)",
            params![now(), actor, kind, payload.to_string()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The id of the newest event; 0 when there is none.
    ///
    /// # Errors
    /// The database read fails.
    pub fn last_event_id(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT coalesce(max(id), 0) FROM events", [], |r| r.get(0))?)
    }

    /// One usage row for a gateway call.
    ///
    /// # Errors
    /// The database write fails.
    pub fn gateway_usage_add(&self, row: &GatewayUsage) -> Result<()> {
        self.conn.execute(
            "INSERT INTO gateway_usage (ts, account, model, tokens_in, tokens_out, cached, status, ms, cost_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![now(), row.account, row.model, row.tokens_in, row.tokens_out, row.cached, row.status, row.ms, row.cost_usd],
        )?;
        Ok(())
    }

    /// Usage rows after the row id `after`, oldest first, at most `limit`; and the id to go on after when more
    /// follow.
    ///
    /// # Errors
    /// The database read fails.
    pub fn gateway_usage_page(
        &self,
        after: i64,
        limit: u32,
    ) -> Result<(Vec<GatewayUsage>, Option<i64>)> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, account, model, tokens_in, tokens_out, cached, status, ms, cost_usd FROM gateway_usage
             WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after, i64::from(limit) + 1], |r| {
            Ok(GatewayUsage {
                id: r.get(0)?,
                ts: r.get(1)?,
                account: r.get(2)?,
                model: r.get(3)?,
                tokens_in: r.get(4)?,
                tokens_out: r.get(5)?,
                cached: r.get(6)?,
                status: r.get(7)?,
                ms: r.get(8)?,
                cost_usd: r.get(9)?,
            })
        })?;
        let rows = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(cut(rows, limit, |row| row.id))
    }

    /// USD that `account` spent on `model` in the last `seconds`, replays not counted.
    ///
    /// # Errors
    /// The database read fails.
    pub fn gateway_spent(&self, account: &str, model: &str, seconds: i64) -> Result<f64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM gateway_usage
             WHERE account = ?1 AND model = ?2 AND cached = 0 AND ts > ?3",
            params![account, model, now() - seconds],
            |r| r.get(0),
        )?)
    }

    /// The events after the id `after` that `filter` lets through, oldest first, at most `limit`; and the id to go
    /// on after when more follow.
    ///
    /// # Errors
    /// The database read fails.
    pub fn events_page(
        &self,
        filter: &EventFilter,
        after: i64,
        limit: u32,
    ) -> Result<(Vec<EventRow>, Option<i64>)> {
        self.events_page_in(filter, after, limit, "")
    }

    fn events_page_in(
        &self,
        filter: &EventFilter,
        after: i64,
        limit: u32,
        order: &str,
    ) -> Result<(Vec<EventRow>, Option<i64>)> {
        let mut sql = String::from("SELECT id, ts, actor, kind, payload FROM events WHERE id > ?");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(after)];
        if !filter.kinds.is_empty() {
            let marks = vec!["?"; filter.kinds.len()].join(", ");
            let _ = write!(sql, " AND kind IN ({marks})");
            args.extend(
                filter
                    .kinds
                    .iter()
                    .map(|k| Box::new(k.to_string()) as Box<dyn rusqlite::ToSql>),
            );
        }
        if let Some((path, value)) = filter.field {
            sql.push_str(" AND json_extract(payload, ?) = ?");
            args.push(Box::new(path.to_string()));
            args.push(Box::new(value.to_string()));
        }
        if let Some(text) = filter.naming {
            let escaped = text
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            sql.push_str(" AND payload LIKE ? ESCAPE '\\'");
            args.push(Box::new(format!("%{}%", escaped.replace('*', "%"))));
        }
        let _ = write!(sql, " ORDER BY id {order} LIMIT ?");
        args.push(Box::new(i64::from(limit) + 1));
        let rows = self.query_events(
            &sql,
            rusqlite::params_from_iter(args.iter().map(AsRef::as_ref)),
        )?;
        Ok(cut(rows, limit, |row| row.id))
    }

    /// The newest event that `filter` lets through.
    ///
    /// # Errors
    /// The database read fails.
    pub fn last_event(&self, filter: &EventFilter) -> Result<Option<EventRow>> {
        let (rows, _) = self.events_page_in(filter, 0, 1, "DESC")?;
        Ok(rows.into_iter().next())
    }

    /// The newest `limit` events that `filter` lets through, oldest first.
    ///
    /// # Errors
    /// The database read fails.
    pub fn events_newest(&self, filter: &EventFilter, limit: u32) -> Result<Vec<EventRow>> {
        let (mut rows, _) = self.events_page_in(filter, 0, limit, "DESC")?;
        rows.reverse();
        Ok(rows)
    }

    /// For each session (`$.input.session` of a call's audit), its newest event of these kinds since `ts`.
    ///
    /// # Errors
    /// The database read fails.
    pub fn newest_per_session(&self, kinds: &[&str], ts: i64) -> Result<Vec<EventRow>> {
        let marks = vec!["?"; kinds.len()].join(", ");
        let sql = format!(
            "SELECT id, ts, actor, kind, payload FROM events WHERE id IN (SELECT max(id) FROM events
             WHERE kind IN ({marks}) AND ts >= ? GROUP BY json_extract(payload, '$.input.session')) ORDER BY id"
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = kinds
            .iter()
            .map(|k| Box::new((*k).to_string()) as Box<dyn rusqlite::ToSql>)
            .collect();
        args.push(Box::new(ts));
        self.query_events(
            &sql,
            rusqlite::params_from_iter(args.iter().map(AsRef::as_ref)),
        )
    }

    /// The event with this id.
    ///
    /// # Errors
    /// The database read fails.
    pub fn event(&self, id: i64) -> Result<Option<EventRow>> {
        Ok(self
            .query_events(
                "SELECT id, ts, actor, kind, payload FROM events WHERE id = ?1",
                params![id],
            )?
            .pop())
    }

    /// Delete events and gateway usage older than `days` (0 keeps everything).
    ///
    /// # Errors
    /// The database write fails.
    pub fn prune(&self, days: u32) -> Result<usize> {
        if days == 0 {
            return Ok(0);
        }
        let before = now() - i64::from(days) * 86_400;
        Ok(self
            .conn
            .execute("DELETE FROM events WHERE ts < ?1", params![before])?
            + self
                .conn
                .execute("DELETE FROM gateway_usage WHERE ts < ?1", params![before])?)
    }

    /// Events from `ts` (seconds since the epoch) on, oldest first.
    ///
    /// # Errors
    /// The database read fails.
    pub fn events_from(&self, ts: i64) -> Result<Vec<EventRow>> {
        self.query_events(
            "SELECT id, ts, actor, kind, payload FROM events WHERE ts >= ?1 ORDER BY id",
            params![ts],
        )
    }

    fn query_events(&self, sql: &str, args: impl rusqlite::Params) -> Result<Vec<EventRow>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(args, |r| {
            let payload: String = r.get(4)?;
            Ok(EventRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                actor: r.get(2)?,
                kind: r.get(3)?,
                payload: serde_json::from_str(&payload)
                    .unwrap_or(serde_json::Value::String(payload)),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// One call through the model gateway.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GatewayUsage {
    pub id: i64,
    /// Seconds since the epoch.
    pub ts: i64,
    pub account: String,
    pub model: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    /// Replayed from the cache: no provider call.
    pub cached: bool,
    /// The HTTP status returned to the caller.
    pub status: u16,
    pub ms: i64,
    /// From `gateway.prices`; 0 for a replay or a model without a price.
    pub cost_usd: f64,
}

/// One entry of the event log: who did what, when.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct EventRow {
    pub id: i64,
    /// Seconds since the epoch.
    pub ts: i64,
    pub actor: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// Apply the steps from the file's version on, in one write transaction: an opener that comes second reads the new
/// version and has nothing to do. A version with no step stays as it is.
fn upgrade(conn: &Connection, schema: &Schema) -> Result<i64> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    while let Some((_, sql)) = schema.steps.iter().find(|(from, _)| *from == version) {
        tx.execute_batch(sql)
            .with_context(|| format!("the schema step from v{version}"))?;
        version += 1;
        tx.pragma_update(None, "user_version", version)?;
    }
    tx.commit()?;
    Ok(version)
}

/// Make the database at `path`: whole (WAL mode, `schema`, its version) in a file of this process and thread, then
/// linked into place, which fails when another opener was first. So no connection sees a file that is half made
/// (opening one while another switches it to WAL or makes its tables can fail at once with "database is locked").
fn make(path: &Path, schema: &Schema) -> Result<()> {
    let new = PathBuf::from(format!(
        "{}.new-{}-{:?}",
        path.display(),
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&new);
    let conn = Connection::open(&new).with_context(|| format!("creating {}", new.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(schema.sql)
        .with_context(|| format!("making the schema of {}", path.display()))?;
    conn.pragma_update(None, "user_version", schema.version)?;
    drop(conn);
    let linked = std::fs::hard_link(&new, path);
    std::fs::remove_file(&new)?;
    match linked {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            Err(e).with_context(|| format!("creating {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file from another release (an old step-by-step version, or a newer schema) is refused, names its version,
    /// and keeps its data; a file at this version opens with its data.
    #[test]
    fn a_database_of_another_schema_version_is_refused_and_kept() {
        let dir = crate::scratch::Scratch::new(
            std::env::temp_dir().join(format!("legionctl-db-version-{}", std::process::id())),
        );
        let path = dir.join("operator.db");
        let db = Db::open(&path, OPERATOR).unwrap();
        db.record("cli", "kept", &serde_json::json!({})).unwrap();
        drop(db);
        let reopened = Db::open(&path, OPERATOR).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), OPERATOR.version);
        assert_eq!(
            reopened
                .events_page(&EventFilter::default(), 0, 10)
                .unwrap()
                .0[0]
                .kind,
            "kept"
        );
        drop(reopened);
        for version in [OPERATOR.version - 1, OPERATOR.version + 1] {
            Connection::open(&path)
                .unwrap()
                .pragma_update(None, "user_version", version)
                .unwrap();
            let refused = Db::open(&path, OPERATOR).err().unwrap().to_string();
            assert!(
                refused.contains(&format!("has schema v{version}")),
                "{refused}"
            );
            let left: i64 = Connection::open(&path)
                .unwrap()
                .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
                .unwrap();
            assert_eq!(left, 1, "the refused file keeps its data");
        }
    }

    /// The operator opens its database for each call, from several threads, while the recorder writes: a new
    /// database that many connections open at once gets its schema once, and every open succeeds.
    #[test]
    fn many_connections_that_open_a_new_database_at_once_all_succeed() {
        let dir = crate::scratch::Scratch::new(
            std::env::temp_dir().join(format!("legionctl-db-race-{}", std::process::id())),
        );
        let path = dir.join("operator.db");
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            let opens: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        Db::open(&path, OPERATOR).map(|db| db.schema_version().unwrap())
                    })
                })
                .collect();
            for open in opens {
                assert_eq!(open.join().unwrap().unwrap(), OPERATOR.version);
            }
        });
        let mode: String = Db::open(&path, OPERATOR)
            .unwrap()
            .conn()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal", "readers go on while one process writes");
    }
}
