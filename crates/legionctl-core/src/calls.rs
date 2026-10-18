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
            "SELECT id, call, session, kind, server, name, input_hash, started, ended, outcome, bytes FROM calls
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
                })
            })?
            .collect::<Result<_, _>>()?;
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let next = (rows.len() > limit).then(|| rows[limit - 1].id);
        Ok((rows.into_iter().take(limit).collect(), next))
    }
}
