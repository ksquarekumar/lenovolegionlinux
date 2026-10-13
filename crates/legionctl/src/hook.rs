//! `legionctl hook <event>`: the adapter between Claude Code's hooks and the operator. It reads the hook's JSON from
//! stdin (`session_id`, `cwd`, `prompt`, `tool_input`; in a legionctl session `LEGIONCTL_SESSION` replaces
//! `session_id`) and answers in Claude Code's hook protocol:
//!
//! | Event | Operation | Answer |
//! | --- | --- | --- |
//! | session-start | `internal.hooks.run` | stdout: added to the context |
//! | prompt | `internal.hooks.run` | stdout, or `{"decision":"block","reason":...}` from the dossier gate |
//! | pre-tool | `internal.dossier.guard` | `permissionDecision: deny` for a write into another dossier |
//! | stop | `internal.hooks.run` | `{"decision":"block","reason":...}` to the agent when the session is not set up; once per reply |
//! | session-end | `dossier.release` | nothing |
//!
//! A hook must never stop the agent because legionctl is down: errors go to stderr and the exit code stays 0.

use anyhow::{Context, Result};
use legionctl_api::{
    ClientPath, Event, Guard, GuardIn, HookReply, HookRunIn, SessionId, SessionIn, op,
};
use serde_json::{Value, json};

use legionctl_api::client::Client;

pub const EVENTS: [&str; 5] = ["session-start", "prompt", "pre-tool", "stop", "session-end"];

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
    };
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
            let tool = &input["tool_input"];
            let Some(path) = tool["file_path"]
                .as_str()
                .or(tool["notebook_path"].as_str())
            else {
                return Ok(None);
            };
            let Some(session) = session else {
                return Ok(None);
            };
            let Guard { refusal } = client
                .call::<op::DossierGuard>(&GuardIn {
                    path: path.to_string(),
                    session,
                })
                .await?;
            refusal.map(|reason| {
                json!({"hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }})
                .to_string()
            })
        }
        // Claude Code sets stop_hook_active when this hook already made the agent continue: never block twice
        "stop" if input["stop_hook_active"] == true => None,
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
