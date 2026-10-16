//! `legionctl hook <event>`: the adapter between Claude Code's hooks and the operator. It reads the hook's JSON from
//! stdin (`session_id`, `cwd`, `prompt`, `tool_input`; in a legionctl session `LEGIONCTL_SESSION` replaces
//! `session_id`) and answers in Claude Code's hook protocol:
//!
//! | Event | Operation | Answer |
//! | --- | --- | --- |
//! | session-start | `internal.hooks.run` | stdout: added to the context |
//! | prompt | `internal.hooks.run` | stdout, or `{"decision":"block","reason":...}` from the dossier gate |
//! | pre-tool | `internal.thread.gate`, `internal.dossier.guard` | `permissionDecision: deny` for a tool that a thread hard-block holds, or a file tool's write (`Write`, `Edit`, `MultiEdit`, `NotebookEdit`) into a dossier: another one, or the session's own, which the dossier commands write with their checks |
//! | subagent-start | `internal.hooks.delegate` | `additionalContext` for the subagent: the delegation rule of the session's dossier (none without a dossier) |
//! | pre-compact | `internal.hooks.compact` | nothing (Claude Code takes no instructions for the summary from a `PreCompact` hook): the session's dossier gets an immutable JOURNAL entry and HEALTH note, and the session start after the compaction (`source: compact`) points to them |
//! | post-tool | `internal.agent.tool` | the call is recorded (thread log, watch rules, presence) per peer: a subagent (`agent_id`) is a peer of its own and gets its messages as `additionalContext` |
//! | stop | `internal.hooks.run` | `{"decision":"block","reason":...}` to the agent when the session is not set up (once per reply), or for a thread block |
//! | session-end | `dossier.release` | nothing; its `reason` `clear` or `resume` keeps the pool that the session leads |
//! | teammate-idle | `internal.thread.teammate` | exit 2 with the teammate's new thread messages on stderr: it goes on with them |
//! | task-completed | `internal.thread.teammate` | exit 2 with the open items of the thread's `task:completed` gate: the task stays open |
//!
//! Only these rules block. A failure (legionctl-operator is down, the input is not the hook's JSON, an event that
//! this build does not know) handles nothing: it goes to stderr with exit 1, which Claude Code shows and does not
//! block on. No failure exits 2, because exit 2 blocks the agent.

use anyhow::{Context, Result};
use legionctl_api::{
    AgentToolIn, ClientPath, CompactIn, DossierReleaseIn, Event, Guard, GuardIn, HookReply,
    HookRunIn, SessionId, SessionIn, TeammateIn, ThreadGateIn, op,
};
use serde_json::{Value, json};

use legionctl_api::client::Client;
use legionctl_core::plugin::HOOK_EVENTS;

/// What the hook gives back: text on stdout (exit 0), or the feedback of a team mode hook on stderr (exit 2).
enum Answer {
    Out(String),
    KeepWorking(String),
}

/// The project of the hook input: its `cwd`, which Claude Code always sends.
fn project(input: &Value) -> Result<ClientPath> {
    Ok(ClientPath(
        input["cwd"]
            .as_str()
            .context("the hook input has no cwd")?
            .into(),
    ))
}

async fn answer(event: &str, input: &Value) -> Result<Option<Answer>> {
    let client = Client::new("hook")?;
    // in a legionctl session the dossier is bound to its id, not to the id of the agent inside the sandbox
    let session = std::env::var("LEGIONCTL_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| input["session_id"].as_str().map(String::from))
        .map(SessionId);
    let project = project(input)?;
    let run = |event: Event| HookRunIn {
        event,
        project: project.clone(),
        session: session.clone(),
        prompt: input["prompt"].as_str().map(String::from),
        // Claude Code sets stop_hook_active when a stop hook already made the agent go on in this reply
        again: input["stop_hook_active"] == true,
        source: input["source"].as_str().map(String::from),
    };
    let tool = &input["tool_input"];
    let path = tool["file_path"]
        .as_str()
        .or(tool["notebook_path"].as_str())
        .map(String::from);
    if let ("teammate-idle" | "task-completed", Some(session)) = (event, &session) {
        return team_mode(&client, event, input, session).await;
    }
    Ok(match event {
        "session-start" => Some(
            client
                .call::<op::HooksRun>(&run(Event::SessionStart))
                .await?
                .text,
        ),
        "prompt" => {
            let HookReply { text, block } =
                client.call::<op::HooksRun>(&run(Event::Prompt)).await?;
            Some(match block {
                Some(reason) => json!({"decision": "block", "reason": reason}).to_string(),
                None => text,
            })
        }
        "pre-tool" => {
            let Some(session) = session else {
                return Ok(None);
            };
            pre_tool(&client, session, input, path).await?
        }
        // additionalContext of SubagentStart reaches the subagent before its first prompt; PreToolUse's updatedInput
        // would need permissionDecision allow (it skips the permission prompt) to be documented
        "subagent-start" => match session {
            Some(session) => {
                let text = client
                    .call::<op::HooksDelegate>(&SessionIn { session })
                    .await?
                    .text;
                (!text.is_empty()).then(|| {
                    json!({"hookSpecificOutput": {
                        "hookEventName": "SubagentStart",
                        "additionalContext": text,
                    }})
                    .to_string()
                })
            }
            None => None,
        },
        "pre-compact" => {
            if let Some(session) = session {
                client
                    .call::<op::HooksCompact>(&CompactIn {
                        trigger: input["trigger"].as_str().map(String::from),
                        session,
                    })
                    .await?;
            }
            None
        }
        "post-tool" => match session {
            Some(session) => post_tool(&client, session, project, input, path).await?,
            None => None,
        },
        "stop" => client
            .call::<op::HooksRun>(&run(Event::Stop))
            .await?
            .block
            .map(|reason| json!({"decision": "block", "reason": reason}).to_string()),
        "session-end" => {
            if let Some(session) = session {
                client
                    .call::<op::DossierRelease>(&DossierReleaseIn {
                        session,
                        reason: input["reason"].as_str().map(String::from),
                    })
                    .await?;
            }
            None
        }
        "teammate-idle" | "task-completed" => None,
        other => anyhow::bail!("unknown hook event {other}"),
    }
    .map(Answer::Out))
}

/// The pre-tool hook: a thread hard-block that holds the tool, else the dossier write guard, as a deny.
async fn pre_tool(
    client: &Client,
    session: SessionId,
    input: &Value,
    path: Option<String>,
) -> Result<Option<String>> {
    let held = client
        .call::<op::ThreadGate>(&ThreadGateIn {
            session: session.clone(),
            tool: input["tool_name"].as_str().unwrap_or_default().to_string(),
        })
        .await?;
    // the guard is on writes: every dossier can be read
    let writes = matches!(
        input["tool_name"].as_str(),
        Some("Write" | "Edit" | "MultiEdit" | "NotebookEdit")
    );
    let refusal = match (held.refusal, path.filter(|_| writes)) {
        (Some(reason), _) => Some(reason),
        (None, Some(path)) => {
            let Guard { refusal } = client
                .call::<op::DossierGuard>(&GuardIn { path, session })
                .await?;
            refusal
        }
        (None, None) => None,
    };
    Ok(refusal.map(|reason| {
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }})
        .to_string()
    }))
}

/// A team mode hook of the lead: the text that keeps the teammate working or holds the task.
async fn team_mode(
    client: &Client,
    event: &str,
    input: &Value,
    session: &SessionId,
) -> Result<Option<Answer>> {
    let Guard { refusal } = client
        .call::<op::ThreadTeammate>(&TeammateIn {
            event: if event == "teammate-idle" {
                "idle"
            } else {
                "task"
            }
            .into(),
            teammate: input["teammate_name"].as_str().map(String::from),
            session: session.clone(),
        })
        .await?;
    Ok(refusal.map(Answer::KeepWorking))
}

/// Record a tool call; a subagent's call (`agent_id`) is a peer of its own and gets its messages back, as it has no
/// prompt hook.
async fn post_tool(
    client: &Client,
    session: SessionId,
    project: ClientPath,
    input: &Value,
    path: Option<String>,
) -> Result<Option<String>> {
    let text = |key: &str| input["tool_input"][key].as_str().map(String::from);
    let told = client
        .call::<op::AgentTool>(&AgentToolIn {
            session,
            agent: input["agent_id"].as_str().map(String::from),
            agent_type: input["agent_type"].as_str().map(String::from),
            project,
            tool: input["tool_name"].as_str().unwrap_or_default().to_string(),
            path,
            command: text("command"),
            to: text("to"),
            message: text("message"),
            // a tool that reports its result has `success: false` when it failed (SendMessage)
            ok: input["tool_response"]["success"] != false,
        })
        .await?
        .text;
    Ok((!told.is_empty()).then(|| {
        json!({"hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": told,
        }})
        .to_string()
    }))
}

/// `legionctl hook <args>`: exit 0 with the answer on stdout, exit 2 when a team mode rule keeps the teammate
/// working or the task open, exit 1 when the hook failed.
pub fn run(args: &[String]) {
    let answered = match args {
        [event] if HOOK_EVENTS.contains(&event.as_str()) => {
            serde_json::from_reader::<_, Value>(std::io::stdin())
                .context("stdin is not the hook's JSON")
                .and_then(|input| tokio::runtime::Runtime::new()?.block_on(answer(event, &input)))
        }
        _ => Err(not_known(args)),
    };
    match answered.context("legionctl hook") {
        Ok(Some(Answer::Out(text))) if !text.is_empty() => println!("{text}"),
        Ok(Some(Answer::KeepWorking(text))) => {
            eprintln!("{text}");
            std::process::exit(2);
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
    }
}

/// The arguments name no event of this build: the plugin that runs the hook is of another (newer) build.
fn not_known(args: &[String]) -> anyhow::Error {
    let exe = std::env::current_exe().map_or_else(|e| e.to_string(), |p| p.display().to_string());
    anyhow::anyhow!(
        "this legionctl ({exe} {}, API build {}) does not know `hook {}`: the legionctl plugin is newer than \
         this legionctl, and the hook did nothing; run legionctl doctor\nthe events of this legionctl: {}",
        env!("CARGO_PKG_VERSION"),
        legionctl_api::api_fingerprint(),
        args.join(" "),
        HOOK_EVENTS.join(", ")
    )
}
