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

/// Ordered schema migrations; index + 1 is the schema version after applying it. Never edit a released entry.
const OPERATOR: &[&str] = &[
    // v1: event log, hook actions and bindings, with the default bindings the agent plugin relies on
    "CREATE TABLE events (
        id      INTEGER PRIMARY KEY,
        ts      INTEGER NOT NULL,
        actor   TEXT NOT NULL,
        kind    TEXT NOT NULL,
        payload TEXT NOT NULL
     );
     CREATE TABLE hook_actions (
        id        TEXT PRIMARY KEY,
        command   TEXT NOT NULL,
        timeout_s INTEGER NOT NULL DEFAULT 5
     );
     CREATE TABLE hook_bindings (
        event    TEXT NOT NULL,
        action   TEXT NOT NULL,
        position INTEGER NOT NULL,
        PRIMARY KEY (event, action)
     );
     INSERT INTO hook_bindings (event, action, position) VALUES
        ('session-start', 'reminders', 0),
        ('session-start', 'load', 1),
        ('session-start', 'setup-check', 2),
        ('prompt', 'reminders', 0),
        ('prompt', 'load-if-busy', 1);",
    // v2: hook actions and bindings are config now (hooks.actions, hooks.bindings)
    "DROP TABLE hook_actions;
     DROP TABLE hook_bindings;",
    // v3: results of MCP tools that mcp.servers.<name>.cache allows to reuse
    "CREATE TABLE tool_cache (
        key     TEXT PRIMARY KEY,
        value   TEXT NOT NULL,
        expires INTEGER NOT NULL
     );",
    // v4: the model gateway: replayable responses and one usage row per call (note 19)
    "CREATE TABLE gateway_cache (
        key          TEXT PRIMARY KEY,
        status       INTEGER NOT NULL,
        content_type TEXT NOT NULL,
        body         BLOB NOT NULL,
        expires      INTEGER NOT NULL
     );
     CREATE TABLE gateway_usage (
        id         INTEGER PRIMARY KEY,
        ts         INTEGER NOT NULL,
        account    TEXT NOT NULL,
        model      TEXT NOT NULL,
        tokens_in  INTEGER NOT NULL,
        tokens_out INTEGER NOT NULL,
        cached     INTEGER NOT NULL,
        status     INTEGER NOT NULL,
        ms         INTEGER NOT NULL
     );",
    // v5: the cost of each gateway call, from gateway.prices (money budgets)
    "ALTER TABLE gateway_usage ADD COLUMN cost_usd REAL NOT NULL DEFAULT 0;",
    // v6: the semantic cache: the prompt's embedding for each kept answer (gateway.semantic)
    "CREATE TABLE gateway_semantic (
        key    TEXT PRIMARY KEY,
        scope  TEXT NOT NULL,
        vector BLOB NOT NULL
     );",
    // v7: session tokens (DESIGN/03): the hash of each sandbox's token and its session; the user token is a file
    "CREATE TABLE tokens (
        hash    TEXT PRIMARY KEY,
        session TEXT NOT NULL,
        created INTEGER NOT NULL
     );",
    // v8: pages of the log by kind (DESIGN/03): `kind = ? AND id > ?` uses this
    "CREATE INDEX events_kind ON events (kind, id);",
    // v9: the caches moved to the disposable cache.db (legionctl_core::cache)
    "DROP TABLE tool_cache; DROP TABLE gateway_semantic; DROP TABLE gateway_cache;",
    // v10: jobs, the long operations (DESIGN/02): what ran, its input, and how it ended; the operator resumes the ones
    // a restart cut off
    "CREATE TABLE jobs (
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
];

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

/// A long operation and how it ended (`state`: running, done, failed).
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
            "UPDATE jobs SET state = ?2, result = ?3, error = ?4, updated = ?5 WHERE id = ?1",
            params![id, state, result, error, now()],
        )?;
        Ok(())
    }

    /// Run the job once more (after a restart cut it off).
    ///
    /// # Errors
    /// The database write fails.
    pub fn job_again(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET attempts = attempts + 1, updated = ?2 WHERE id = ?1",
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
    /// Open the database at `path` and apply the `migrations` it has not run yet (index + 1 is the schema version).
    ///
    /// # Errors
    /// The folder or database cannot be created or opened, or a migration fails.
    pub fn open(path: &Path, migrations: &[&str]) -> Result<Db> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let mut conn =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn, migrations, path)?;
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

/// Apply the pending migrations; an existing database is first copied to `<db>.v<version>.bak` (note 13: a failed
/// migration leaves the previous version's data in place).
fn migrate(conn: &mut Connection, migrations: &[&str], path: &Path) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > 0 && usize::try_from(current).unwrap_or(usize::MAX) < migrations.len() {
        let backup = PathBuf::from(format!("{}.v{current}.bak", path.display()));
        // a backup of this version that exists is kept: another process that opens the database now makes the same
        // one, and deleting it under that process breaks its write
        if !backup.exists()
            && let Err(e) = conn.execute("VACUUM INTO ?1", params![backup.to_string_lossy()])
            && !backup.exists()
        {
            return Err(e)
                .with_context(|| format!("backing up {} before its migration", path.display()));
        }
    }
    for (index, sql) in migrations
        .iter()
        .enumerate()
        .skip(usize::try_from(current).unwrap_or(0))
    {
        // the write lock first, then the version again: another process may have applied this step meanwhile
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let applied: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if usize::try_from(applied).unwrap_or(0) > index {
            continue;
        }
        tx.execute_batch(sql)
            .with_context(|| format!("applying schema migration {}", index + 1))?;
        tx.pragma_update(None, "user_version", i64::try_from(index + 1)?)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn temp_db(tag: &str) -> Db {
    let dir = std::env::temp_dir().join(format!("legionctl-db-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Db::open(&dir.join("operator.db"), OPERATOR).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_migration_keeps_a_backup_of_the_previous_version() {
        let dir = std::env::temp_dir().join(format!("legionctl-db-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("operator.db");
        let old = Db::open(&path, &OPERATOR[..3]).unwrap();
        old.record("cli", "before.upgrade", &serde_json::json!({}))
            .unwrap();
        drop(old);
        let new = Db::open(&path, OPERATOR).unwrap();
        assert_eq!(
            new.schema_version().unwrap(),
            i64::try_from(OPERATOR.len()).unwrap()
        );
        let backup = Db::open(&dir.join("operator.db.v3.bak"), &OPERATOR[..3]).unwrap();
        assert_eq!(
            backup.schema_version().unwrap(),
            3,
            "the backup is the previous version, unmigrated"
        );
        assert_eq!(
            backup
                .events_page(&EventFilter::default(), 0, 10)
                .unwrap()
                .0[0]
                .kind,
            "before.upgrade"
        );
    }

    /// The operator opens its database for each call, from several threads, while the recorder writes: an old
    /// database that many connections open at once is migrated once, and every open succeeds.
    #[test]
    fn many_connections_that_open_an_old_database_at_once_all_succeed() {
        let dir = std::env::temp_dir().join(format!("legionctl-db-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("operator.db");
        drop(Db::open(&path, &OPERATOR[..2]).unwrap());
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
                assert_eq!(
                    open.join().unwrap().unwrap(),
                    i64::try_from(OPERATOR.len()).unwrap()
                );
            }
        });
    }

    #[test]
    fn migrates_once_and_reopens_cleanly() {
        let db = temp_db("migrate");
        assert_eq!(
            db.schema_version().unwrap(),
            i64::try_from(OPERATOR.len()).unwrap()
        );
        let path = db.conn().path().unwrap().to_owned();
        drop(db);
        // a second run of v2 would fail on DROP TABLE: reopening must not migrate again
        let again = Db::open(Path::new(&path), OPERATOR).unwrap();
        let hook_tables: i64 = again
            .conn()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name LIKE 'hook_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hook_tables, 0, "hook bindings are config since v2");
        let mode: String = again
            .conn()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
}
