//! The `thread` operations (design 23, Part B, P5): threads, members, the outbox, watch rules, pace and presence.
//! The work is in `legionctl_core::thread`; the hooks deliver the messages (`engine::hooks_run`).

use anyhow::{Context, Result};
use legionctl_api::{
    AgentToolIn, Guard, HookReply, IntoWire, Lines, ObserveIn, Observed, Peer, PeersIn, RuleAddIn,
    RuleRmIn, SessionIn, ThreadAckIn, ThreadDeclareIn, ThreadGateIn, ThreadIdIn, ThreadInfo,
    ThreadJoinIn, ThreadMemberIn, ThreadMessage, ThreadPaceIn, ThreadSendIn, ThreadStartIn,
    ThreadView, WatchRule,
};
use legionctl_core::db::Db;
use legionctl_core::thread::{NewRule, Scope, ToolUse, peer_key};

use crate::engine::{dossiers, session};

fn lines(lines: Vec<String>) -> Lines {
    Lines { lines }
}

pub fn start(input: &ThreadStartIn) -> Result<Lines> {
    let store = dossiers()?;
    let me = session(&input.session)?;
    let dossier = match &input.dossier {
        Some(d) => d.clone(),
        None => store
            .active(me)?
            .context("this session has no dossier; bind one or give --dossier")?,
    };
    let id = store.thread_start(me, &input.title, &dossier)?;
    Ok(lines(vec![format!(
        "thread {id} started on dossier {dossier}: this session is main0"
    )]))
}

pub fn list() -> Result<Vec<ThreadInfo>> {
    Ok(dossiers()?.threads()?.into_wire())
}

pub fn show(input: &ThreadIdIn) -> Result<ThreadView> {
    Ok(dossiers()?.thread_show(input.thread)?.into_wire())
}

pub fn join(input: &ThreadJoinIn) -> Result<Lines> {
    dossiers()?.thread_join(
        input.thread,
        &input.name,
        input.archetype.into(),
        input.member_session.as_deref(),
        input.pace.into(),
    )?;
    Ok(lines(vec![format!(
        "{} joined thread {} as {} (pace {})",
        input.name, input.thread, input.archetype, input.pace
    )]))
}

pub fn leave(input: &ThreadMemberIn) -> Result<Lines> {
    Ok(lines(dossiers()?.thread_leave(input.thread, &input.name)?))
}

pub fn send(input: &ThreadSendIn) -> Result<Lines> {
    let id = dossiers()?.thread_send(
        session(&input.session)?,
        input.thread,
        (&input.to, input.kind.into(), &input.text),
        input.tool.as_deref(),
        input.rule,
    )?;
    Ok(lines(vec![format!(
        "message #{id} to {} ({})",
        input.to, input.kind
    )]))
}

pub fn inbox(input: &SessionIn) -> Result<Vec<ThreadMessage>> {
    Ok(dossiers()?.inbox(session(&input.session)?)?.into_wire())
}

pub fn ack(input: &ThreadAckIn) -> Result<Lines> {
    Ok(lines(dossiers()?.thread_ack(
        session(&input.session)?,
        input.id,
        input.reply.as_deref(),
    )?))
}

pub fn declare(input: &ThreadDeclareIn) -> Result<Lines> {
    Ok(lines(dossiers()?.thread_declare(
        session(&input.session)?,
        input.word.into(),
        input.note.as_deref(),
    )?))
}

pub fn pace(input: &ThreadPaceIn) -> Result<Lines> {
    dossiers()?.thread_pace(input.thread, &input.name, input.pace.into())?;
    Ok(lines(vec![format!(
        "{} now wakes {}",
        input.name, input.pace
    )]))
}

pub fn rules(input: &ThreadIdIn) -> Result<Vec<WatchRule>> {
    Ok(dossiers()?.rules(input.thread)?.into_wire())
}

pub fn rule_add(input: &RuleAddIn) -> Result<Lines> {
    let id = dossiers()?.rule_add(
        input.thread,
        &NewRule {
            member: &input.member,
            watched: &input.watched,
            tools: &input.tools,
            path: &input.path,
            expect: &input.expect,
            every: input.every,
            kind: input.kind.into(),
        },
    )?;
    Ok(lines(vec![format!("rule {id} added")]))
}

pub fn rule_rm(input: &RuleRmIn) -> Result<Lines> {
    dossiers()?.rule_rm(input.id)?;
    Ok(lines(vec![format!("rule {} removed", input.id)]))
}

pub fn peers(input: &PeersIn) -> Result<Vec<Peer>> {
    let scope = Scope {
        dossier: input.dossier.clone(),
        topic: input.topic.clone(),
        thread: input.thread,
    };
    Ok(dossiers()?
        .peers(
            &Db::open_default()?,
            &input.session.0,
            input.repo.get()?,
            &scope,
        )?
        .into_wire())
}

pub fn observe(input: &ObserveIn) -> Result<Observed> {
    Ok(dossiers()?
        .observe(
            &Db::open_default()?,
            &input.session.0,
            input.repo.get()?,
            &input.peer,
        )?
        .into_wire())
}

/// Record a tool call of a session's agent or subagent (a peer of its own, `peer_key`). A subagent has no prompt
/// hook, so the messages to it come back here, for its post-tool hook.
pub fn tool(input: &AgentToolIn) -> Result<HookReply> {
    if input.session.0.is_empty() {
        return Ok(HookReply::default());
    }
    let store = dossiers()?;
    let peer = peer_key(&input.session.0, input.agent.as_deref());
    store.tool_used(
        &peer,
        &ToolUse {
            tool: &input.tool,
            path: input.path.as_deref(),
            command: input.command.as_deref(),
            to: input.to.as_deref(),
            message: input.message.as_deref(),
            ok: input.ok,
        },
    )?;
    Ok(HookReply {
        text: match input.agent {
            Some(_) => store.thread_prompt(&peer)?.unwrap_or_default(),
            None => String::new(),
        },
        block: None,
    })
}

pub fn gate(input: &ThreadGateIn) -> Result<Guard> {
    Ok(Guard {
        refusal: match input.session.0.as_str() {
            "" => None,
            me => dossiers()?.thread_gate(me, &input.tool)?,
        },
    })
}
