//! `legionctl hook <event>`: the adapter between Claude Code's hooks and the operator. It reads the hook's JSON from
//! stdin (`session_id`, `cwd`, `prompt`, `tool_input`; in a legionctl session `LEGIONCTL_SESSION` replaces
//! `session_id`) and answers in Claude Code's hook protocol:
//!
//! | Event | Operation | Answer |
//! | --- | --- | --- |
//! | session-start | `internal.hooks.run` | stdout: added to the context |
//! | prompt | `internal.hooks.run` | stdout, or `{"decision":"block","reason":...}` from the dossier gate |
//! | pre-tool | `internal.thread.gate`, `internal.dossier.guard` | `permissionDecision: deny` for a tool that a thread hard-block holds, or a write into another dossier |
//! | post-tool | `internal.agent.tool` | nothing: the call is recorded (thread log, watch rules, presence) |
//! | stop | `internal.hooks.run` | `{"decision":"block","reason":...}` to the agent when the session is not set up (once per reply), or for a thread block |
//! | session-end | `dossier.release` | nothing |
//!
//! A hook must never stop the agent because legionctl is down: errors go to stderr and the exit code stays 0.

use anyhow::{Context, Result};
use legionctl_api::{
    AgentToolIn, ClientPath, Event, Guard, GuardIn, HookReply, HookRunIn, SessionId, SessionIn,
    ThreadGateIn, op,
};
use serde_json::{Value, json};

use legionctl_api::client::Client;

pub const EVENTS: [&str; 6] = [
    "session-start",
    "prompt",
    "pre-tool",
    "post-tool",
    "stop",
    "session-end",
];

async fn answer(event: &str, input: &Value) -> Result<Option<String>> {
    let client = Client::new("hook")?;
    // in a legionctl session the dossier is bound to its id, not to the id of the agent inside the sandbox
    let session = std::env::var("LEGIONCTL_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| input["session_id"].as_str().map(String::from))
        .map(SessionId);
    let project = ClientPath(input["cwd"].as_str().map_or_else(|| ".".into(), Into::into));
    let run = |event: Event| HookRunIn {
        event,
        project: project.clone(),
        session: session.clone(),
        prompt: input["prompt"].as_str().map(String::from),
        // Claude Code sets stop_hook_active when a stop hook already made the agent go on in this reply
        again: input["stop_hook_active"] == true,
    };
    let tool = &input["tool_input"];
    let path = tool["file_path"]
        .as_str()
        .or(tool["notebook_path"].as_str())
        .map(String::from);
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
            let held = client
                .call::<op::ThreadGate>(&ThreadGateIn {
                    session: session.clone(),
                    tool: input["tool_name"].as_str().unwrap_or_default().to_string(),
                })
                .await?;
            let refusal = match (held.refusal, path) {
                (Some(reason), _) => Some(reason),
                (None, Some(path)) => {
                    let Guard { refusal } = client
                        .call::<op::DossierGuard>(&GuardIn { path, session })
                        .await?;
                    refusal
                }
                (None, None) => None,
            };
            refusal.map(|reason| {
                json!({"hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }})
                .to_string()
            })
        }
        "post-tool" => {
            let Some(session) = session else {
                return Ok(None);
            };
            let text = |key: &str| tool[key].as_str().map(String::from);
            client
                .call::<op::AgentTool>(&AgentToolIn {
                    session,
                    project: project.clone(),
                    tool: input["tool_name"].as_str().unwrap_or_default().to_string(),
                    path,
                    command: text("command"),
                    to: text("to"),
                    message: text("message"),
                    // a tool that reports its result has `success: false` when it failed (SendMessage)
                    ok: input["tool_response"]["success"] != false,
                })
                .await?;
            None
        }
        "stop" => client
            .call::<op::HooksRun>(&run(Event::Stop))
            .await?
            .block
            .map(|reason| json!({"decision": "block", "reason": reason}).to_string()),
        "session-end" => {
            if let Some(session) = session {
                client
                    .call::<op::DossierRelease>(&SessionIn { session })
                    .await?;
            }
            None
        }
        other => anyhow::bail!("unknown hook event {other}"),
    })
}

pub fn run(event: &str) -> Result<()> {
    let input: Value = serde_json::from_reader(std::io::stdin()).unwrap_or(Value::Null);
    let runtime = tokio::runtime::Runtime::new()?;
    match runtime
        .block_on(answer(event, &input))
        .context("legionctl hook")
    {
        Ok(Some(text)) if !text.is_empty() => println!("{text}"),
        Ok(_) => {}
        Err(e) => eprintln!("{e:#}"),
    }
    Ok(())
}
