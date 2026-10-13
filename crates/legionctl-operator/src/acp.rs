//! Headless runs (note 14, S3): the agent's ACP adapter runs in the session's sandbox with the git gate, through
//! adk-rust's ACP client. The session's MCP servers (legionctl's own and the proxied ones) go to the adapter at
//! `session/new`. Permission asks are allowed, because the sandbox and the gates are the boundary; each is logged.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use adk_acp::{AcpAgentConfig, AcpSession, PermissionDecision, PermissionPolicy};
use agent_client_protocol::schema::v1::McpServer;
use anyhow::{Context, Result};
use legionctl_api::RunOutput;
use legionctl_core::db::Db;
use legionctl_core::session::Launch;

/// Run `prompt` in the prepared session and return the agent's answer.
///
/// # Errors
/// The adapter does not start or speak ACP, or the prompt fails.
pub async fn run(launch: Launch, prompt: &str) -> Result<RunOutput> {
    let started = Instant::now();
    let legionctl = crate::engine::which("legionctl")
        .context("legionctl is not on PATH (the git gate is its binary)")?;
    let gate = [
        "--ro-bind".to_string(),
        legionctl.display().to_string(),
        "/usr/bin/git".into(),
        "--setenv".into(),
        "LEGIONCTL_REAL_GIT".into(),
        launch.real_git.display().to_string(),
    ];
    let argv = [launch.scope, launch.sandbox, gate.to_vec(), launch.agent].concat();
    // the JSON form of a stdio server: the argv as it is, no shell quoting
    let command =
        serde_json::json!({ "name": "agent", "command": argv[0], "args": argv[1..], "env": [] });
    let mut config = AcpAgentConfig::new(command.to_string()).working_dir(&launch.workdir);
    config.mcp_servers = launch.mcp["mcpServers"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, server)| {
            let mut stdio = server.clone();
            stdio["name"] = name.clone().into();
            stdio["env"] = serde_json::json!([]);
            serde_json::from_value::<McpServer>(stdio).with_context(|| format!("MCP server {name}"))
        })
        .collect::<Result<_>>()?;

    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    let id = launch.id.clone();
    let policy = PermissionPolicy::Custom(Box::new(move |request| {
        let line = format!("{} ({:?}): allowed", request.title, request.kind);
        let payload =
            serde_json::json!({ "session": id, "tool": request.title, "decision": "allow_once" });
        if let Err(e) =
            Db::open_default().and_then(|db| db.record("acp", "session.permission", &payload))
        {
            eprintln!("operator: cannot log a permission of session {id}: {e:#}");
        }
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line);
        PermissionDecision::AllowOnce
    }));
    let mut session = AcpSession::start(config, Arc::new(policy)).await?;
    let answer = session.prompt(prompt).await?;
    // session/close is optional in ACP; without it, dropping the session ends the adapter
    if let Err(e) = session.close().await {
        eprintln!(
            "operator: session {}: the agent did not close: {e}",
            launch.id
        );
    }
    let permissions = asked
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    Ok(RunOutput {
        id: launch.id,
        text: answer.text,
        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        permissions,
    })
}
