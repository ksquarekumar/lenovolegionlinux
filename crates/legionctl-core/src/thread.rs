//! Threads (design 23, Part B, P5): one task with its members (`main0` and observers), an append-only thread log,
//! a durable outbox that the main agent's hooks deliver, deterministic watch rules with counts, pace, and presence
//! (the peers of a session and what they do). The tables are in the operator's database (a thread's members come
//! from several dossiers, so its rows span dossiers: design 23, E1 decision 1); the log is the source, and each
//! projection (members, messages, rule counts) changes in the same transaction as its log row.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

use crate::db::{Db, EventRow, now};
use crate::dossier::Dossiers;
use crate::workspace::Workspace;

/// The thread tables (operator.db v11; workspace.db v13 before the dossier homes).
macro_rules! tables {
    () => {
        "
     -- threads (design 23, B2): one task; its log is append-only, the tables after it are its projections
     CREATE TABLE threads (
        id      INTEGER PRIMARY KEY,
        title   TEXT NOT NULL,
        dossier TEXT NOT NULL,
        created INTEGER NOT NULL,
        state   TEXT NOT NULL DEFAULT 'open'
     );
     CREATE TABLE thread_log (
        id      INTEGER PRIMARY KEY,
        thread  INTEGER NOT NULL,
        ts      INTEGER NOT NULL,
        member  TEXT NOT NULL,
        kind    TEXT NOT NULL,
        payload TEXT NOT NULL
     );
     CREATE INDEX thread_log_thread ON thread_log (thread, id);
     -- cursor: the last log id the member saw (its last wake)
     CREATE TABLE members (
        thread    INTEGER NOT NULL,
        name      TEXT NOT NULL,
        archetype TEXT NOT NULL,
        session   TEXT,
        pace      TEXT NOT NULL,
        cursor    INTEGER NOT NULL DEFAULT 0,
        state     TEXT NOT NULL,
        joined    INTEGER NOT NULL,
        PRIMARY KEY (thread, name)
     );
     CREATE INDEX members_session ON members (session);
     -- the outbox: recipient is a member name of the thread or a peer's session id
     CREATE TABLE messages (
        id        INTEGER PRIMARY KEY,
        thread    INTEGER NOT NULL,
        sender    TEXT NOT NULL,
        recipient TEXT NOT NULL,
        kind      TEXT NOT NULL,
        body      TEXT NOT NULL,
        tool      TEXT,
        state     TEXT NOT NULL DEFAULT 'pending',
        created   INTEGER NOT NULL,
        delivered INTEGER,
        acked     INTEGER
     );
     -- watch rules and their counts: run = trigger matches since the last expected step, misses = times it fired,
     -- done = steps the watching member did for the watched one
     CREATE TABLE watch_rules (
        id      INTEGER PRIMARY KEY,
        thread  INTEGER NOT NULL,
        member  TEXT NOT NULL,
        watched TEXT NOT NULL,
        tools   TEXT NOT NULL,
        path    TEXT NOT NULL,
        expect  TEXT NOT NULL,
        every   INTEGER NOT NULL,
        kind    TEXT NOT NULL,
        created INTEGER NOT NULL,
        run     INTEGER NOT NULL DEFAULT 0,
        misses  INTEGER NOT NULL DEFAULT 0,
        done    INTEGER NOT NULL DEFAULT 0
     );"
    };
}
pub(crate) use tables;

/// A closed set of words, kept in the database as text.
macro_rules! words {
    ($(#[$m:meta])* $E:ident { $($(#[$vm:meta])* $V:ident = $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $E { $($(#[$vm])* $V),* }

        impl $E {
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$V => $s),* }
            }

            /// # Errors
            /// The word is not one of the set.
            pub fn parse(word: &str) -> Result<Self> {
                match word {
                    $($s => Ok(Self::$V),)*
                    other => bail!("unknown {} '{other}'", stringify!($E)),
                }
            }
        }
    };
}

words!(
    /// What a message does to its receiver (design 23, B3).
    Kind {
        Note = "note",
        Hint = "hint",
        Reminder = "reminder",
        Notice = "notice",
        Result = "result",
        SoftBlock = "soft-block",
        HardBlock = "hard-block",
    }
);

words!(
    /// The role of a member (design 23, B+3).
    Archetype {
        MainAgent = "main-agent",
        ThreadObserver = "thread-observer",
        UiComposer = "ui-composer",
    }
);

words!(
    /// When the operator wakes a member (design 23, B5); timed paces come with the pools (P6b).
    Pace {
        OnTrigger = "on-trigger",
        OnStop = "on-stop",
        OnDeclare = "on-declare",
    }
);

words!(
    /// A state that a member announces.
    Declared {
        Done = "done",
        Ready = "ready",
        Blocked = "blocked",
        TestsWritten = "tests-written",
    }
);

/// The message kinds that the prompt hook gives; the blocks go to the stop and pre-tool hooks.
const TOLD: &str = "'note', 'hint', 'reminder', 'notice', 'result'";
/// Message states that still act on the receiver.
pub(crate) const OPEN: &str = "'pending', 'delivered'";

pub struct ThreadInfo {
    pub id: i64,
    pub title: String,
    pub dossier: String,
    pub created: i64,
    pub state: String,
    pub account: Option<String>,
    pub imported_from: Option<i64>,
}

pub struct Member {
    pub name: String,
    pub archetype: Archetype,
    pub session: Option<String>,
    pub pace: Pace,
    pub cursor: i64,
    pub state: String,
    pub joined: i64,
    /// The wakes the operator gave it.
    pub wakes: u32,
    /// The agent config it uses (a team member).
    pub config: Option<String>,
    /// How it runs: self, operator, teammate or subagent.
    pub run: String,
}

pub struct Message {
    pub id: i64,
    pub thread: i64,
    pub from: String,
    pub to: String,
    pub kind: Kind,
    pub body: String,
    pub tool: Option<String>,
    pub state: String,
    pub created: i64,
}

pub struct Rule {
    pub id: i64,
    pub thread: i64,
    pub member: String,
    pub watched: String,
    pub tools: String,
    pub path: String,
    pub expect: String,
    pub every: u32,
    pub kind: Kind,
    pub created: i64,
    pub run: u32,
    pub misses: u32,
    pub done: u32,
    /// The expected command must come before the reply ends: the stop counts the miss, not `every`.
    pub reply: bool,
}

pub struct LogRow {
    pub id: i64,
    pub ts: i64,
    pub member: String,
    pub kind: String,
    pub payload: Value,
}

pub struct ThreadView {
    pub thread: ThreadInfo,
    pub members: Vec<Member>,
    pub messages: Vec<Message>,
    pub rules: Vec<Rule>,
    pub log: Vec<LogRow>,
    /// Per member: its wakes' tokens and time, or (main0) the context its hooks added and their wait.
    pub costs: Vec<crate::pool::Cost>,
    /// What the members did for main that `thread compensate` can undo.
    pub effects: Vec<crate::pool::Effect>,
}

/// A watch rule as `thread rules add` takes it.
pub struct NewRule<'a> {
    pub member: &'a str,
    pub watched: &'a str,
    pub tools: &'a str,
    pub path: &'a str,
    pub expect: &'a str,
    pub every: u32,
    pub kind: Kind,
}

/// One tool call of an agent, from the `post-tool` hook.
pub struct ToolUse<'a> {
    pub tool: &'a str,
    pub path: Option<&'a str>,
    pub command: Option<&'a str>,
    /// `SendMessage`: the receiver and the text.
    pub to: Option<&'a str>,
    pub message: Option<&'a str>,
    pub ok: bool,
}

/// Which peers `thread peers` lists: the ones that share one of these with the caller.
#[derive(Default)]
pub struct Scope {
    pub dossier: Option<String>,
    pub topic: Option<String>,
    pub thread: Option<i64>,
}

pub struct Peer {
    /// Its id (`peer_key`).
    pub session: String,
    pub agent: String,
    /// A subagent's type.
    pub subagent: Option<String>,
    pub account: Option<String>,
    pub dossier: Option<String>,
    /// The topic of the workspace it works in ("" for the dossier's own workspace).
    pub topic: Option<String>,
    pub workspace: Option<PathBuf>,
    /// `<thread>/<member>` for each thread it is a member of.
    pub members: Vec<String>,
    /// What it shares with the caller: `dossier <d>`, `workspace <path>`, `thread <id>`.
    pub shares: Vec<String>,
    /// running, idle, ended or unknown: from its last hook call.
    pub state: String,
    pub seen: Option<i64>,
    pub last_tool: Option<String>,
    pub last_path: Option<String>,
    pub declared: Option<String>,
    /// Messages to it that it did not ack.
    pub pending: u32,
    /// How to reach it: `outbox` (`thread send`), and `claude-inbox` (Claude Code's `SendMessage`).
    pub channels: Vec<String>,
}

pub struct Observed {
    pub peer: Peer,
    pub events: Vec<EventRow>,
}

/// 1st, 2nd, 3rd, 4th, ..., 11th, 12th, 13th, 21st.
pub(crate) fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// The clock time of `ts` (UTC).
pub(crate) fn clock(ts: i64) -> String {
    format!(
        "{:02}:{:02} UTC",
        ts.rem_euclid(86_400) / 3600,
        ts.rem_euclid(3600) / 60
    )
}

pub(crate) fn log(
    tx: &Transaction,
    thread: i64,
    member: &str,
    kind: &str,
    payload: &Value,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO thread_log (thread, ts, member, kind, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![thread, now(), member, kind, payload.to_string()],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Write a message into the outbox, with its log row.
pub(crate) fn post(
    tx: &Transaction,
    thread: i64,
    (from, to): (&str, &str),
    kind: Kind,
    body: &str,
    tool: Option<&str>,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO messages (thread, sender, recipient, kind, body, tool, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![thread, from, to, kind.as_str(), body, tool, now()],
    )?;
    let id = tx.last_insert_rowid();
    log(
        tx,
        thread,
        from,
        "thread.message",
        &json!({"id": id, "to": to, "kind": kind.as_str(), "text": body, "tool": tool, "channel": "outbox"}),
    )?;
    Ok(id)
}

/// Wake a member: the log events since its cursor go to it. A member that the operator runs (`run` operator) gets
/// its wake as an ACP prompt (`pool`), and its cursor moves when the prompt ends; for the others the wake is a record,
/// and the cursor moves at once.
pub(crate) fn wake(tx: &Transaction, thread: i64, member: &str, reason: &str) -> Result<()> {
    let (cursor, run): (i64, String) = tx.query_row(
        "SELECT cursor, run FROM members WHERE thread = ?1 AND name = ?2",
        params![thread, member],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let events: i64 = tx.query_row(
        "SELECT count(*) FROM thread_log WHERE thread = ?1 AND id > ?2",
        params![thread, cursor],
        |r| r.get(0),
    )?;
    let id = log(
        tx,
        thread,
        member,
        "thread.wake",
        &json!({"reason": reason, "events": events}),
    )?;
    let column = if run == crate::pool::Run::Operator.as_str() {
        "due"
    } else {
        "cursor"
    };
    tx.execute(
        &format!("UPDATE members SET {column} = ?3 WHERE thread = ?1 AND name = ?2"),
        params![thread, member, id],
    )?;
    Ok(())
}

/// Wake the other active members of `thread` whose pace is one of `paces`.
fn wake_paced(
    tx: &Transaction,
    thread: i64,
    except: &str,
    paces: &[Pace],
    reason: &str,
) -> Result<()> {
    let members: Vec<(String, String)> = tx
        .prepare(
            "SELECT name, pace FROM members WHERE thread = ?1 AND name != ?2 AND state = 'active'",
        )?
        .query_map(params![thread, except], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (name, pace) in members {
        if paces.contains(&Pace::parse(&pace)?) {
            wake(tx, thread, &name, reason)?;
        }
    }
    Ok(())
}

/// The messages to `session`: to its member name in a thread, or to its session id.
pub(crate) fn to_session(states: &str, kinds: &str) -> String {
    format!(
        "SELECT m.id, m.thread, m.sender, m.recipient, m.kind, m.body, m.tool, m.state, m.created FROM messages m
         WHERE m.state IN ({states}) AND m.kind IN ({kinds}) AND (m.recipient = ?1 OR EXISTS (SELECT 1 FROM members b
         WHERE b.thread = m.thread AND b.name = m.recipient AND b.session = ?1 AND b.state = 'active')) ORDER BY m.id"
    )
}

/// A word column parsed into its set; a word outside the set is a conversion error of that column.
fn word<T>(r: &rusqlite::Row, column: usize, parse: fn(&str) -> Result<T>) -> rusqlite::Result<T> {
    parse(&r.get::<_, String>(column)?).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, e.into())
    })
}

pub(crate) fn messages(
    conn: &rusqlite::Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<Message>> {
    Ok(conn
        .prepare(sql)?
        .query_map(args, |r| {
            Ok(Message {
                id: r.get(0)?,
                thread: r.get(1)?,
                from: r.get(2)?,
                to: r.get(3)?,
                kind: word(r, 4, Kind::parse)?,
                body: r.get(5)?,
                tool: r.get(6)?,
                state: r.get(7)?,
                created: r.get(8)?,
            })
        })?
        .collect::<Result<_, _>>()?)
}

/// Mark these messages delivered.
pub(crate) fn delivered(tx: &Transaction, list: &[Message]) -> Result<()> {
    for m in list.iter().filter(|m| m.state == "pending") {
        tx.execute(
            "UPDATE messages SET state = 'delivered', delivered = ?2 WHERE id = ?1",
            params![m.id, now()],
        )?;
    }
    Ok(())
}

/// The tool a hard-block names matches a tool call: the same name, or an MCP tool that ends with `__<name>`.
fn names(blocked: &str, tool: &str) -> bool {
    tool == blocked || tool.ends_with(&format!("__{blocked}"))
}

/// A watch rule's trigger: one of its tools on a path that has the rule's path as a part.
// shortcut: the path is matched as text in the absolute path (`/crates/` anywhere), not against the repository root;
// upgrade when a rule needs a root-only match
fn triggers(rule: &Rule, used: &ToolUse) -> bool {
    rule.tools.split('|').any(|t| t == used.tool)
        && used.path.is_some_and(|p| {
            rule.path.is_empty() || p.contains(&format!("/{}", rule.path.trim_start_matches('/')))
        })
}

/// A peer's id: its session id, and `/<agent id>` for a subagent of that session (the text of `db::PEER`).
#[must_use]
pub fn peer_key(session: &str, agent: Option<&str>) -> String {
    match agent {
        Some(agent) => format!("{session}/{agent}"),
        None => session.to_string(),
    }
}

/// The session of a peer id: a subagent works in its session's binding.
fn session_of(peer: &str) -> &str {
    peer.split_once('/').map_or(peer, |(session, _)| session)
}

/// The sessions and their subagents that `thread peers` reads, by peer id (`peer_key`).
pub(crate) struct Presence {
    /// The newest hook call, tool call or release of each peer.
    pub(crate) seen: BTreeMap<String, EventRow>,
    tools: BTreeMap<String, EventRow>,
    workspaces: Vec<Workspace>,
    /// (session, dossier) of each open binding.
    bound: Vec<(String, String)>,
    /// (session, thread, name) of each active member of an open thread.
    members: Vec<(String, i64, String)>,
    /// The agent and account of each session that legionctl started.
    started: BTreeMap<String, (String, Option<String>)>,
}

/// running, idle, ended or unknown: from the session's last hook call, tool call or release.
pub(crate) fn live_state(p: &Presence, session: &str) -> &'static str {
    match p.seen.get(session) {
        Some(e) => match (e.kind.as_str(), e.payload["input"]["event"].as_str()) {
            ("dossier.release", _) => "ended",
            ("internal.agent.tool", _) | (_, Some("prompt")) => "running",
            _ => "idle",
        },
        None => "unknown",
    }
}

impl Presence {
    fn workspace_of(&self, dir: &Path) -> Option<Workspace> {
        self.workspaces
            .iter()
            .find(|w| dir.starts_with(&w.path))
            .cloned()
    }

    /// The dossier a session writes (its binding), else the dossier of the workspace it works in.
    fn dossier_of(&self, peer: &str, project: Option<&Path>) -> Option<String> {
        self.bound
            .iter()
            .find(|(s, _)| s == session_of(peer))
            .map(|(_, d)| d.clone())
            .or_else(|| {
                project
                    .and_then(|dir| self.workspace_of(dir))
                    .map(|w| w.dossier)
            })
    }
}

impl Dossiers {
    pub(crate) fn begin(&self) -> Result<Transaction<'_>> {
        Ok(Transaction::new_unchecked(
            self.db(),
            TransactionBehavior::Immediate,
        )?)
    }

    /// The open threads where `session` is an active member, with its name there.
    pub(crate) fn memberships(&self, session: &str) -> Result<Vec<(i64, String)>> {
        Ok(self
            .db()
            .prepare(
                "SELECT m.thread, m.name FROM members m JOIN threads t ON t.id = m.thread
                 WHERE m.session = ?1 AND m.state = 'active' AND t.state = 'open' ORDER BY m.thread",
            )?
            .query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    pub(crate) fn member(
        &self,
        thread: i64,
        name: &str,
    ) -> Result<Option<(Archetype, Option<String>, String)>> {
        let row: Option<(String, Option<String>, String)> = self
            .db()
            .query_row(
                "SELECT archetype, session, state FROM members WHERE thread = ?1 AND name = ?2",
                params![thread, name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        row.map(|(a, s, st)| Ok((Archetype::parse(&a)?, s, st)))
            .transpose()
    }

    /// A thread that takes writes: it exists and is open (an imported thread is read-only).
    fn writable(&self, thread: i64) -> Result<()> {
        let state: Option<String> = self
            .db()
            .query_row("SELECT state FROM threads WHERE id = ?1", [thread], |r| {
                r.get(0)
            })
            .optional()?;
        match state.as_deref() {
            Some("open") => Ok(()),
            Some(state) => bail!("thread {thread} is {state}: read-only"),
            None => bail!("no thread {thread}"),
        }
    }

    /// A new thread on `dossier`, with `session` as its main agent `main0`.
    ///
    /// # Errors
    /// The database write fails.
    pub fn thread_start(&self, session: &str, title: &str, dossier: &str) -> Result<i64> {
        let tx = self.begin()?;
        tx.execute(
            "INSERT INTO threads (title, dossier, created) VALUES (?1, ?2, ?3)",
            params![title, dossier, now()],
        )?;
        let id = tx.last_insert_rowid();
        log(
            &tx,
            id,
            "main0",
            "thread.start",
            &json!({"title": title, "dossier": dossier, "session": session}),
        )?;
        tx.commit()?;
        self.thread_join(
            id,
            "main0",
            Archetype::MainAgent,
            Some(session),
            Pace::OnTrigger,
        )?;
        Ok(id)
    }

    /// Add a member to an open thread.
    ///
    /// # Errors
    /// The thread is not open, the name is taken or not valid, or the database write fails.
    pub fn thread_join(
        &self,
        thread: i64,
        name: &str,
        archetype: Archetype,
        session: Option<&str>,
        pace: Pace,
    ) -> Result<()> {
        ensure!(
            crate::dossier::valid_name(name),
            "a member name must be a-z, 0-9 and '-'"
        );
        self.writable(thread)?;
        let tx = self.begin()?;
        tx.execute(
            "INSERT INTO members (thread, name, archetype, session, pace, state, joined) VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6)",
            params![thread, name, archetype.as_str(), session, pace.as_str(), now()],
        )
        .with_context(|| format!("thread {thread} has a member {name} already"))?;
        log(
            &tx,
            thread,
            name,
            "thread.join",
            &json!({"archetype": archetype.as_str(), "session": session, "pace": pace.as_str()}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A member leaves: it gets no wakes, and its open blocks expire, so it cannot hold the main agent.
    ///
    /// # Errors
    /// No such member, or the database write fails.
    pub fn thread_leave(&self, thread: i64, name: &str) -> Result<Vec<String>> {
        let tx = self.begin()?;
        let changed = tx.execute(
            "UPDATE members SET state = 'left' WHERE thread = ?1 AND name = ?2 AND state = 'active'",
            params![thread, name],
        )?;
        ensure!(changed == 1, "thread {thread} has no active member {name}");
        let expired = tx.execute(
            &format!(
                "UPDATE messages SET state = 'expired' WHERE thread = ?1 AND sender = ?2 AND ((kind = 'hard-block'
                 AND state IN ({OPEN})) OR (kind = 'soft-block' AND state = 'pending'))"
            ),
            params![thread, name],
        )?;
        log(
            &tx,
            thread,
            name,
            "thread.leave",
            &json!({"expired": expired}),
        )?;
        tx.commit()?;
        Ok(vec![format!(
            "{name} left thread {thread}; {expired} of its blocks expired"
        )])
    }

    /// Send a message. The sender is the caller's member in the thread (`thread`, else its only open thread); a
    /// session that is not a member may send a note or a hint to a member. Only a thread-observer sends blocks. A
    /// notice with `rule` counts a step that the sender did for the watched member and tells the counts.
    ///
    /// # Errors
    /// No thread, a kind that the sender may not send, an unknown receiver or rule, or the database write fails.
    pub fn thread_send(
        &self,
        session: &str,
        thread: Option<i64>,
        (to, kind, body): (&str, Kind, &str),
        tool: Option<&str>,
        (rule, teammate): (Option<i64>, Option<&str>),
    ) -> Result<i64> {
        let mine = self.memberships(session)?;
        let (thread, mut from) = match (thread, mine.as_slice()) {
            (Some(t), _) => (
                t,
                mine.iter().find(|(m, _)| *m == t).map(|(_, n)| n.clone()),
            ),
            (None, [(t, n)]) => (*t, Some(n.clone())),
            (None, []) => bail!("this session is in no thread; give --thread"),
            (None, _) => bail!("this session is in more than one thread; give --thread"),
        };
        self.writable(thread)?;
        if let Some(name) = teammate {
            let run: Option<String> = self
                .db()
                .query_row(
                    "SELECT run FROM members WHERE thread = ?1 AND name = ?2 AND state = 'active'",
                    params![thread, name],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(
                from.as_deref() == Some("main0") && run.as_deref() == Some("teammate"),
                "--teammate {name}: only a teammate of the lead (main0) of thread {thread} sends as itself"
            );
            from = Some(name.to_string());
        }
        let from_archetype = match &from {
            Some(name) => self.member(thread, name)?.map(|(a, _, _)| a),
            None => None,
        };
        let allowed = match kind {
            Kind::Note | Kind::Hint => true,
            Kind::SoftBlock | Kind::HardBlock => from_archetype == Some(Archetype::ThreadObserver),
            _ => from_archetype.is_some(),
        };
        ensure!(
            allowed,
            "a {} comes only from a {} of thread {thread}",
            kind.as_str(),
            if matches!(kind, Kind::SoftBlock | Kind::HardBlock) {
                "thread-observer member"
            } else {
                "member"
            }
        );
        let to_member = self.member(thread, to)?;
        ensure!(
            to_member
                .as_ref()
                .is_none_or(|(_, _, state)| state == "active"),
            "{to} is not an active member of thread {thread}"
        );
        ensure!(
            to_member.is_some() || !matches!(kind, Kind::SoftBlock | Kind::HardBlock),
            "a block goes to a member of thread {thread}; {to} is not one"
        );
        let from = from.unwrap_or_else(|| session.to_string());
        let tx = self.begin()?;
        let body = match rule {
            None => body.to_string(),
            Some(rule) => {
                ensure!(
                    kind == Kind::Notice,
                    "--rule counts a step done for the main agent: kind notice"
                );
                let changed = tx.execute(
                    "UPDATE watch_rules SET done = done + 1 WHERE id = ?1 AND thread = ?2 AND member = ?3",
                    params![rule, thread, from],
                )?;
                ensure!(
                    changed == 1,
                    "rule {rule} is not a rule of {from} in thread {thread}"
                );
                let (misses, done, created): (u32, u32, i64) = tx.query_row(
                    "SELECT misses, done, created FROM watch_rules WHERE id = ?1",
                    [rule],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                format!(
                    "{from}: {body} — {} miss since {}, {done} done for you",
                    ordinal(misses),
                    clock(created)
                )
            }
        };
        let id = post(&tx, thread, (&from, to), kind, &body, tool)?;
        if from_archetype == Some(Archetype::ThreadObserver) {
            crate::pool::effect(
                &tx,
                thread,
                &from,
                ("message", &id.to_string(), &id.to_string()),
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// The prompt hook: the messages to `session` that it did not see, as one block of context; each is then
    /// delivered and never shown again.
    ///
    /// # Errors
    /// The database fails.
    pub fn thread_prompt(&self, session: &str) -> Result<Option<String>> {
        let started = std::time::Instant::now();
        let tx = self.begin()?;
        let list = messages(&tx, &to_session("'pending'", TOLD), [session])?;
        delivered(&tx, &list)?;
        if list.is_empty() {
            tx.commit()?;
            return Ok(None);
        }
        let mut lines = vec![format!(
            "legionctl thread: {} new messages for you. Act on them; answer one with `legionctl thread ack <id>`.",
            list.len()
        )];
        lines.extend(list.iter().map(|m| {
            format!(
                "- #{} {} from {} (thread {}): {}",
                m.id,
                m.kind.as_str(),
                m.from,
                m.thread,
                m.body
            )
        }));
        let text = lines.join("\n");
        crate::pool::injected(&tx, &list, &text, started)?;
        tx.commit()?;
        Ok(Some(text))
    }

    /// The stop hook: each open hard-block, and each soft-block not yet given, stop the end of the reply; `again`
    /// (the reply goes on because a stop hook stopped it) gives the hard-blocks only. A stop wakes the members with
    /// pace on-stop.
    ///
    /// # Errors
    /// The database fails.
    pub fn thread_stop(&self, session: &str, again: bool) -> Result<Option<String>> {
        let tx = self.begin()?;
        let mut list = messages(&tx, &to_session(OPEN, "'hard-block'"), [session])?;
        if !again {
            list.extend(messages(
                &tx,
                &to_session("'pending'", "'soft-block'"),
                [session],
            )?);
            for (thread, name) in self.memberships(session)? {
                wake_paced(&tx, thread, &name, &[Pace::OnStop], "stop")?;
                crate::pool::reply_misses(&tx, thread, &name)?;
            }
        }
        delivered(&tx, &list)?;
        tx.commit()?;
        if list.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            list.iter()
                .map(|m| match m.kind {
                    Kind::HardBlock => format!(
                        "legionctl thread {}: hard-block #{} from {}: {} This stops each reply until you fix it and \
                         ack it: `legionctl thread ack {}`.",
                        m.thread, m.id, m.from, m.body, m.id
                    ),
                    _ => format!(
                        "legionctl thread {}: soft-block #{} from {}: {} Do this before you end the reply.",
                        m.thread, m.id, m.from, m.body
                    ),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }

    /// The pre-tool hook: why an open hard-block to `session` that names this tool refuses the call.
    ///
    /// # Errors
    /// The database fails.
    pub fn thread_gate(&self, session: &str, tool: &str) -> Result<Option<String>> {
        let list = messages(self.db(), &to_session(OPEN, "'hard-block'"), [session])?;
        Ok(list
            .into_iter()
            .find(|m| m.tool.as_deref().is_some_and(|t| names(t, tool)))
            .map(|m| {
                format!(
                    "legionctl thread {}: {} holds {tool} (hard-block #{}): {} Fix it, then ack it: `legionctl thread \
                     ack {}`.",
                    m.thread, m.from, m.id, m.body, m.id
                )
            }))
    }

    /// The receiver acks a message; with `reply`, a note goes back to the sender.
    ///
    /// # Errors
    /// The message is not to this session, or the database write fails.
    pub fn thread_ack(&self, session: &str, id: i64, reply: Option<&str>) -> Result<Vec<String>> {
        let found = messages(
            self.db(),
            &to_session(OPEN, &format!("{TOLD}, 'soft-block', 'hard-block'")),
            [session],
        )?
        .into_iter()
        .find(|m| m.id == id)
        .with_context(|| format!("message {id} is not an open message to this session"))?;
        let tx = self.begin()?;
        tx.execute(
            "UPDATE messages SET state = 'acked', acked = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
        log(
            &tx,
            found.thread,
            &found.to,
            "thread.ack",
            &json!({"id": id, "reply": reply}),
        )?;
        if let Some(text) = reply {
            post(
                &tx,
                found.thread,
                (&found.to, &found.from),
                Kind::Note,
                text,
                None,
            )?;
        }
        tx.commit()?;
        Ok(vec![format!("acked #{id} from {}", found.from)])
    }

    /// The caller announces a state in each of its threads; members with pace on-declare or on-stop wake.
    ///
    /// # Errors
    /// The session is in no thread, or the database write fails.
    pub fn thread_declare(
        &self,
        session: &str,
        word: Declared,
        note: Option<&str>,
    ) -> Result<Vec<String>> {
        let mine = self.memberships(session)?;
        ensure!(!mine.is_empty(), "this session is in no thread");
        if word == Declared::Done {
            for (thread, _) in &mine {
                let open = self.gate_failures(*thread, "declare:done")?;
                ensure!(
                    open.is_empty(),
                    "thread {thread}: not done yet; the gate declare:done has open items:\n{}",
                    open.join("\n")
                );
            }
        }
        let tx = self.begin()?;
        let mut out = Vec::new();
        for (thread, name) in mine {
            log(
                &tx,
                thread,
                &name,
                "thread.declare",
                &json!({"word": word.as_str(), "note": note, "session": session}),
            )?;
            wake_paced(
                &tx,
                thread,
                &name,
                &[Pace::OnDeclare, Pace::OnStop],
                &format!("declare {}", word.as_str()),
            )?;
            if matches!(word, Declared::Done | Declared::Ready) {
                crate::pool::trigger(&tx, thread, &format!("declare {}", word.as_str()))?;
            }
            if word == Declared::Done {
                crate::pool::stop_at(&tx, thread, "declare:done")?;
            }
            out.push(format!(
                "{name} declared {} in thread {thread}",
                word.as_str()
            ));
        }
        tx.commit()?;
        Ok(out)
    }

    /// Change a member's pace.
    ///
    /// # Errors
    /// No such member, or the database write fails.
    pub fn thread_pace(&self, thread: i64, name: &str, pace: Pace) -> Result<()> {
        self.writable(thread)?;
        let tx = self.begin()?;
        let changed = tx.execute(
            "UPDATE members SET pace = ?3 WHERE thread = ?1 AND name = ?2",
            params![thread, name, pace.as_str()],
        )?;
        ensure!(changed == 1, "thread {thread} has no member {name}");
        log(
            &tx,
            thread,
            name,
            "thread.pace",
            &json!({"pace": pace.as_str()}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Add a watch rule of `rule.member` on `rule.watched`.
    ///
    /// # Errors
    /// A member is missing, `every` is 0, or the database write fails.
    pub fn rule_add(&self, thread: i64, rule: &NewRule) -> Result<i64> {
        ensure!(rule.every > 0, "--every must be 1 or more");
        self.writable(thread)?;
        for name in [rule.member, rule.watched] {
            ensure!(
                self.member(thread, name)?.is_some(),
                "thread {thread} has no member {name}"
            );
        }
        let tx = self.begin()?;
        tx.execute(
            "INSERT INTO watch_rules (thread, member, watched, tools, path, expect, every, kind, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![thread, rule.member, rule.watched, rule.tools, rule.path, rule.expect, rule.every, rule.kind.as_str(), now()],
        )?;
        let id = tx.last_insert_rowid();
        log(
            &tx,
            thread,
            rule.member,
            "thread.rule",
            &json!({"id": id, "watched": rule.watched, "tools": rule.tools, "path": rule.path, "expect": rule.expect,
                    "every": rule.every, "kind": rule.kind.as_str()}),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Remove a watch rule.
    ///
    /// # Errors
    /// No such rule, or the database write fails.
    pub fn rule_rm(&self, id: i64) -> Result<()> {
        let tx = self.begin()?;
        let thread: i64 = tx
            .query_row("SELECT thread FROM watch_rules WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .with_context(|| format!("no rule {id}"))?;
        tx.execute("DELETE FROM watch_rules WHERE id = ?1", [id])?;
        log(&tx, thread, "", "thread.rule.rm", &json!({"id": id}))?;
        tx.commit()?;
        Ok(())
    }

    /// The watch rules of a thread with their counts.
    ///
    /// # Errors
    /// The database read fails.
    pub fn rules(&self, thread: i64) -> Result<Vec<Rule>> {
        self.db()
            .prepare(
                "SELECT id, thread, member, watched, tools, path, expect, every, kind, created, run, misses, done, reply
                 FROM watch_rules WHERE thread = ?1 ORDER BY id",
            )?
            .query_map([thread], |r| {
                Ok((
                    (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?),
                    (r.get(7)?, r.get::<_, String>(8)?, r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?, r.get(13)?),
                ))
            })?
            .map(|row| {
                let ((id, thread, member, watched, tools, path, expect), (every, kind, created, run, misses, done, reply)) = row?;
                Ok(Rule {
                    id,
                    thread,
                    member,
                    watched,
                    tools,
                    path,
                    expect,
                    every,
                    kind: Kind::parse(&kind)?,
                    created,
                    run,
                    misses,
                    done,
                    reply,
                })
            })
            .collect()
    }

    /// The post-tool hook: a `SendMessage` call goes into the log of each thread of the session; the watch rules on
    /// the session's member count the call, and a rule whose count reaches its `every` sends its message and wakes
    /// its member when its pace is on-trigger.
    ///
    /// # Errors
    /// The database fails.
    pub fn tool_used(&self, session: &str, used: &ToolUse) -> Result<()> {
        let mine = self.memberships(session)?;
        if mine.is_empty() {
            return Ok(());
        }
        let tx = self.begin()?;
        for (thread, name) in mine {
            crate::pool::test_change(&tx, thread, &name, used)?;
            if used.tool == "SendMessage" {
                log(
                    &tx,
                    thread,
                    &name,
                    "thread.message",
                    &json!({"to": used.to, "text": used.message, "channel": "claude-inbox", "sent": used.ok}),
                )?;
            }
            for rule in self
                .rules(thread)?
                .into_iter()
                .filter(|r| r.watched == name)
            {
                if triggers(&rule, used) {
                    let run = rule.run + 1;
                    // a reply rule counts its miss at the stop (`pool::reply_misses`)
                    if run < rule.every || rule.reply {
                        tx.execute(
                            "UPDATE watch_rules SET run = ?2 WHERE id = ?1",
                            params![rule.id, run],
                        )?;
                        continue;
                    }
                    let misses = rule.misses + 1;
                    tx.execute(
                        "UPDATE watch_rules SET run = 0, misses = ?2 WHERE id = ?1",
                        params![rule.id, misses],
                    )?;
                    let of = if rule.path.is_empty() {
                        String::new()
                    } else {
                        format!(" of `{}`", rule.path)
                    };
                    let body = format!(
                        "{}: no `{}` after {} edits{of} — {} time since {}",
                        rule.member,
                        rule.expect,
                        rule.every,
                        ordinal(misses),
                        clock(rule.created)
                    );
                    post(&tx, thread, (&rule.member, &name), rule.kind, &body, None)?;
                    let pace: String = tx.query_row(
                        "SELECT pace FROM members WHERE thread = ?1 AND name = ?2 AND state = 'active'",
                        params![thread, rule.member],
                        |r| r.get(0),
                    )?;
                    if Pace::parse(&pace)? == Pace::OnTrigger {
                        wake(&tx, thread, &rule.member, &format!("rule {}", rule.id))?;
                    }
                } else if used.tool == "Bash"
                    && used.command.is_some_and(|c| c.contains(&rule.expect))
                {
                    tx.execute("UPDATE watch_rules SET run = 0 WHERE id = ?1", [rule.id])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The messages to `session` that it did not ack.
    ///
    /// # Errors
    /// The database read fails.
    pub fn inbox(&self, session: &str) -> Result<Vec<Message>> {
        messages(
            self.db(),
            &to_session(OPEN, &format!("{TOLD}, 'soft-block', 'hard-block'")),
            [session],
        )
    }

    /// Every thread, newest first.
    ///
    /// # Errors
    /// The database read fails.
    pub fn threads(&self) -> Result<Vec<ThreadInfo>> {
        Ok(self
            .db()
            .prepare(
                // the account: the newest import's (a copy keeps the rows of the imports before it), else the main
                // session's
                "SELECT id, title, dossier, created, state,
                 coalesce((SELECT json_extract(payload, '$.account') FROM thread_log l
                           WHERE l.thread = t.id AND l.kind = 'thread.import' ORDER BY l.id DESC LIMIT 1),
                          (SELECT s.account FROM members m JOIN sessions s ON s.id = m.session
                           WHERE m.thread = t.id AND m.name = 'main0')),
                 (SELECT json_extract(payload, '$.from') FROM thread_log l
                  WHERE l.thread = t.id AND l.kind = 'thread.import' ORDER BY l.id DESC LIMIT 1)
                 FROM threads t ORDER BY id DESC",
            )?
            .query_map([], |r| {
                Ok(ThreadInfo {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    dossier: r.get(2)?,
                    created: r.get(3)?,
                    state: r.get(4)?,
                    account: r.get(5)?,
                    imported_from: r.get(6)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    /// Copy `thread` to the account profile `account` (design 23, C5): a new thread with its log, members (without
    /// their sessions, which run on the old account), messages and watch rules, and the main session's transcript in
    /// the account's folders. The copy's log ends with `thread.import`; the original becomes `imported`, read-only.
    ///
    /// # Errors
    /// No such thread, it was imported already (the copy exists), it runs on `account`, no such account profile, or
    /// a copy or the database write fails.
    pub fn thread_import(&self, thread: i64, account: &str) -> Result<i64> {
        let info = self
            .threads()?
            .into_iter()
            .find(|t| t.id == thread)
            .with_context(|| format!("no thread {thread}"))?;
        if info.state == "imported" {
            let copy: Option<i64> = self
                .db()
                .query_row(
                    "SELECT thread FROM thread_log WHERE kind = 'thread.import' AND json_extract(payload, '$.from') = ?1
                     ORDER BY id DESC LIMIT 1",
                    [thread],
                    |r| r.get(0),
                )
                .optional()?;
            bail!(
                "thread {thread} was imported to thread {} already: a thread is imported once",
                copy.map_or_else(|| "?".into(), |c| c.to_string())
            );
        }
        self.writable(thread)?;
        ensure!(
            info.account.as_deref() != Some(account),
            "thread {thread} runs on account {account} already"
        );
        crate::session::account_dir(account)?;
        let main: Option<String> = self
            .db()
            .query_row(
                "SELECT session FROM members WHERE thread = ?1 AND name = 'main0'",
                [thread],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(session) = &main {
            crate::session::copy_transcript_to(session, info.account.as_deref(), account)?;
        }
        let tx = self.begin()?;
        tx.execute(
            "INSERT INTO threads (title, dossier, created) VALUES (?1, ?2, ?3)",
            params![info.title, info.dossier, now()],
        )?;
        let copy = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO thread_log (thread, ts, member, kind, payload)
             SELECT ?1, ts, member, kind, payload FROM thread_log WHERE thread = ?2 ORDER BY id",
            params![copy, thread],
        )?;
        tx.execute(
            "INSERT INTO messages (thread, sender, recipient, kind, body, tool, state, created, delivered, acked)
             SELECT ?1, sender, recipient, kind, body, tool, state, created, delivered, acked FROM messages
             WHERE thread = ?2 ORDER BY id",
            params![copy, thread],
        )?;
        tx.execute(
            "INSERT INTO watch_rules (thread, member, watched, tools, path, expect, every, kind, created, run, misses, done)
             SELECT ?1, member, watched, tools, path, expect, every, kind, created, run, misses, done FROM watch_rules
             WHERE thread = ?2 ORDER BY id",
            params![copy, thread],
        )?;
        let imported = log(
            &tx,
            copy,
            "",
            "thread.import",
            &json!({"from": thread, "account": account, "session": main}),
        )?;
        // the members start at the import: the copied rows are their history
        tx.execute(
            "INSERT INTO members (thread, name, archetype, session, pace, cursor, state, joined)
             SELECT ?1, name, archetype, NULL, pace, ?3, state, joined FROM members WHERE thread = ?2",
            params![copy, thread, imported],
        )?;
        tx.execute(
            "UPDATE threads SET state = 'imported' WHERE id = ?1",
            [thread],
        )?;
        log(
            &tx,
            thread,
            "",
            "thread.imported",
            &json!({"to": copy, "account": account}),
        )?;
        tx.commit()?;
        Ok(copy)
    }

    /// Show the view `view` (with its `params` and the repository `repo` of its layers) in pane 3 of the thread's
    /// screens: a `views.place` row of the log, so the layout goes with the thread (and its imports).
    ///
    /// # Errors
    /// The thread is not open, or the database write fails.
    pub fn thread_place(
        &self,
        thread: i64,
        view: &crate::views::Placed,
        repo: &Path,
    ) -> Result<()> {
        self.writable(thread)?;
        let tx = self.begin()?;
        log(
            &tx,
            thread,
            "",
            "views.place",
            &json!({"view": view.name, "params": view.params, "repo": repo}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// One thread: its members, messages, rules and log.
    ///
    /// # Errors
    /// No such thread, or the database read fails.
    pub fn thread_show(&self, id: i64) -> Result<ThreadView> {
        let thread = self
            .threads()?
            .into_iter()
            .find(|t| t.id == id)
            .with_context(|| format!("no thread {id}"))?;
        let members = self
            .db()
            .prepare(
                "SELECT name, archetype, session, pace, cursor, state, joined, (SELECT count(*) FROM thread_log l
                 WHERE l.thread = m.thread AND l.member = m.name AND l.kind = 'thread.wake'), config, run FROM members m
                 WHERE thread = ?1 ORDER BY joined, name",
            )?
            .query_map([id], |r| {
                Ok(((r.get(0)?, r.get::<_, String>(1)?, r.get(2)?, r.get::<_, String>(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?), (r.get(8)?, r.get(9)?)))
            })?
            .map(|row| {
                let ((name, archetype, session, pace, cursor, state, joined, wakes), (config, run)) = row?;
                Ok(Member {
                    name,
                    archetype: Archetype::parse(&archetype)?,
                    session,
                    pace: Pace::parse(&pace)?,
                    cursor,
                    state,
                    joined,
                    wakes,
                    config,
                    run,
                })
            })
            .collect::<Result<_>>()?;
        let messages = messages(
            self.db(),
            "SELECT id, thread, sender, recipient, kind, body, tool, state, created FROM messages WHERE thread = ?1
             ORDER BY id",
            [id],
        )?;
        let log = self
            .db()
            .prepare("SELECT id, ts, member, kind, payload FROM thread_log WHERE thread = ?1 ORDER BY id")?
            .query_map([id], |r| {
                let payload: String = r.get(4)?;
                Ok(LogRow {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    member: r.get(2)?,
                    kind: r.get(3)?,
                    payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(ThreadView {
            thread,
            members,
            messages,
            rules: self.rules(id)?,
            log,
            costs: self.costs(id)?,
            effects: self.effects(id)?,
        })
    }

    /// What the store and the operator's event log (`events`) know about the live sessions: the last hook call and
    /// tool call of each within the dossier stale time, the open bindings, thread members and started sessions.
    pub(crate) fn presence(&self, events: &Db) -> Result<Presence> {
        let since = now() - self.stale_after();
        let by_peer = |rows: Vec<EventRow>| -> BTreeMap<String, EventRow> {
            rows.into_iter()
                .filter_map(|e| {
                    let input = &e.payload["input"];
                    let key = peer_key(input["session"].as_str()?, input["agent"].as_str());
                    Some((key, e))
                })
                .collect()
        };
        Ok(Presence {
            seen: by_peer(events.newest_per_peer(
                &["internal.hooks.run", "internal.agent.tool", "dossier.release"],
                since,
            )?),
            tools: by_peer(events.newest_per_peer(&["internal.agent.tool"], since)?),
            workspaces: self.workspaces(None)?,
            bound: self
                .db()
                .prepare("SELECT session, dossier FROM bindings WHERE released IS NULL")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?,
            members: self
                .db()
                .prepare(
                    "SELECT m.session, m.thread, m.name FROM members m JOIN threads t ON t.id = m.thread
                     WHERE m.session IS NOT NULL AND t.state = 'open' AND m.state = 'active'",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?,
            started: self
                .db()
                .prepare("SELECT id, agent, account FROM sessions")?
                .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
                .collect::<Result<_, _>>()?,
        })
    }

    /// The sessions that share a dossier, a workspace or a thread with `me` (working in `here`), or with the scope
    /// that `scope` names, with their state.
    ///
    /// # Errors
    /// A database read fails, or `--topic` names no open workspace of the caller's dossier.
    pub fn peers(&self, events: &Db, me: &str, here: &Path, scope: &Scope) -> Result<Vec<Peer>> {
        let p = self.presence(events)?;
        let explicit = scope.dossier.is_some() || scope.topic.is_some() || scope.thread.is_some();
        let dossier = match &scope.dossier {
            Some(d) => Some(d.clone()),
            None if !explicit => p.dossier_of(me, Some(here)),
            None => None,
        };
        let workspace = match &scope.topic {
            Some(t) => {
                let d = p
                    .dossier_of(me, Some(here))
                    .context("this session has no dossier for --topic")?;
                Some(
                    p.workspaces
                        .iter()
                        .find(|w| w.dossier == d && w.topic == *t)
                        .cloned()
                        .with_context(|| {
                            format!("dossier {d} has no open workspace with topic {t}")
                        })?,
                )
            }
            None if !explicit => p.workspace_of(here),
            None => None,
        };
        let threads: BTreeSet<i64> = match scope.thread {
            Some(t) => [t].into(),
            None if !explicit => p
                .members
                .iter()
                .filter(|(s, _, _)| s == me)
                .map(|(_, t, _)| *t)
                .collect(),
            None => BTreeSet::new(),
        };
        let candidates: BTreeSet<&String> = p
            .seen
            .keys()
            .chain(p.bound.iter().map(|(s, _)| s))
            .chain(p.members.iter().map(|(s, _, _)| s))
            .filter(|s| *s != me && !s.is_empty())
            .collect();
        let mut peers = Vec::new();
        for session in candidates {
            let peer = self.peer(&p, session)?;
            let mut shares = Vec::new();
            if let (Some(d), Some(theirs)) = (&dossier, &peer.dossier)
                && d == theirs
            {
                shares.push(format!("dossier {d}"));
            }
            if let (Some(w), Some(theirs)) = (&workspace, &peer.workspace)
                && w.path == *theirs
            {
                shares.push(format!("workspace {}", w.path.display()));
            }
            shares.extend(
                p.members
                    .iter()
                    .filter(|(s, t, _)| s == session && threads.contains(t))
                    .map(|(_, t, _)| format!("thread {t}")),
            );
            if !shares.is_empty() {
                peers.push(Peer { shares, ..peer });
            }
        }
        Ok(peers)
    }

    /// One session or subagent as a peer, before the scope it shares is known.
    fn peer(&self, p: &Presence, session: &str) -> Result<Peer> {
        let project = p
            .seen
            .get(session)
            .and_then(|e| e.payload["input"]["project"].as_str())
            .map(PathBuf::from);
        let workspace = project.as_deref().and_then(|dir| p.workspace_of(dir));
        let (state, seen) = (live_state(p, session), p.seen.get(session).map(|e| e.ts));
        let tool = |field: &str| {
            p.tools
                .get(session)
                .and_then(|e| e.payload["input"][field].as_str())
                .map(String::from)
        };
        let declared: Option<String> = self
            .db()
            .query_row(
                "SELECT json_extract(payload, '$.word') FROM thread_log WHERE kind = 'thread.declare'
                 AND json_extract(payload, '$.session') = ?1 ORDER BY id DESC LIMIT 1",
                [session],
                |r| r.get(0),
            )
            .optional()?;
        let (agent, account) = p
            .started
            .get(session_of(session))
            .cloned()
            .unwrap_or_else(|| ("claude".into(), None));
        let mut channels = vec!["outbox".to_string()];
        if agent == "claude" {
            channels.push("claude-inbox".into());
        }
        Ok(Peer {
            session: session.to_string(),
            agent,
            subagent: p
                .seen
                .get(session)
                .and_then(|e| e.payload["input"]["agent_type"].as_str())
                .map(String::from),
            account,
            dossier: p.dossier_of(session, project.as_deref()),
            topic: workspace.as_ref().map(|w| w.topic.clone()),
            workspace: workspace.map(|w| w.path),
            members: p
                .members
                .iter()
                .filter(|(s, _, _)| s == session)
                .map(|(_, t, n)| format!("{t}/{n}"))
                .collect(),
            shares: Vec::new(),
            state: state.into(),
            seen,
            last_tool: tool("tool"),
            last_path: tool("path"),
            declared,
            pending: u32::try_from(self.inbox(session)?.len()).unwrap_or(u32::MAX),
            channels,
        })
    }

    /// One peer (a peer id, or a member name in one of the caller's threads): its state and its newest events,
    /// the same rows as `trace`.
    ///
    /// # Errors
    /// The peer is not a peer of the caller, or a database read fails.
    pub fn observe(&self, events: &Db, me: &str, here: &Path, peer: &str) -> Result<Observed> {
        let mut by_name = None;
        for (thread, _) in self.memberships(me)? {
            if let Some((_, Some(session), _)) = self.member(thread, peer)? {
                by_name = Some(session);
            }
        }
        let session = by_name.unwrap_or_else(|| peer.to_string());
        let peer = self
            .peers(events, me, here, &Scope::default())?
            .into_iter()
            .find(|p| p.session == session)
            .with_context(|| {
                format!("{peer} is not a peer of this session (see `legionctl thread peers`)")
            })?;
        let events = events.events_of_peer(&session, 50)?;
        Ok(Observed { peer, events })
    }
}
