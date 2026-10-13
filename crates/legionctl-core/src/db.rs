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
use rusqlite::{Connection, params};

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

fn now() -> i64 {
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
        migrate(&mut conn, migrations)?;
        Ok(Db { conn })
    }

    /// # Errors
    /// As [`Db::open`].
    pub fn open_default() -> Result<Db> {
        Self::open(&default_path(), OPERATOR)
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

    /// Events after `since` (an event id), oldest first, at most `limit`.
    ///
    /// # Errors
    /// The database read fails.
    pub fn events(&self, since: i64, limit: u32) -> Result<Vec<EventRow>> {
        self.query_events(
            "SELECT id, ts, actor, kind, payload FROM events WHERE id > ?1 ORDER BY id LIMIT ?2",
            params![since, limit],
        )
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

    /// The last `limit` events whose payload names `path` (`*` matches any text, for per-CPU files), oldest first.
    ///
    /// # Errors
    /// The database read fails.
    pub fn events_naming(&self, path: &str, limit: u32) -> Result<Vec<EventRow>> {
        let escaped = path
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{}%", escaped.replace('*', "%"));
        let mut rows = self.query_events(
            "SELECT id, ts, actor, kind, payload FROM events WHERE payload LIKE ?1 ESCAPE '\\' \
             ORDER BY id DESC LIMIT ?2",
            params![pattern, limit],
        )?;
        rows.reverse();
        Ok(rows)
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

fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (index, sql) in migrations
        .iter()
        .enumerate()
        .skip(usize::try_from(current).unwrap_or(0))
    {
        let tx = conn.transaction()?;
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
