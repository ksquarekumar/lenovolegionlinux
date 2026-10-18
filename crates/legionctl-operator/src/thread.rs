//! The `thread` operations (design 23, Part B, P5 and P6): threads, members, the outbox, watch rules, pace,
//! presence, and pools: a team's members, the wakes of operator members (one ACP prompt each, within a budget),
//! branch, compensation and the team mode hooks. The work is in `legionctl_core::thread` and `legionctl_core::pool`;
//! the hooks deliver the messages (`engine::hooks_run`).

use std::collections::BTreeSet;
use std::path::Path;
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

pub fn start(dir: &Path, input: &ThreadStartIn) -> Result<Lines> {
    let store = dossiers()?;
    // no session: the user's own thread (the app's chat), on the dossier that the user names
    if input.session.0.is_empty() && input.team.is_none() {
        let dossier = input.dossier.as_deref().context(
            "your thread needs a dossier: give --dossier <name> (dossier list; a new one: dossier create <name>)",
        )?;
        store.known(dossier)?;
        let id = store.thread_start(None, &input.title, dossier)?;
        return Ok(lines(vec![format!(
            "thread {id} started on dossier {dossier}: your thread"
        )]));
    }
    let me = session(&input.session)?;
    let dossier = match &input.dossier {
        Some(d) => d.clone(),
        None => store.active(me)?.context(
            "this session has no dossier: bind one with dossier use <name>, or give --dossier",
        )?,
    };
    let Some(team) = &input.team else {
        let id = store.thread_start(Some(me), &input.title, &dossier)?;
        return Ok(lines(vec![format!(
            "thread {id} started on dossier {dossier}: this session is main0"
        )]));
    };
    let sources = Sources::resolve(&Dirs::from_env(), Some(dir))?;
    let path = sources.team_path(team)?.with_context(|| {
        format!("no team {team} in .legionctl/teams, your teams or the plugin's teams")
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
                // the harness of the member's config (an operator member's agent of session.acp), else the main's
                agent: match sources.configs.get(&m.config).and_then(|c| c.harness) {
                    Some(Harness::Claude) => "claude",
                    Some(Harness::Codex) => "codex",
                    None => &main_agent,
                },
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

/// One wake of an operator member, through its route (todo-138; none: `acp`): each configured step in order. An
/// rlcd step that escalates gives the next step its brief; an rlcd step that ignores, notes or fails ends the wake
/// (a failed step is reported, and the next step does not run in its place). Returns true when it must wake again
/// now.
fn wake(thread: i64, member: &str) -> Result<bool> {
    let store = dossiers()?;
    let Some(w) = store.wake_begin(thread, member)? else {
        return Ok(false);
    };
    let runtime = RUNTIME
        .get()
        .context("the operator has no runtime for wakes")?;
    let route: Vec<String> = match w.fields.get("route") {
        Some(route) => serde_json::from_value(route.clone()).context("the member's route")?,
        None => vec!["acp".into()],
    };
    let mut escalated = None;
    for (i, step) in route.iter().enumerate() {
        match step.as_str() {
            "rlcd" if w.fields.get("rlcd").is_none() => {}
            "rlcd" => match crate::rlcd::observe(runtime, &store, &w, i + 1 < route.len())? {
                crate::rlcd::Step::Ended { again } => return Ok(again),
                crate::rlcd::Step::Escalate { decision, brief } => {
                    escalated = Some((decision, brief));
                }
            },
            "acp" => return acp_wake(&store, &w, escalated.as_ref()),
            other => anyhow::bail!("route step {other} is not rlcd or acp"),
        }
    }
    anyhow::bail!("the route {route:?} has no configured step")
}

/// The account that the member runs on at this wake (todo-136, todo-188): its own (its config's, or the user's pick),
/// or the next account of clauth's fallback chain when its own is at the chain's limit, with a thread note and an
/// event. When clauth cannot tell, the wake fails with the reason.
fn account_of(
    store: &legionctl_core::dossier::Dossiers,
    w: &legionctl_core::pool::Wake,
) -> Result<Option<String>> {
    let own = w.fields["account"].as_str();
    Ok(match crate::accounts::decide(&w.agent, own)? {
        crate::accounts::Decision::Stay => own.map(String::from),
        crate::accounts::Decision::Move { to, why } => {
            store.thread_account(w.thread, Some(&w.member), Some(&to), Some(&why))?;
            Db::open_default()?.record(
                "operator",
                "agents.accounts.switch",
                &serde_json::json!({"thread": w.thread, "member": w.member, "from": own, "to": to, "why": why}),
            )?;
            Some(to)
        }
    })
}

/// `thread account`: an operator member's account, or the chat bar's; none: the current login.
pub fn account(input: &legionctl_api::ThreadAccountIn) -> Result<Lines> {
    let account = input.account.as_deref();
    dossiers()?.thread_account(input.thread, input.member.as_deref(), account, None)?;
    let account = account.map_or_else(|| "the current login".into(), |a| format!("account {a}"));
    Ok(lines(vec![match &input.member {
        Some(member) => format!(
            "thread {}: {member} runs on {account} from its next wake",
            input.thread
        ),
        None => format!("thread {}: the chat bar runs on {account}", input.thread),
    }]))
}

/// The member's session for this wake on its account (`account_of`): made at its first wake (headless on the host,
/// the capability sets of its config with the main agent's as its parent), else resumed, with the token of its run.
fn launch(
    store: &legionctl_core::dossier::Dossiers,
    w: &legionctl_core::pool::Wake,
) -> Result<(legionctl_core::session::Launch, String)> {
    let resolved = resolve(&Dirs::from_env(), Some(&w.workdir))?;
    let account = account_of(store, w)?;
    let token = legionctl_api::random_token()?;
    let name = format!("{}/{}", w.thread, w.member);
    let launch = match &w.session {
        None => {
            let launch = store.start_session(
                &Start {
                    agent: &w.agent,
                    dossier: &w.dossier,
                    steal: false,
                    token: &token,
                    dir: &w.workdir,
                    topic: None,
                    headless: true,
                    account: account.as_deref(),
                    inside: Inside::Host,
                    gpu: false,
                    member: Some((&name, &w.workdir)),
                    // a member is in a thread: its messages are part of its work
                    capabilities: &[
                        member_sets(&w.fields)?,
                        legionctl_core::archetypes::declared_toolsets(
                            &resolved.config,
                            w.archetype.as_str(),
                        ),
                        vec!["thread-member".into()],
                    ]
                    .concat(),
                    parent: Some(&main_sets(&resolved.config)),
                    tools: &[],
                },
                &resolved.config,
            )?;
            store.member_session(w, &launch.id)?;
            launch
        }
        Some(id) => store.resume_session(
            id,
            &resolved.config,
            account.as_deref(),
            &token,
            (true, &[]),
        )?,
    };
    Ok((launch, token))
}

/// The prompt of a member's wake: its archetype's instructions with the member's own, the escalation's brief, then
/// the wake's prompt.
fn wake_prompt(
    w: &legionctl_core::pool::Wake,
    escalated: Option<&(i64, String)>,
) -> Result<String> {
    // no instructions field: none; a field that is not a list of texts fails the wake (never a wake without them)
    let extra: Vec<String> = match &w.fields["instructions"] {
        serde_json::Value::Null => Vec::new(),
        value => serde_json::from_value(value.clone())
            .map_err(|e| anyhow::anyhow!("member {} instructions: {e}", w.member))?,
    };
    let instructions = Sources::resolve(&Dirs::from_env(), Some(&w.workdir))?
        .instructions(w.archetype.as_str(), &extra)?;
    Ok(match escalated {
        Some((_, brief)) => format!("{instructions}\n\n{brief}\n\n{}", w.prompt),
        None => format!("{instructions}\n\n{}", w.prompt),
    })
}

/// The capability sets of a member's resolved config (`runnables check` shows them).
fn member_sets(fields: &serde_json::Value) -> Result<Vec<String>> {
    match &fields["capabilities"] {
        serde_json::Value::Null => Ok(Vec::new()),
        value => serde_json::from_value(value.clone())
            .map_err(|e| anyhow::anyhow!("the member's capabilities: {e}")),
    }
}

/// A member's parent: the sets of the config main-agent (the main agent of its thread), with its skills' toolsets.
// shortcut: a team whose main0 uses another config still has main-agent's sets as the parent; upgrade when a thread
// keeps its main agent's grant
fn main_sets(config: &legionctl_core::config::Config) -> Vec<String> {
    legionctl_core::archetypes::requested_sets(config, "main-agent")
}

/// The acp step of a wake: the member's session (made at its first wake), one ACP prompt with its instructions, the
/// rlcd brief of an escalate, and the events since its cursor, within its budget. The answer is the outcome of the
/// escalating decision. Returns true when it must wake again now (a failed wake within the restart limit).
fn acp_wake(
    store: &legionctl_core::dossier::Dossiers,
    w: &legionctl_core::pool::Wake,
    escalated: Option<&(i64, String)>,
) -> Result<bool> {
    let (thread, member) = (w.thread, w.member.as_str());
    let name = format!("{thread}/{member}");
    // from here each error ends the wake through wake_end: the restart limit applies, and the request does not stay due
    let started = launch(store, w);
    let ran = started.and_then(|(launch, token)| {
        let id = launch.id.clone();
        let db = Db::open_default()?;
        db.issue_token(&launch.id, &token)?;
        let prompt = wake_prompt(w, escalated)?;
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
            store,
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
    let ran = ran.and_then(|r| r);
    let outcome = match &ran {
        Ok((out, over, _)) => {
            serde_json::json!({"ok": true, "over": over, "text": out.text.chars().take(2000).collect::<String>()})
        }
        Err(e) => serde_json::json!({"error": format!("{e:#}")}),
    };
    let end = match ran {
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
    let again = store.wake_end(w, &end)?;
    if let Some((decision, _)) = escalated {
        store.decision_acp(*decision, &outcome)?;
    }
    Ok(again)
}

/// The threads, each with the folder its runs work in (`root`: config workspace.root, as a run takes it).
pub fn list(root: &Path) -> Result<Vec<ThreadInfo>> {
    let mut threads = dossiers()?.threads()?;
    for t in &mut threads {
        in_scratch(root, t);
    }
    Ok(threads.into_wire())
}

pub fn show(root: &Path, input: &ThreadIdIn) -> Result<ThreadView> {
    let mut view = dossiers()?.thread_show(input.thread)?;
    in_scratch(root, &mut view.thread);
    Ok(view.into_wire())
}

/// A thread on no folder works in its scratch folder: the app's chat line and the agent name the same folder.
fn in_scratch(root: &Path, t: &mut legionctl_core::thread::ThreadInfo) {
    if t.repo.is_none() {
        t.folder = Some(
            legionctl_core::thread::scratch_folder(root, t.id)
                .to_string_lossy()
                .into_owned(),
        );
    }
}

/// The chat bar's agent of a thread: a name of `session.acp` in the config of the thread's folder `dir`, else
/// refused with the names.
pub fn agent(dir: &Path, input: &legionctl_api::ThreadAgentIn) -> Result<Lines> {
    let config = crate::engine::config(Some(dir))?;
    let names: Vec<&String> = config.config.session.acp.keys().collect();
    anyhow::ensure!(
        names.contains(&&input.agent),
        "no agent {}; the agents are: {}",
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
    let mut out = vec![format!("message #{id} to {} ({})", input.to, input.kind)];
    let limit = crate::engine::config(None)?.config.dossier.message_chars;
    if limit > 0 && input.text.chars().count() > limit && !links_dossier(&input.text) {
        out.push(format!(
            "hint: the message has more than {limit} characters and no dossier link: put the details in the dossier \
             and link them (plan#todo-NN, MEMORY-n, EVIDENCE/..., a commit)"
        ));
    }
    Ok(lines(out))
}

/// The text links a dossier artifact or a commit: `plan#`, a class name (`MEMORY-3`, `EVIDENCE/...`), or a word of
/// 7 to 40 hex digits with a digit in it.
fn links_dossier(text: &str) -> bool {
    const MARKS: [&str; 7] = [
        "plan#", "MEMORY", "DESIGN", "EVIDENCE", "SPIKE", "JOURNAL", "HEALTH",
    ];
    MARKS.iter().any(|m| text.contains(m))
        || text.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| {
            (7..=40).contains(&w.len())
                && w.bytes().all(|b| b.is_ascii_hexdigit())
                && w.bytes().any(|b| b.is_ascii_digit())
        })
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

pub fn peers(dir: &Path, input: &PeersIn) -> Result<Vec<Peer>> {
    let scope = Scope {
        dossier: input.dossier.clone(),
        topic: input.topic.clone(),
        thread: input.thread,
    };
    Ok(dossiers()?
        .peers(&Db::open_default()?, &input.session.0, dir, &scope)?
        .into_wire())
}

pub fn observe(dir: &Path, input: &ObserveIn) -> Result<Observed> {
    Ok(dossiers()?
        .observe(&Db::open_default()?, &input.session.0, dir, &input.peer)?
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
