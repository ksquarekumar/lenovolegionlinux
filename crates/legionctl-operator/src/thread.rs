//! The `thread` operations (design 23, Part B, P5 and P6): threads, members, the outbox, watch rules, pace,
//! presence, and pools: a team's members, the wakes of operator members (one ACP prompt each, within a budget),
//! branch, compensation and the team mode hooks. The work is in `legionctl_core::thread` and `legionctl_core::pool`;
//! the hooks deliver the messages (`engine::hooks_run`).

use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use legionctl_api::{
    AgentToolIn, Guard, HookReply, IntoWire, Lines, ObserveIn, Observed, Peer, PeersIn, RuleAddIn,
    RuleRmIn, SessionIn, TeammateIn, ThreadAckIn, ThreadBranchIn, ThreadCompensateIn,
    ThreadDeclareIn, ThreadGateIn, ThreadIdIn, ThreadInfo, ThreadJoinIn, ThreadMemberIn,
    ThreadMessage, ThreadPaceIn, ThreadSendIn, ThreadStartIn, ThreadView, WatchRule,
};
use legionctl_core::archetypes::{Harness, Sources};
use legionctl_core::config::{Dirs, resolve};
use legionctl_core::db::Db;
use legionctl_core::pool::{NewMember, Run, TeamStart, WakeEnd, tokens};
use legionctl_core::session::{Inside, Start};
use legionctl_core::thread::{Archetype, NewRule, Scope, ToolUse, peer_key};

use crate::engine::{dossiers, end_job, session};

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
    let Some(team) = &input.team else {
        let id = store.thread_start(me, &input.title, &dossier)?;
        return Ok(lines(vec![format!(
            "thread {id} started on dossier {dossier}: this session is main0"
        )]));
    };
    let dir = input.repo.get()?;
    let sources = Sources::resolve(&Dirs::from_env(), Some(dir))?;
    let path = sources.team_path(team).with_context(|| {
        format!("no team {team} in .legionctl/teams, the user's teams or the plugin's teams")
    })?;
    // a session that legionctl did not start is Claude Code's: only its hooks reach the operator
    let main_agent = store
        .session(me)
        .map_or_else(|_| "claude".to_string(), |r| r.agent);
    let run = Run::for_main(&main_agent);
    let harness = if main_agent == "codex" {
        Harness::Codex
    } else {
        Harness::Claude
    };
    let check = sources.check(Some((team, &path)), harness, run == Run::Operator)?;
    anyhow::ensure!(
        check.refused.is_empty(),
        "team {team} cannot start with a {main_agent} main agent:\n{}",
        check.refused.join("\n")
    );
    let members = check
        .members
        .iter()
        .filter(|m| m.archetype != Archetype::MainAgent.as_str())
        .map(|m| {
            Ok(NewMember {
                name: &m.name,
                archetype: Archetype::parse(&m.archetype)?,
                config: &m.config,
                agent: m
                    .fields
                    .get("harness")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&main_agent),
                run,
                fields: &m.fields,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let (_, out) = store.team_start(&TeamStart {
        session: me,
        title: &input.title,
        dossier: &dossier,
        workdir: dir,
        team,
        flow: &check.flow,
        members,
    })?;
    Ok(lines(out))
}

pub fn branch(input: &ThreadBranchIn) -> Result<Lines> {
    let (_, out) = dossiers()?.branch(session(&input.session)?, input.thread, input.at)?;
    Ok(lines(out))
}

pub fn compensate(input: &ThreadCompensateIn) -> Result<Lines> {
    Ok(lines(
        dossiers()?.compensate(session(&input.session)?, input.effect)?,
    ))
}

pub fn teammate(input: &TeammateIn) -> Result<Guard> {
    let store = dossiers()?;
    let lead = session(&input.session)?;
    Ok(Guard {
        refusal: match (input.event.as_str(), &input.teammate) {
            ("idle", Some(name)) => store.teammate_idle(lead, name)?,
            ("idle", None) => None,
            ("task", _) => store.task_gate(lead)?,
            (other, _) => anyhow::bail!("no team mode hook {other}: use idle or task"),
        },
    })
}

/// The runtime that runs the wakes, from the operator's start.
static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();
/// The members whose wake runs now: one wake per member at a time.
static RUNNING: Mutex<BTreeSet<(i64, String)>> = Mutex::new(BTreeSet::new());

/// At the operator's start: the runtime of the wakes, then each wake that is due (a trigger before a restart, or a
/// wake that the restart cut off: it loads its ACP session again).
pub fn init(runtime: tokio::runtime::Handle) {
    let _ = RUNTIME.set(runtime);
    let cut = Db::open_default().and_then(|db| {
        let store = dossiers()?;
        for job in db
            .jobs_in("running")?
            .into_iter()
            .filter(|j| j.kind == WAKE)
        {
            store.wake_cut_off(
                job.input["thread"].as_i64().unwrap_or_default(),
                job.input["member"].as_str().unwrap_or_default(),
                job.input["acp"].as_str(),
            )?;
            db.job_end(
                job.id,
                &Err("cut off by an operator restart; the member wakes again".into()),
            )?;
        }
        Ok(())
    });
    if let Err(e) = cut {
        eprintln!("operator: cut-off thread wakes not recorded: {e:#}");
    }
    wake_due();
}

/// The job kind of a member's wake.
pub const WAKE: &str = "thread.wake";

/// Run each due wake of an operator member in the background.
pub fn wake_due() {
    let due = match dossiers().and_then(|s| s.wakes_due()) {
        Ok(due) => due,
        Err(e) => return eprintln!("operator: thread wakes not read: {e:#}"),
    };
    for (thread, member) in due {
        let key = (thread, member.clone());
        if !RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone())
        {
            continue;
        }
        std::thread::spawn(move || {
            loop {
                match wake(thread, &member) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => {
                        eprintln!("operator: thread {thread}: wake of {member}: {e:#}");
                        break;
                    }
                }
            }
            RUNNING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&key);
            // a trigger during the wake
            wake_due();
        });
    }
}

/// One wake of an operator member: its session (made at its first wake), one ACP prompt with its instructions and
/// the events since its cursor, within its budget. Returns true when it must wake again now (a failed wake within
/// the restart limit).
fn wake(thread: i64, member: &str) -> Result<bool> {
    let store = dossiers()?;
    let Some(w) = store.wake_begin(thread, member)? else {
        return Ok(false);
    };
    let name = format!("{thread}/{member}");
    // from here each error ends the wake through wake_end: the restart limit applies, and the request does not stay due
    let started = resolve(&Dirs::from_env(), Some(&w.workdir)).and_then(|resolved| {
        let token = legionctl_api::random_token()?;
        let launch = match &w.session {
            None => store
                .start_session(
                    &Start {
                        agent: &w.agent,
                        dossier: &w.dossier,
                        steal: false,
                        token: &token,
                        dir: &w.workdir,
                        headless: true,
                        account: w.fields["account"].as_str(),
                        inside: Inside::Host,
                        gpu: false,
                        member: Some((&name, &w.workdir)),
                    },
                    &resolved.config,
                )
                .and_then(|launch| {
                    store.member_session(&w, &launch.id)?;
                    Ok(launch)
                }),
            Some(id) => store.resume_session(id, &resolved.config, None, &token, true),
        }?;
        Ok((launch, token))
    });
    let ran = started.and_then(|(launch, token)| {
        let id = launch.id.clone();
        let db = Db::open_default()?;
        db.issue_token(&launch.id, &token)?;
        let extra: Vec<String> =
            serde_json::from_value(w.fields["instructions"].clone()).unwrap_or_default();
        let instructions = Sources::resolve(&Dirs::from_env(), Some(&w.workdir))?
            .instructions(w.archetype.as_str(), &extra)?;
        let prompt = format!("{instructions}\n\n{}", w.prompt);
        let input = serde_json::json!({"thread": thread, "member": member, "session": launch.id,
            "tag": format!("thread-{name}"), "acp": w.acp, "load": false});
        let job = db.job_begin(WAKE, &input)?;
        let budget = crate::acp::Budget {
            tokens: w.tokens.map(|t| t.saturating_sub(tokens(&prompt))),
            seconds: w.seconds,
        };
        // the wake is not offloaded: its sender lives as long as the run
        let (_keep, stop) = tokio::sync::oneshot::channel();
        let runtime = RUNTIME
            .get()
            .context("the operator has no runtime for wakes")?;
        let out = runtime.block_on(crate::acp::run_within(
            launch,
            &prompt,
            (job, &input),
            (stop, budget),
            &store,
        ));
        end_job(
            &db,
            job,
            &out.as_ref()
                .map(|(o, _)| o)
                .map_err(|e| anyhow::anyhow!("{e:#}")),
        )?;
        legionctl_core::session::drop_overlay(&id)?;
        Ok(out.map(|(o, over)| (o, over, tokens(&prompt))))
    });
    let end = match ran.and_then(|r| r) {
        Ok((out, over, asked)) => WakeEnd {
            tokens: asked + tokens(&out.text),
            ms: out.ms,
            over,
            error: None,
            acp: out.acp,
        },
        Err(e) => WakeEnd {
            tokens: 0,
            ms: 0,
            over: None,
            error: Some(format!("{e:#}")),
            acp: None,
        },
    };
    store.wake_end(&w, &end)
}

pub fn list() -> Result<Vec<ThreadInfo>> {
    Ok(dossiers()?.threads()?.into_wire())
}

pub fn show(input: &ThreadIdIn) -> Result<ThreadView> {
    Ok(dossiers()?.thread_show(input.thread)?.into_wire())
}

/// The chat bar's agent of a thread: a name of `session.acp`, else refused with the names.
pub fn agent(input: &legionctl_api::ThreadAgentIn) -> Result<Lines> {
    let config = crate::engine::config(Some(&input.repo))?;
    let names: Vec<&String> = config.config.session.acp.keys().collect();
    anyhow::ensure!(
        names.contains(&&input.agent),
        "no agent {} in session.acp; it has {}",
        input.agent,
        names
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    dossiers()?.thread_agent(input.thread, &input.agent)?;
    Ok(lines(vec![format!(
        "thread {}: the chat bar sends its prompts to {}",
        input.thread, input.agent
    )]))
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
        (input.rule, input.teammate.as_deref()),
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
    let out = dossiers()?.thread_declare(
        session(&input.session)?,
        input.word.into(),
        input.note.as_deref(),
    )?;
    wake_due();
    Ok(lines(out))
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
    wake_due();
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
