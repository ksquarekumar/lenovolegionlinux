//! Pools (design 23, B+4, P6b and P6c): a thread started from a team file, with members that run. A member runs as
//! its own operator ACP session (`operator`: the operator wakes it with one prompt per wake, within its budget), as a
//! Claude Code teammate of a Claude lead (`teammate`, team mode: its hooks deliver), or as a Codex subagent of a Codex
//! main session (`subagent`). The flow of the team gates the main agent's checkpoint and `declare done`, ends the
//! pool, and starts a failed member again within its limit. The members' work for the main agent are effects that
//! `thread compensate` undoes: a backfill line in the main agent's dossier, a message.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use crate::archetypes::{Condition, Flow, condition, seconds};
use crate::db::{Db, now};
use crate::dossier::Dossiers;
use crate::thread::{
    Archetype, Kind, Message, OPEN, Pace, ToolUse, live_state, log, messages, post, wake,
};

/// The pool's columns and tables (operator.db v12).
macro_rules! tables {
    () => {
        "
     -- pools (design 23, P6b): the team, its flow (JSON), where the main agent works, and the thread it branched from
     ALTER TABLE threads ADD COLUMN team TEXT;
     ALTER TABLE threads ADD COLUMN flow TEXT NOT NULL DEFAULT '{}';
     ALTER TABLE threads ADD COLUMN workdir TEXT;
     ALTER TABLE threads ADD COLUMN parent INTEGER;
     -- run: self, operator, teammate or subagent; agent: its session.acp agent; fields: its resolved config; acp: its
     -- ACP session; due: the log id of a wake that the operator did not run yet
     ALTER TABLE members ADD COLUMN config TEXT;
     ALTER TABLE members ADD COLUMN run TEXT NOT NULL DEFAULT 'self';
     ALTER TABLE members ADD COLUMN agent TEXT;
     ALTER TABLE members ADD COLUMN fields TEXT NOT NULL DEFAULT '{}';
     ALTER TABLE members ADD COLUMN acp TEXT;
     ALTER TABLE members ADD COLUMN due INTEGER;
     -- reply: the expected command must come before the reply ends (the stop), not within `every` calls
     ALTER TABLE watch_rules ADD COLUMN reply INTEGER NOT NULL DEFAULT 0;
     -- the cost of each wake (kind wake) and of the context the hooks gave a member (kind inject); `over`: the budget
     -- a wake reached. Not thread events: no wake counts or shows them
     CREATE TABLE costs (
        thread INTEGER NOT NULL,
        member TEXT NOT NULL,
        ts     INTEGER NOT NULL,
        kind   TEXT NOT NULL,
        tokens INTEGER NOT NULL,
        ms     INTEGER NOT NULL,
        over   TEXT
     );
     -- what a member did for another, and how `thread compensate` undoes it
     CREATE TABLE effects (
        id      INTEGER PRIMARY KEY,
        thread  INTEGER NOT NULL,
        member  TEXT NOT NULL,
        kind    TEXT NOT NULL,
        target  TEXT NOT NULL,
        undo    TEXT NOT NULL,
        state   TEXT NOT NULL DEFAULT 'done',
        created INTEGER NOT NULL
     );"
    };
}
pub(crate) use tables;

/// How a team member runs (a member of `thread join` has its own session: run `self` in the database).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// An operator ACP session, one prompt per wake.
    Operator,
    /// A Claude Code teammate of the main agent (team mode).
    Teammate,
    /// A Codex subagent of the main Codex session.
    Subagent,
}

impl Run {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Run::Operator => "operator",
            Run::Teammate => "teammate",
            Run::Subagent => "subagent",
        }
    }

    /// The run of a team's members when the main agent is `agent`: a Claude main agent leads a Claude Code agent team
    /// (user decision 4), a Codex one starts Codex subagents (decision 6), any other runs them as operator sessions.
    #[must_use]
    pub fn for_main(agent: &str) -> Run {
        match agent {
            "claude" => Run::Teammate,
            "codex" => Run::Subagent,
            _ => Run::Operator,
        }
    }
}

/// One member of a team, resolved (`runnables check`).
pub struct NewMember<'a> {
    pub name: &'a str,
    pub archetype: Archetype,
    pub config: &'a str,
    /// The `session.acp` agent of an operator member.
    pub agent: &'a str,
    pub run: Run,
    pub fields: &'a serde_json::Map<String, Value>,
}

/// What `thread start --team` starts.
pub struct TeamStart<'a> {
    pub session: &'a str,
    pub title: &'a str,
    pub dossier: &'a str,
    pub workdir: &'a Path,
    pub team: &'a str,
    pub flow: &'a Flow,
    pub members: Vec<NewMember<'a>>,
}

/// The cost of one member and kind, summed over the thread.
pub struct Cost {
    pub member: String,
    /// wake (an ACP wake), rlcd (an rlcd step of a wake) or inject (the context its hooks gave the main agent).
    pub kind: String,
    /// Wakes or rlcd steps that ran, or prompts that got thread context.
    pub runs: u32,
    pub tokens: i64,
    pub ms: i64,
    /// Wakes stopped at their budget.
    pub over: u32,
    /// Runs that failed.
    pub failed: u32,
}

pub struct Effect {
    pub id: i64,
    pub member: String,
    /// backfill or message.
    pub kind: String,
    /// The dossier file of a backfill, the message id of a message.
    pub target: String,
    /// done or undone.
    pub state: String,
}

/// One wake of an operator member, as the operator runs it.
pub struct Wake {
    pub thread: i64,
    pub member: String,
    pub archetype: Archetype,
    /// Its session; none before its first wake.
    pub session: Option<String>,
    pub agent: String,
    /// The dossier it writes: `<thread dossier>--<member>`.
    pub dossier: String,
    pub workdir: PathBuf,
    pub acp: Option<String>,
    pub fields: Value,
    pub tokens: Option<u64>,
    pub seconds: Option<u64>,
    /// The wake request it answers, the newest log row it covers, and whether newer rows wait for the next wake.
    due: i64,
    to: i64,
    more: bool,
    /// The prompt without the instruction text.
    pub prompt: String,
}

/// How the rlcd step of a wake ended (todo-138): its cost, its thread event (`thread.rlcd`), and whether the wake
/// ends with it (ignore, note, a failure, or an escalate with no next step). A failed step clears the wake request:
/// the member goes on (no restart, its blocks stay) and its events stay after the cursor.
pub struct RlcdEnd {
    pub tokens: u64,
    pub ms: u64,
    pub error: Option<String>,
    pub event: Value,
    pub ends: bool,
}

/// How a wake ended.
pub struct WakeEnd {
    pub tokens: u64,
    pub ms: u64,
    /// `tokens` or `seconds` when the wake stopped at that budget.
    pub over: Option<&'static str>,
    pub error: Option<String>,
    pub acp: Option<String>,
}

/// A rough token count (4 characters a token).
// shortcut: the gateway has no counter for text that it did not proxy; upgrade when the members' usage comes from
// their sessions (`agents usage`)
#[must_use]
pub fn tokens(text: &str) -> u64 {
    u64::try_from(text.len().div_ceil(4)).unwrap_or(u64::MAX)
}

/// A test file: in a `tests` folder, or named `test_*`, `*_test.*` or `*.test.*`.
// shortcut: by name, not by the repository's test runners (design 23, E3); upgrade with the runners in the graph
fn is_test(path: &str) -> bool {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    p.components().any(|c| c.as_os_str() == "tests")
        || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
}

fn flow_of(tx: &rusqlite::Connection, thread: i64) -> Result<Flow> {
    let text: String = tx.query_row("SELECT flow FROM threads WHERE id = ?1", [thread], |r| {
        r.get(0)
    })?;
    Ok(serde_json::from_str(&text)?)
}

/// Wake the active thread-observers of `thread` that sleep until a trigger (B+3: a checkpoint, a test change, a
/// declare done or ready, a missed step).
pub(crate) fn trigger(tx: &Transaction, thread: i64, reason: &str) -> Result<()> {
    let names: Vec<String> = tx
        .prepare(
            "SELECT name FROM members WHERE thread = ?1 AND state = 'active' AND archetype = ?2 AND pace = ?3",
        )?
        .query_map(
            params![
                thread,
                Archetype::ThreadObserver.as_str(),
                Pace::OnTrigger.as_str()
            ],
            |r| r.get(0),
        )?
        .collect::<Result<_, _>>()?;
    for name in names {
        wake(tx, thread, &name, reason)?;
    }
    Ok(())
}

/// At the stop of `name`: each reply rule on it whose tool calls came without the expected command is a miss, which
/// wakes the rule's member to do the step (or nudge).
pub(crate) fn reply_misses(tx: &Transaction, thread: i64, name: &str) -> Result<()> {
    let missed: Vec<(i64, String, String)> = tx
        .prepare(
            "SELECT id, member, expect FROM watch_rules WHERE thread = ?1 AND watched = ?2 AND reply = 1 AND run > 0",
        )?
        .query_map(params![thread, name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (id, member, expect) in missed {
        tx.execute(
            "UPDATE watch_rules SET run = 0, misses = misses + 1 WHERE id = ?1",
            [id],
        )?;
        log(
            tx,
            thread,
            name,
            "thread.miss",
            &json!({"rule": id, "expect": expect}),
        )?;
        let active: Option<String> = tx
            .query_row(
                "SELECT pace FROM members WHERE thread = ?1 AND name = ?2 AND state = 'active'",
                params![thread, member],
                |r| r.get(0),
            )
            .optional()?;
        if active.is_some() {
            wake(
                tx,
                thread,
                &member,
                &format!("stop: rule {id} missed `{expect}`"),
            )?;
        }
    }
    Ok(())
}

/// The main agent changed a test file: a thread-observer wakes for its test review.
pub(crate) fn test_change(tx: &Transaction, thread: i64, name: &str, used: &ToolUse) -> Result<()> {
    let edits = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
    let Some(path) = used
        .path
        .filter(|p| edits.contains(&used.tool) && is_test(p))
    else {
        return Ok(());
    };
    if name != "main0" {
        return Ok(());
    }
    log(
        tx,
        thread,
        name,
        "thread.test-change",
        &json!({"tool": used.tool, "path": path}),
    )?;
    trigger(tx, thread, &format!("test change {path}"))
}

fn cost(
    tx: &Transaction,
    (thread, member): (i64, &str),
    kind: &str,
    (tokens, ms, over, error): (u64, u64, Option<&str>, Option<&str>),
) -> Result<()> {
    tx.execute(
        "INSERT INTO costs (thread, member, ts, kind, tokens, ms, over, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            thread,
            member,
            now(),
            kind,
            i64::try_from(tokens)?,
            i64::try_from(ms)?,
            over,
            error
        ],
    )?;
    Ok(())
}

/// The context that the prompt hook gave to its session: its size and the hook's time, per thread.
pub(crate) fn injected(
    tx: &Transaction,
    list: &[Message],
    text: &str,
    started: Instant,
) -> Result<()> {
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut threads: Vec<(i64, &str)> = list.iter().map(|m| (m.thread, m.to.as_str())).collect();
    threads.dedup();
    for (thread, to) in threads {
        cost(tx, (thread, to), "inject", (tokens(text), ms, None, None))?;
    }
    Ok(())
}

/// Record an effect: what `member` did, and how to undo it.
pub(crate) fn effect(
    tx: &Transaction,
    thread: i64,
    member: &str,
    (kind, target, undo): (&str, &str, &str),
) -> Result<i64> {
    tx.execute(
        "INSERT INTO effects (thread, member, kind, target, undo, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![thread, member, kind, target, undo, now()],
    )?;
    Ok(tx.last_insert_rowid())
}

/// The open blocks of `member` expire, so it cannot hold the main agent.
fn expire(tx: &Transaction, thread: i64, member: &str) -> Result<usize> {
    Ok(tx.execute(
        &format!(
            "UPDATE messages SET state = 'expired' WHERE thread = ?1 AND sender = ?2 AND ((kind = 'hard-block'
             AND state IN ({OPEN})) OR (kind = 'soft-block' AND state = 'pending'))"
        ),
        params![thread, member],
    )?)
}

/// End the pool when `why` is one of its stop conditions: the thread closes, its members end, their blocks expire.
pub(crate) fn stop_at(tx: &Transaction, thread: i64, why: &str) -> Result<()> {
    if !flow_of(tx, thread)?.stop.iter().any(|s| s == why) {
        return Ok(());
    }
    let members: Vec<String> = tx
        .prepare("SELECT name FROM members WHERE thread = ?1 AND state = 'active'")?
        .query_map([thread], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for member in &members {
        expire(tx, thread, member)?;
    }
    tx.execute(
        "UPDATE members SET state = 'ended', due = NULL WHERE thread = ?1 AND state = 'active'",
        [thread],
    )?;
    tx.execute(
        "UPDATE threads SET state = 'closed' WHERE id = ?1",
        [thread],
    )?;
    log(
        tx,
        thread,
        "",
        "thread.stop",
        &json!({"why": why, "members": members}),
    )?;
    Ok(())
}

/// A wake saw its events: the cursor moves past them; the rows that did not fit wake it again now, for the same
/// request (true), else the request is answered.
fn seen(tx: &Transaction, w: &Wake) -> Result<bool> {
    tx.execute(
        "UPDATE members SET cursor = ?3 WHERE thread = ?1 AND name = ?2",
        params![w.thread, w.member, w.to],
    )?;
    if w.more {
        return Ok(true);
    }
    answered(tx, w)?;
    Ok(false)
}

/// The wake request is answered; a trigger during the wake keeps its own request.
fn answered(tx: &Transaction, w: &Wake) -> Result<()> {
    tx.execute(
        "UPDATE members SET due = NULL WHERE thread = ?1 AND name = ?2 AND due = ?3",
        params![w.thread, w.member, w.due],
    )?;
    Ok(())
}

/// A member's row when its wake begins.
struct Waking {
    archetype: String,
    session: Option<String>,
    agent: Option<String>,
    acp: Option<String>,
    fields: String,
    cursor: i64,
    due: Option<i64>,
}

/// Whether `member` used its tokens of the day: then its wake does not run, and with the stop `budget` it ends.
fn out_of_budget(tx: &Transaction, (thread, member): (i64, &str), budget: &Value) -> Result<bool> {
    let day: i64 = tx.query_row(
        "SELECT coalesce(sum(tokens), 0) FROM costs WHERE thread = ?1 AND member = ?2 AND ts >= ?3",
        params![thread, member, now() - now().rem_euclid(86_400)],
        |r| r.get(0),
    )?;
    let Some(limit) = budget["tokens_per_day"].as_i64().filter(|l| day >= *l) else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE members SET due = NULL WHERE thread = ?1 AND name = ?2",
        params![thread, member],
    )?;
    log(
        tx,
        thread,
        member,
        "thread.budget",
        &json!({"tokens_today": day, "tokens_per_day": limit}),
    )?;
    if flow_of(tx, thread)?.stop.iter().any(|s| s == "budget") {
        expire(tx, thread, member)?;
        tx.execute(
            "UPDATE members SET state = 'ended' WHERE thread = ?1 AND name = ?2",
            params![thread, member],
        )?;
    }
    Ok(true)
}

/// The lines of a wake prompt: the log rows after `cursor` up to `to`, the member's open messages, its rules.
/// The lines of a wake: the events after `cursor` that fit in `share` tokens (the first one cut to fit), the open
/// messages and the watch rules; and the last event given and the number of events that wait for the next wake.
fn wake_lines(
    tx: &Transaction,
    (thread, member): (i64, &str),
    (cursor, share): (i64, Option<u64>),
) -> Result<([Vec<String>; 3], i64, usize)> {
    let rows: Vec<(i64, String)> = tx
        .prepare(
            "SELECT id, member, kind, payload FROM thread_log WHERE thread = ?1 AND id > ?2 ORDER BY id",
        )?
        .query_map(params![thread, cursor], |r| {
            let id = r.get::<_, i64>(0)?;
            Ok((
                id,
                format!(
                    "- #{id} {} {} {}",
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?
                ),
            ))
        })?
        .collect::<Result<_, _>>()?;
    let (mut events, mut used, mut last) = (Vec::new(), 0, cursor);
    for (id, line) in &rows {
        let cost = tokens(line);
        match share {
            Some(share) if used + cost > share => {
                if !events.is_empty() {
                    break;
                }
                let cut = usize::try_from(share.saturating_mul(4)).unwrap_or(usize::MAX);
                events.push(format!(
                    "{} [cut]",
                    line.chars().take(cut).collect::<String>()
                ));
            }
            _ => events.push(line.clone()),
        }
        used += cost;
        last = *id;
    }
    let left = rows.len() - events.len();
    let inbox: Vec<String> = messages(
        tx,
        &format!(
            "SELECT id, thread, sender, recipient, kind, body, tool, state, created FROM messages
             WHERE thread = ?1 AND recipient = ?2 AND state IN ({OPEN}) ORDER BY id"
        ),
        params![thread, member],
    )?
    .into_iter()
    .map(|m| {
        format!(
            "- #{} {} from {}: {}",
            m.id,
            m.kind.as_str(),
            m.from,
            m.body
        )
    })
    .collect();
    let rules: Vec<String> = tx
        .prepare("SELECT id, watched, expect, misses, done FROM watch_rules WHERE thread = ?1 AND member = ?2")?
        .query_map(params![thread, member], |r| {
            Ok(format!(
                "- rule {}: {} should run `{}`; misses {}, done for it {}",
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, u32>(4)?
            ))
        })?
        .collect::<Result<_, _>>()?;
    Ok(([events, inbox, rules], last, left))
}

impl Dossiers {
    /// A thread from a team: `main0` is the caller, each other member joins asleep with its run, config and budget,
    /// and the flow's watch rules start.
    ///
    /// # Errors
    /// A member name is not valid, or the database write fails.
    pub fn team_start(&self, s: &TeamStart) -> Result<(i64, Vec<String>)> {
        let id = self.thread_start(Some(s.session), s.title, s.dossier, None)?;
        let tx = self.begin()?;
        tx.execute(
            "UPDATE threads SET team = ?2, flow = ?3, workdir = ?4 WHERE id = ?1",
            params![
                id,
                s.team,
                serde_json::to_string(s.flow)?,
                s.workdir.to_string_lossy()
            ],
        )?;
        tx.commit()?;
        let mut out = vec![format!(
            "thread {id} started on dossier {} with team {}: this session is main0",
            s.dossier, s.team
        )];
        for m in &s.members {
            let pace = m
                .fields
                .get("pace")
                .and_then(Value::as_str)
                .map_or(Ok(Pace::OnTrigger), Pace::parse)?;
            self.thread_join(id, m.name, m.archetype, None, pace)?;
            self.db().execute(
                "UPDATE members SET config = ?3, run = ?4, agent = ?5, fields = ?6 WHERE thread = ?1 AND name = ?2",
                params![id, m.name, m.config, m.run.as_str(), m.agent, Value::Object(m.fields.clone()).to_string()],
            )?;
            let budget = &m.fields.get("budget").cloned().unwrap_or_default();
            out.push(match m.run {
                Run::Operator => format!(
                    "{}: {} as an operator session, asleep until a trigger (pace {}; per wake at most {} tokens, {} s)",
                    m.name,
                    m.archetype.as_str(),
                    pace.as_str(),
                    budget["wake_tokens"],
                    budget["wake_seconds"]
                ),
                Run::Teammate => format!(
                    "{}: team mode: spawn it as the teammate {} (agent type legionctl:{}); its hooks deliver its \
                     messages, and after a /resume the operator runs it",
                    m.name,
                    m.name,
                    m.archetype.as_str()
                ),
                Run::Subagent => format!(
                    "{}: a Codex subagent: start the agent {} (setup apply wrote it); it reads `legionctl thread inbox`",
                    m.name, m.config
                ),
            });
        }
        let tx = self.begin()?;
        for rule in &s.flow.rules {
            tx.execute(
                "INSERT INTO watch_rules (thread, member, watched, tools, path, expect, every, kind, created, reply)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'reminder', ?7, 1)",
                params![id, rule.member, rule.watched, rule.tools, rule.path, rule.expect, now()],
            )?;
            let rid = tx.last_insert_rowid();
            log(
                &tx,
                id,
                &rule.member,
                "thread.rule",
                &json!({"id": rid, "watched": rule.watched, "tools": rule.tools, "path": rule.path,
                        "expect": rule.expect, "reply": true}),
            )?;
            out.push(format!(
                "rule {rid}: {} watches {} for `{}` before each reply ends",
                rule.member, rule.watched, rule.expect
            ));
        }
        tx.commit()?;
        Ok((id, out))
    }

    /// The caller (a main agent) made a checkpoint: a log row in each of its threads, and the observers wake.
    ///
    /// # Errors
    /// The database write fails.
    pub fn thread_checkpoint(
        &self,
        session: &str,
        commit: Option<&str>,
        message: &str,
    ) -> Result<()> {
        let mine = self.memberships(session)?;
        let tx = self.begin()?;
        for (thread, name) in mine {
            log(
                &tx,
                thread,
                &name,
                "thread.checkpoint",
                &json!({"commit": commit, "message": message}),
            )?;
            trigger(&tx, thread, "checkpoint")?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Why `gate` of `thread` does not pass: one line per open item. The result is the outcome of the thread's rlcd
    /// decisions that have none yet (todo-138).
    ///
    /// # Errors
    /// The database fails.
    pub fn gate_failures(&self, thread: i64, gate: &str) -> Result<Vec<String>> {
        let flow = flow_of(self.db(), thread)?;
        let mut out = Vec::new();
        let gated = flow.gates.contains_key(gate);
        for text in flow.gates.get(gate).into_iter().flatten() {
            match condition(text)? {
                Condition::NoHardBlock(member) => {
                    let open: Vec<(i64, String)> = self
                        .db()
                        .prepare(&format!(
                            "SELECT id, body FROM messages WHERE thread = ?1 AND sender = ?2 AND kind = 'hard-block'
                             AND state IN ({OPEN}) ORDER BY id"
                        ))?
                        .query_map(params![thread, member], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<_, _>>()?;
                    out.extend(
                        open.into_iter()
                            .map(|(id, body)| format!("{text}: hard-block #{id} is open: {body}")),
                    );
                }
                Condition::Result(member, words) => {
                    // a result counts for main0's work up to it: not before main0's last checkpoint or `declare ready`
                    let newest: Option<(String, bool)> = self
                        .db()
                        .query_row(
                            "SELECT json_extract(payload, '$.text'), id > (SELECT coalesce(max(id), 0) FROM thread_log
                                 WHERE thread = ?1 AND member = 'main0' AND (kind = 'thread.checkpoint'
                                 OR (kind = 'thread.declare' AND json_extract(payload, '$.word') = 'ready')))
                             FROM thread_log WHERE thread = ?1 AND member = ?2 AND kind = 'thread.message'
                             AND json_extract(payload, '$.kind') = 'result' ORDER BY id DESC LIMIT 1",
                            params![thread, member],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    match newest {
                        Some((body, true)) if body.starts_with(&words) => {}
                        Some((body, true)) => {
                            out.push(format!("{text}: its newest result is: {body}"));
                        }
                        Some((_, false)) => out.push(format!(
                            "{text}: its newest result is older than main0's last checkpoint or declare ready"
                        )),
                        None => out.push(format!("{text}: no result from {member} yet")),
                    }
                }
            }
        }
        if gated {
            crate::rlcd::gate_outcome(self.db(), thread, gate, &out)?;
        }
        Ok(out)
    }

    /// The checkpoint gate of each thread of `session`: why `workspace checkpoint` must wait, if it must.
    ///
    /// # Errors
    /// The database read fails.
    pub fn checkpoint_gate(&self, session: &str) -> Result<Option<String>> {
        let mut out = Vec::new();
        for (thread, _) in self.memberships(session)? {
            out.extend(
                self.gate_failures(thread, "checkpoint")?
                    .into_iter()
                    .map(|l| format!("thread {thread}: {l}")),
            );
        }
        Ok((!out.is_empty()).then(|| {
            format!(
                "the checkpoint gate holds: {}. Fix it, then ack the block.",
                out.join("; ")
            )
        }))
    }

    /// The main agent's session ended: each of its pools with the stop `main0 ended` ends.
    ///
    /// # Errors
    /// The database write fails.
    pub fn main_ended(&self, session: &str) -> Result<()> {
        let mine = self.memberships(session)?;
        let tx = self.begin()?;
        for (thread, name) in mine.into_iter().filter(|(_, n)| n == "main0") {
            log(
                &tx,
                thread,
                &name,
                "thread.end",
                &json!({"session": session}),
            )?;
            stop_at(&tx, thread, "main0 ended")?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The operator members with a wake that did not run yet.
    ///
    /// # Errors
    /// The database read fails.
    pub fn wakes_due(&self) -> Result<Vec<(i64, String)>> {
        Ok(self
            .db()
            .prepare(
                "SELECT m.thread, m.name FROM members m JOIN threads t ON t.id = m.thread WHERE m.run = 'operator'
                 AND m.state = 'active' AND m.due IS NOT NULL AND t.state = 'open' ORDER BY m.due",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    /// The next wake of an operator member: the events since its cursor, its open messages and its budget. None
    /// when it has no wake due, or it used its tokens of the day (then it sleeps, or ends with the stop `budget`).
    ///
    /// # Errors
    /// The database fails.
    pub fn wake_begin(&self, thread: i64, member: &str) -> Result<Option<Wake>> {
        let tx = self.begin()?;
        let row = tx
            .query_row(
                "SELECT archetype, session, agent, acp, fields, cursor, due FROM members
                 WHERE thread = ?1 AND name = ?2 AND state = 'active' AND run = 'operator'",
                params![thread, member],
                |r| {
                    Ok(Waking {
                        archetype: r.get(0)?,
                        session: r.get(1)?,
                        agent: r.get(2)?,
                        acp: r.get(3)?,
                        fields: r.get(4)?,
                        cursor: r.get(5)?,
                        due: r.get(6)?,
                    })
                },
            )
            .optional()?;
        let Some((row, Some(due))) = row.map(|r| {
            let due = r.due;
            (r, due)
        }) else {
            return Ok(None);
        };
        let fields: Value = serde_json::from_str(&row.fields)?;
        let budget = &fields["budget"];
        if out_of_budget(&tx, (thread, member), budget)? {
            tx.commit()?;
            return Ok(None);
        }
        let (title, dossier, workdir): (String, String, Option<String>) = tx.query_row(
            "SELECT title, dossier, workdir FROM threads WHERE id = ?1",
            [thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let workdir = PathBuf::from(workdir.context("the thread has no working folder")?);
        let (tokens, seconds) = (
            budget["wake_tokens"].as_u64(),
            budget["wake_seconds"].as_u64(),
        );
        // the events take at most half of the wake's tokens, so a backlog never fills the budget before the answer
        let ([events, inbox, rules], to, left) =
            wake_lines(&tx, (thread, member), (row.cursor, tokens.map(|t| t / 2)))?;
        tx.commit()?;
        let limit = |v: Option<u64>| v.map_or_else(|| "no limit".to_string(), |v| v.to_string());
        let prompt = [
            format!("You are {member} ({}) of thread {thread}: {title}.", row.archetype),
            format!("main0 workdir: {}", workdir.display()),
            "That folder is an overlay in your sandbox: run checks there; your writes stay in your own layer and \
             never reach main0's files."
                .into(),
            format!(
                "This wake: at most {} tokens and {} seconds. Send each result with `legionctl thread send`.",
                limit(tokens),
                limit(seconds)
            ),
            format!("New events since your last wake ({}):", events.len()),
            events.join("\n"),
            if left > 0 {
                format!("{left} newer events do not fit in this wake: your next wake gets them.")
            } else {
                String::new()
            },
            format!("Open messages to you ({}):", inbox.len()),
            inbox.join("\n"),
            format!("Your watch rules ({}):", rules.len()),
            rules.join("\n"),
        ]
        .join("\n");
        Ok(Some(Wake {
            thread,
            member: member.into(),
            archetype: Archetype::parse(&row.archetype)?,
            session: row.session,
            agent: row.agent.context("the member has no agent")?,
            dossier: format!("{dossier}--{member}"),
            workdir,
            acp: row.acp,
            fields,
            tokens,
            seconds,
            due,
            to,
            more: left > 0,
            prompt,
        }))
    }

    /// The session that the operator made for a member at its first wake.
    ///
    /// # Errors
    /// The database write fails.
    pub fn member_session(&self, w: &Wake, session: &str) -> Result<()> {
        self.db().execute(
            "UPDATE members SET session = ?3 WHERE thread = ?1 AND name = ?2",
            params![w.thread, w.member, session],
        )?;
        Ok(())
    }

    /// The end of a wake: its cost, and the cursor after the events it saw. A wake stopped at its budget leaves its
    /// events after the cursor for the next wake; a wake whose events did not all fit wakes again now. A failed wake
    /// starts again within the flow's restart limit; past it, the member fails, its blocks expire and the main agent
    /// gets a note. Returns true when the member must wake again now.
    ///
    /// # Errors
    /// The database fails.
    pub fn wake_end(&self, w: &Wake, end: &WakeEnd) -> Result<bool> {
        let tx = self.begin()?;
        tx.execute(
            "UPDATE members SET acp = coalesce(?3, acp) WHERE thread = ?1 AND name = ?2",
            params![w.thread, w.member, end.acp],
        )?;
        cost(
            &tx,
            (w.thread, &w.member),
            "wake",
            (end.tokens, end.ms, end.over, end.error.as_deref()),
        )?;
        let mut again = false;
        if let Some(error) = &end.error {
            let flow = flow_of(&tx, w.thread)?;
            let since = now() - i64::try_from(seconds(&flow.restart.within)?)?;
            let failed: u32 = tx.query_row(
                "SELECT count(*) FROM thread_log WHERE thread = ?1 AND member = ?2 AND kind = 'thread.fail' AND ts >= ?3",
                params![w.thread, w.member, since],
                |r| r.get(0),
            )?;
            log(
                &tx,
                w.thread,
                &w.member,
                "thread.fail",
                &json!({"error": error, "failures": failed + 1}),
            )?;
            if failed + 1 > flow.restart.max {
                let expired = expire(&tx, w.thread, &w.member)?;
                tx.execute(
                    "UPDATE members SET state = 'failed', due = NULL WHERE thread = ?1 AND name = ?2",
                    params![w.thread, w.member],
                )?;
                post(
                    &tx,
                    w.thread,
                    (&w.member, "main0"),
                    Kind::Note,
                    &format!(
                        "{} failed {} times within {}: {error}. It stopped; {expired} of its blocks expired.",
                        w.member,
                        failed + 1,
                        flow.restart.within
                    ),
                    None,
                )?;
            } else {
                again = true;
            }
        } else if end.over.is_none() {
            again = seen(&tx, w)?;
        } else {
            answered(&tx, w)?;
        }
        tx.commit()?;
        Ok(again)
    }

    /// The end of the rlcd step of a wake (todo-138). It never touches a message: rlcd can raise a gate only through
    /// the step it escalates to, and never clears one. Returns true when the member must wake again now (its events
    /// did not all fit).
    ///
    /// # Errors
    /// The database fails.
    pub fn rlcd_end(&self, w: &Wake, end: &RlcdEnd) -> Result<bool> {
        let tx = self.begin()?;
        cost(
            &tx,
            (w.thread, &w.member),
            "rlcd",
            (end.tokens, end.ms, None, end.error.as_deref()),
        )?;
        log(&tx, w.thread, &w.member, "thread.rlcd", &end.event)?;
        let again = match (end.ends, &end.error) {
            (false, _) => false,
            (true, Some(_)) => {
                answered(&tx, w)?;
                false
            }
            (true, None) => seen(&tx, w)?,
        };
        tx.commit()?;
        Ok(again)
    }

    /// A wake that an operator restart cut off: its ACP session stays the member's, so the next wake loads it and
    /// gets the same events again.
    ///
    /// # Errors
    /// The database write fails.
    pub fn wake_cut_off(&self, thread: i64, member: &str, acp: Option<&str>) -> Result<()> {
        let tx = self.begin()?;
        tx.execute(
            "UPDATE members SET acp = coalesce(?3, acp) WHERE thread = ?1 AND name = ?2",
            params![thread, member, acp],
        )?;
        log(&tx, thread, member, "thread.cut-off", &json!({"acp": acp}))?;
        tx.commit()?;
        Ok(())
    }

    /// A member's backfill (design 23, E6): one line, marked `(by <member>)`, at the end of a file of the main
    /// agent's dossier. Only an active thread-observer of a thread on that dossier, and only while the main agent is
    /// idle; the operator's write lock (one IMMEDIATE transaction) holds the check and the write together. A tell of
    /// the main agent's: a notice lists it, and `thread compensate` removes it.
    ///
    /// # Errors
    /// The caller is not that member, the file is in another dossier, the main agent is not idle (send a nudge), or
    /// the write fails.
    pub fn backfill(
        &self,
        events: &Db,
        session: &str,
        member: &str,
        rel: &str,
        text: &str,
    ) -> Result<Vec<String>> {
        ensure!(
            !text.trim().is_empty() && !text.trim().contains('\n'),
            "a backfill is one line"
        );
        let (thread, dossier, main): (i64, String, Option<String>) = self
            .db()
            .query_row(
                "SELECT t.id, t.dossier, (SELECT session FROM members WHERE thread = t.id AND name = 'main0')
                 FROM members m JOIN threads t ON t.id = m.thread WHERE m.session = ?1 AND m.name = ?2
                 AND m.state = 'active' AND m.archetype = ?3 AND t.state = 'open'",
                params![session, member, Archetype::ThreadObserver.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .with_context(|| {
                format!("this session is not the active thread-observer {member} of an open thread")
            })?;
        ensure!(
            rel.split('/').next() == Some(dossier.as_str()),
            "{member} backfills only the dossier of its thread ({dossier}); {rel} is not in it"
        );
        let main = main.context("main0 has no session")?;
        // the check and the write under one write lock: no hook call of main0 lands between them
        let tx = self.begin()?;
        let state = live_state(&self.presence(events)?, &main);
        if state != "idle" {
            bail!(
                "main0 is {state}: a backfill waits for its idle window. Send a nudge now (`legionctl thread send \
                 main0 reminder ...`)"
            );
        }
        let line = format!("{} (by {member})", text.trim());
        self.append_line(session, rel, &line)?;
        let id = effect(&tx, thread, member, ("backfill", rel, &line))?;
        log(
            &tx,
            thread,
            member,
            "thread.backfill",
            &json!({"effect": id, "path": rel, "line": line}),
        )?;
        post(
            &tx,
            thread,
            (member, "main0"),
            Kind::Notice,
            &format!(
                "{member} wrote for you: {rel}: {line} (undo: legionctl thread compensate {id})"
            ),
            None,
        )?;
        tx.commit()?;
        Ok(vec![format!("backfilled {rel}: {line} (effect {id})")])
    }

    /// Undo an effect: remove a backfill's line, retract a message. Only main0 of the effect's thread or the member
    /// that made it may; the check and the undo are under one write lock, so an effect is undone once.
    ///
    /// # Errors
    /// No such effect, the caller may not undo it, it is undone already, or its line is gone.
    pub fn compensate(&self, session: &str, id: i64) -> Result<Vec<String>> {
        let tx = self.begin()?;
        let (thread, member, kind, target, undo, state): (
            i64,
            String,
            String,
            String,
            String,
            String,
        ) = tx
            .query_row(
                "SELECT thread, member, kind, target, undo, state FROM effects WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("no effect {id}"))?;
        let allowed: bool = tx.query_row(
            "SELECT count(*) > 0 FROM members WHERE thread = ?1 AND session = ?2 AND name IN ('main0', ?3)",
            params![thread, session, member],
            |r| r.get(0),
        )?;
        ensure!(
            allowed,
            "effect {id} is of thread {thread}: only its main0 or {member} undoes it"
        );
        ensure!(state == "done", "effect {id} is {state} already");
        let what = match kind.as_str() {
            "backfill" => {
                self.remove_line(session, &target, &undo)?;
                format!("removed from {target}: {undo}")
            }
            "message" => {
                tx.execute(
                    "UPDATE messages SET state = 'retracted' WHERE id = ?1",
                    [undo.parse::<i64>()?],
                )?;
                format!("retracted message #{target}")
            }
            other => bail!("no compensation for an effect of kind {other}"),
        };
        tx.execute("UPDATE effects SET state = 'undone' WHERE id = ?1", [id])?;
        log(
            &tx,
            thread,
            &member,
            "thread.compensate",
            &json!({"effect": id, "kind": kind, "target": target, "by": session}),
        )?;
        tx.commit()?;
        Ok(vec![format!("effect {id} of {member}: {what}")])
    }

    /// A branch of `thread` at log row `at`: a new thread with the log up to it, the same members (each that did not
    /// leave, active again; an operator member gets a new session at its first wake) and rules, the caller as main0,
    /// and the git branch `legionctl/thread-<new>` at the newest checkpoint up to `at`.
    ///
    /// # Errors
    /// No such thread or row, or the database or git fails.
    pub fn branch(&self, session: &str, thread: i64, at: i64) -> Result<(i64, Vec<String>)> {
        let (title, dossier, team, flow, workdir): (
            String,
            String,
            Option<String>,
            String,
            Option<String>,
        ) = self
            .db()
            .query_row(
                "SELECT title, dossier, team, flow, workdir FROM threads WHERE id = ?1",
                [thread],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .with_context(|| format!("no thread {thread}"))?;
        let found: Option<i64> = self
            .db()
            .query_row(
                "SELECT id FROM thread_log WHERE thread = ?1 AND id = ?2",
                params![thread, at],
                |r| r.get(0),
            )
            .optional()?;
        ensure!(found.is_some(), "thread {thread} has no log row {at}");
        let commit: Option<String> = self
            .db()
            .query_row(
                "SELECT json_extract(payload, '$.commit') FROM thread_log WHERE thread = ?1 AND id <= ?2
                 AND kind = 'thread.checkpoint' ORDER BY id DESC LIMIT 1",
                params![thread, at],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let tx = self.begin()?;
        tx.execute(
            "INSERT INTO threads (title, dossier, created, team, flow, workdir, parent) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![format!("{title} (branch of {thread} at {at})"), dossier, now(), team, flow, workdir, thread],
        )?;
        let new = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO thread_log (thread, ts, member, kind, payload) SELECT ?1, ts, member, kind,
             json_set(payload, '$.from', id) FROM thread_log WHERE thread = ?2 AND id <= ?3 ORDER BY id",
            params![new, thread, at],
        )?;
        tx.execute(
            "INSERT INTO members (thread, name, archetype, session, pace, state, joined, config, run, agent, fields)
             SELECT ?1, name, archetype, CASE WHEN name = 'main0' THEN ?3 END, pace, 'active', ?4, config, run, agent,
             fields FROM members WHERE thread = ?2 AND state != 'left'",
            params![new, thread, session, now()],
        )?;
        tx.execute(
            "INSERT INTO watch_rules (thread, member, watched, tools, path, expect, every, kind, created, run, misses,
             done, reply) SELECT ?1, member, watched, tools, path, expect, every, kind, created, run, misses, done,
             reply FROM watch_rules WHERE thread = ?2",
            params![new, thread],
        )?;
        tx.commit()?;
        let branch = format!("legionctl/thread-{new}");
        let mut out = vec![format!(
            "thread {new} is a branch of thread {thread} at log row {at}: this session is main0"
        )];
        // git runs outside the write lock (todo-189)
        if let (Some(commit), Some(dir)) = (&commit, &workdir) {
            crate::git::text(Path::new(dir), &["branch", &branch, commit])
                .with_context(|| format!("thread {new} is made, its branch {branch} is not"))?;
            out.push(format!(
                "branch {branch} starts at the checkpoint {commit}; open it with `legionctl workspace open --base {branch} --topic thread-{new}`"
            ));
        }
        let tx = self.begin()?;
        log(
            &tx,
            new,
            "main0",
            "thread.branch",
            &json!({"parent": thread, "at": at, "commit": commit, "branch": commit.as_ref().map(|_| &branch)}),
        )?;
        tx.commit()?;
        Ok((new, out))
    }

    /// `TeammateIdle` (team mode): the messages to the teammate `name` of the lead's thread that it did not see, as one
    /// text; the hook gives it with exit 2, so the teammate goes on with them. The member runs as a teammate from now.
    ///
    /// # Errors
    /// The database fails.
    pub fn teammate_idle(&self, lead: &str, name: &str) -> Result<Option<String>> {
        let Some(thread) = self.lead_thread(lead, name)? else {
            return Ok(None);
        };
        let tx = self.begin()?;
        let changed = tx.execute(
            "UPDATE members SET run = 'teammate', due = NULL WHERE thread = ?1 AND name = ?2 AND run != 'teammate'",
            params![thread, name],
        )?;
        if changed > 0 {
            log(&tx, thread, name, "thread.teammate", &json!({"lead": lead}))?;
        }
        let list = messages(
            &tx,
            "SELECT id, thread, sender, recipient, kind, body, tool, state, created FROM messages
             WHERE thread = ?1 AND recipient = ?2 AND state = 'pending' ORDER BY id",
            params![thread, name],
        )?;
        crate::thread::delivered(&tx, &list)?;
        tx.commit()?;
        Ok((!list.is_empty()).then(|| {
            let mut lines = vec![format!(
                "legionctl thread {thread}: {} new messages for {name}. Act on them before you go idle.",
                list.len()
            )];
            lines.extend(
                list.iter()
                    .map(|m| format!("- #{} {} from {}: {}", m.id, m.kind.as_str(), m.from, m.body)),
            );
            lines.join("\n")
        }))
    }

    /// `TaskCompleted` (team mode): why the lead's thread holds the task (its `task:completed` gate), if it does.
    ///
    /// # Errors
    /// The database fails.
    pub fn task_gate(&self, lead: &str) -> Result<Option<String>> {
        let mut out = Vec::new();
        for (thread, name) in self.memberships(lead)? {
            if name == "main0" {
                out.extend(self.gate_failures(thread, "task:completed")?);
            }
        }
        Ok((!out.is_empty())
            .then(|| format!("legionctl thread: the task waits: {}", out.join("; "))))
    }

    /// The lead resumed (`/resume`): Claude Code does not restore its in-process teammates, so each teammate member
    /// is lost and runs again as an operator session, woken at once.
    ///
    /// # Errors
    /// The database fails.
    pub fn lead_resumed(&self, lead: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let tx = self.begin()?;
        for (thread, name) in self.memberships(lead)? {
            if name != "main0" {
                continue;
            }
            let lost: Vec<String> = tx
                .prepare("SELECT name FROM members WHERE thread = ?1 AND run = 'teammate' AND state = 'active'")?
                .query_map([thread], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for member in lost {
                tx.execute(
                    "UPDATE members SET run = 'operator' WHERE thread = ?1 AND name = ?2",
                    params![thread, member],
                )?;
                log(&tx, thread, &member, "thread.lost", &json!({"lead": lead}))?;
                wake(
                    &tx,
                    thread,
                    &member,
                    "lost at /resume: runs as an operator session",
                )?;
                out.push(format!(
                    "legionctl thread {thread}: the teammate {member} was lost at /resume; it now runs as an operator session"
                ));
            }
        }
        tx.commit()?;
        Ok(out)
    }

    /// The open thread where `lead` is main0 and `name` an active member.
    fn lead_thread(&self, lead: &str, name: &str) -> Result<Option<i64>> {
        for (thread, me) in self.memberships(lead)? {
            if me == "main0"
                && self
                    .member(thread, name)?
                    .is_some_and(|(_, _, s)| s == "active")
            {
                return Ok(Some(thread));
            }
        }
        Ok(None)
    }

    pub(crate) fn costs(&self, thread: i64) -> Result<Vec<Cost>> {
        Ok(self
            .db()
            .prepare(
                "SELECT member, kind, count(*), sum(tokens), sum(ms), count(over), count(error) FROM costs
                 WHERE thread = ?1 GROUP BY member, kind ORDER BY member, kind",
            )?
            .query_map([thread], |r| {
                Ok(Cost {
                    member: r.get(0)?,
                    kind: r.get(1)?,
                    runs: r.get(2)?,
                    tokens: r.get(3)?,
                    ms: r.get(4)?,
                    over: r.get(5)?,
                    failed: r.get(6)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    pub(crate) fn effects(&self, thread: i64) -> Result<Vec<Effect>> {
        Ok(self
            .db()
            .prepare(
                "SELECT id, member, kind, target, state FROM effects WHERE thread = ?1 ORDER BY id",
            )?
            .query_map([thread], |r| {
                Ok(Effect {
                    id: r.get(0)?,
                    member: r.get(1)?,
                    kind: r.get(2)?,
                    target: r.get(3)?,
                    state: r.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }
}
