//! The call record (DESIGN/22 rule 3): one row per run of an agent's operation, a brokered MCP tool or a model call,
//! written before the call runs. A caller that retries with the same call id gets the recorded result and never a
//! second run of a change; a read with a cache time answers from the record within that time.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use crate::db::Db;

/// v15 -> v16: the record, and the rows of `internal.tools.record` (the earlier MCP tool call log) in it.
macro_rules! tables {
    () => {
        "
     CREATE TABLE calls (
        id         INTEGER PRIMARY KEY,
        call       TEXT NOT NULL UNIQUE,
        principal  TEXT NOT NULL,
        session    TEXT NOT NULL DEFAULT '',
        kind       TEXT NOT NULL,
        server     TEXT NOT NULL,
        name       TEXT NOT NULL,
        input_hash TEXT NOT NULL,
        scope      TEXT NOT NULL DEFAULT '',
        started    INTEGER NOT NULL,
        ended      INTEGER,
        outcome    TEXT,
        output     TEXT,
        bytes      INTEGER NOT NULL DEFAULT 0
     );
     CREATE INDEX calls_cache ON calls (principal, name, input_hash, id);
     INSERT INTO calls (call, principal, session, kind, server, name, input_hash, started, ended, outcome, bytes)
        SELECT 'event-' || id,
               CASE WHEN COALESCE(json_extract(payload, '$.input.session'), '') = '' THEN 'user'
                    ELSE 'session:' || json_extract(payload, '$.input.session') END,
               COALESCE(json_extract(payload, '$.input.session'), ''),
               'tool',
               json_extract(payload, '$.input.server'),
               json_extract(payload, '$.input.tool'),
               json_extract(payload, '$.input.args_hash'),
               ts * 1000 - COALESCE(json_extract(payload, '$.input.ms'), 0),
               ts * 1000,
               json_extract(payload, '$.input.outcome'),
               COALESCE(json_extract(payload, '$.input.bytes'), 0)
        FROM events WHERE kind = 'internal.tools.record';"
    };
}
pub(crate) use tables;

/// v16 -> v17: the folder each recorded call worked in, the folder each agent session works in now (its newest hook
/// call's `cwd`), which that session's MCP server works in too, each user thread's scratch folder, kept at its
/// start (before, each caller's config gave it), and the app's tab facts without their old `repo=` parameters.
macro_rules! folders {
    () => {
        concat!(
            "ALTER TABLE calls ADD COLUMN folder TEXT NOT NULL DEFAULT '';
     CREATE TABLE agent_folders (
        session TEXT PRIMARY KEY,
        folder  TEXT NOT NULL,
        harness TEXT NOT NULL,
        seen    INTEGER NOT NULL
     );
     ALTER TABLE threads ADD COLUMN scratch TEXT;",
            crate::calls::old_tab_params!()
        )
    };
}
pub(crate) use folders;

/// Part of v16 -> v17: the app's tab facts lose the `repo=` view parameters of the time when each tab had its own
/// repository (the app's one project is the folder of every view now).
macro_rules! old_tab_params {
    () => {
        "
     UPDATE facts SET value = json_set(value, '$.tabs', (
        SELECT json_group_array(CASE WHEN json_type(t.value, '$.params') = 'array' THEN json_set(t.value, '$.params',
            (SELECT json_group_array(p.value) FROM json_each(t.value, '$.params') p WHERE p.value NOT LIKE 'repo=%'))
          ELSE json(t.value) END)
        FROM json_each(facts.value, '$.tabs') t))
     WHERE entity = 'app' AND attribute = 'tabs' AND json_valid(value) AND json_type(value, '$.tabs') = 'array'
       AND value LIKE '%repo=%';"
    };
}
pub(crate) use old_tab_params;

/// A call as it starts.
pub struct Call<'a> {
    /// Its id: the caller's (a retry) or a new one.
    pub call: &'a str,
    /// `user`, `session:<id>` or `agent:<id>`.
    pub principal: &'a str,
    pub session: &'a str,
    /// `op`, `tool` or `model`.
    pub kind: &'a str,
    /// `legionctl` for an operation, the MCP server, or the gateway's account.
    pub server: &'a str,
    pub name: &'a str,
    pub input_hash: &'a str,
    /// The caller's toolsets as one text: a cached answer is for the same scope only.
    pub scope: &'a str,
}

/// What the record says of a call id when the call starts.
pub enum Begun {
    /// A new call: its row is written.
    New,
    /// The call ended before: its outcome and kept output.
    Ended {
        outcome: String,
        output: Option<String>,
    },
    /// The call started and has no end: it still runs, or the operator stopped during it.
    Open { started: i64 },
    /// The id is of another caller's or another input's call.
    Other,
}

/// A row of the record, as `tools calls` lists it.
pub struct Row {
    pub id: i64,
    pub call: String,
    pub session: String,
    pub kind: String,
    pub server: String,
    pub name: String,
    pub input_hash: String,
    pub started: i64,
    pub ended: Option<i64>,
    pub outcome: Option<String>,
    pub bytes: i64,
    /// The folder the call worked in; empty when the record does not say.
    pub folder: String,
}

#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// A new call id.
///
/// # Errors
/// The kernel's uuid file cannot be read.
pub fn new_id() -> Result<String> {
    crate::session::new_id()
}

impl Db {
    /// Start `call`: a new row before the call runs, or what the record has for its id.
    ///
    /// # Errors
    /// The database write fails.
    pub fn call_begin(&self, call: &Call) -> Result<Begun> {
        let tx = rusqlite::Transaction::new_unchecked(
            self.conn(),
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let found = tx
            .query_row(
                "SELECT principal, name, input_hash, ended, outcome, output, started FROM calls WHERE call = ?1",
                [call.call],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        let begun = if let Some((principal, name, hash, ended, outcome, output, started)) = found {
            if principal != call.principal || name != call.name || hash != call.input_hash {
                Begun::Other
            } else if ended.is_some() {
                Begun::Ended {
                    outcome: outcome.unwrap_or_default(),
                    output,
                }
            } else {
                Begun::Open { started }
            }
        } else {
            tx.execute(
                    "INSERT INTO calls (call, principal, session, kind, server, name, input_hash, scope, started)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        call.call,
                        call.principal,
                        call.session,
                        call.kind,
                        call.server,
                        call.name,
                        call.input_hash,
                        call.scope,
                        now_ms()
                    ],
                )?;
            Begun::New
        };
        tx.commit()?;
        Ok(begun)
    }

    /// A call that ended already (a model call of the gateway, which writes its usage row after the call).
    ///
    /// # Errors
    /// The database write fails.
    pub fn call_add(&self, call: &Call, (started, ended): (i64, i64), outcome: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO calls (call, principal, session, kind, server, name, input_hash, scope, started, ended, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                call.call,
                call.principal,
                call.session,
                call.kind,
                call.server,
                call.name,
                call.input_hash,
                call.scope,
                started,
                ended,
                outcome
            ],
        )?;
        Ok(())
    }

    /// The output of the newest ended call of `call`'s caller, name, input and scope since `since` (ms), when the
    /// record kept one.
    ///
    /// # Errors
    /// The database read fails.
    pub fn call_cached(&self, call: &Call, since: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT output FROM calls WHERE principal = ?1 AND name = ?2 AND input_hash = ?3 AND scope = ?4
                 AND outcome = 'ok' AND output IS NOT NULL AND ended >= ?5 AND call != ?6 ORDER BY id DESC LIMIT 1",
                params![
                    call.principal,
                    call.name,
                    call.input_hash,
                    call.scope,
                    since,
                    call.call
                ],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The folder that call `call` works in.
    ///
    /// # Errors
    /// The database write fails.
    pub fn call_folder(&self, call: &str, folder: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE calls SET folder = ?2 WHERE call = ?1",
            params![call, folder],
        )?;
        Ok(())
    }

    /// Agent `session` of `harness` (claude or codex) works in `folder` now (its hook call says so).
    ///
    /// # Errors
    /// The database write fails.
    pub fn agent_folder_set(&self, session: &str, folder: &str, harness: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO agent_folders (session, folder, harness, seen) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (session) DO UPDATE SET folder = excluded.folder, harness = excluded.harness,
             seen = excluded.seen",
            params![session, folder, harness, now_ms()],
        )?;
        Ok(())
    }

    /// Agent `session` ended: it works in no folder.
    ///
    /// # Errors
    /// The database write fails.
    pub fn agent_folder_end(&self, session: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM agent_folders WHERE session = ?1", [session])?;
        Ok(())
    }

    /// The folder agent `session` works in now.
    ///
    /// # Errors
    /// The database read fails.
    pub fn agent_folder(&self, session: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT folder FROM agent_folders WHERE session = ?1",
                [session],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The folders that the live sessions of `harness` work in now, each inside `within` (or `within` itself).
    ///
    /// # Errors
    /// The database read fails.
    pub fn agent_folders_in(&self, harness: &str, within: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT folder FROM agent_folders WHERE harness = ?1 ORDER BY session")?;
        let all: Vec<String> = stmt
            .query_map([harness], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(all
            .into_iter()
            .filter(|f| std::path::Path::new(f).starts_with(within))
            .collect())
    }

    /// End call `call` with its outcome, its size and, when a retry or the cache needs it, its output.
    ///
    /// # Errors
    /// The database write fails.
    pub fn call_end(
        &self,
        call: &str,
        outcome: &str,
        output: Option<&str>,
        bytes: u64,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE calls SET ended = ?2, outcome = ?3, output = ?4, bytes = ?5 WHERE call = ?1",
            params![
                call,
                now_ms(),
                outcome,
                output,
                i64::try_from(bytes).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(())
    }

    /// Rows of the record after the row `after`, oldest first, of `session` and `kind` when given; and the row to go
    /// on after when more follow.
    ///
    /// # Errors
    /// The database read fails.
    pub fn calls_page(
        &self,
        (session, kind): (Option<&str>, Option<&str>),
        after: i64,
        limit: u32,
    ) -> Result<(Vec<Row>, Option<i64>)> {
        let mut stmt = self.conn().prepare(
            "SELECT id, call, session, kind, server, name, input_hash, started, ended, outcome, bytes, folder FROM calls
             WHERE id > ?1 AND (?2 IS NULL OR session = ?2) AND (?3 IS NULL OR kind = ?3) ORDER BY id LIMIT ?4",
        )?;
        let rows: Vec<Row> = stmt
            .query_map(params![after, session, kind, i64::from(limit) + 1], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    call: r.get(1)?,
                    session: r.get(2)?,
                    kind: r.get(3)?,
                    server: r.get(4)?,
                    name: r.get(5)?,
                    input_hash: r.get(6)?,
                    started: r.get(7)?,
                    ended: r.get(8)?,
                    outcome: r.get(9)?,
                    bytes: r.get(10)?,
                    folder: r.get(11)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let next = (rows.len() > limit).then(|| rows[limit - 1].id);
        Ok((rows.into_iter().take(limit).collect(), next))
    }
}

#[cfg(test)]
mod tests {
    /// The app's tab facts of an older build lose their `repo=` view parameters in the v17 step; the other
    /// parameters, tabs and facts stay as they were.
    #[test]
    fn the_v17_step_drops_the_old_repo_parameters_of_the_apps_tabs() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::facts::tables!()).unwrap();
        let fact = |entity: &str, attribute: &str, value: &str| {
            db.execute(
                "INSERT INTO facts (entity, attribute, value, scope, scope_id, source, evidence, confidence, \
                 valid_from, recorded_from) VALUES (?1, ?2, ?3, 'global', '', 'user', '{}', 1.0, 0, 0)",
                [entity, attribute, value],
            )
            .unwrap();
        };
        fact(
            "app",
            "tabs",
            r#"{"tabs":[{"kind":"view","name":"code-graph","params":["repo=/old","key=rust:.:add"]},{"kind":"chat"},{"kind":"thread","id":3}],"active":1}"#,
        );
        fact("app", "project", r#""/work/repo=x""#);
        db.execute_batch(old_tab_params!()).unwrap();
        let values: Vec<serde_json::Value> = db
            .prepare("SELECT value FROM facts ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|v| serde_json::from_str(&v.unwrap()).unwrap())
            .collect();
        assert_eq!(
            values,
            [
                serde_json::json!({"tabs": [{"kind": "view", "name": "code-graph", "params": ["key=rust:.:add"]},
                    {"kind": "chat"}, {"kind": "thread", "id": 3}], "active": 1}),
                serde_json::json!("/work/repo=x")
            ]
        );
    }
}
