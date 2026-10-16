//! Headless runs (note 14, S3): the agent's ACP adapter runs in the session's sandbox with the git gate. The
//! session's MCP servers (legionctl's own and the proxied ones) go to the adapter at `session/new`. Permission asks
//! are allowed, because the sandbox and the gates are the boundary; each is logged. The answer's chunks and tool calls
//! are events (`session.output`) while the run goes on, and the ACP session id goes in the job's input, so a run that
//! a restart cut off goes on with `session/load` when the agent can load sessions.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AvailableCommandInput, CloseSessionRequest, ContentBlock, ContentChunk, InitializeRequest,
    LoadSessionRequest, McpServer, McpServerStdio, NewSessionRequest, PermissionOptionKind,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::{AcpAgent, Client};
use anyhow::{Context, Result};
use legionctl_api::{AgentCommand, RunOutput};
use legionctl_core::db::Db;
use legionctl_core::dossier::Dossiers;
use legionctl_core::session::{self, Inside, Launch};

/// Run `prompt` in the prepared session as the job `job`, whose `input` has the `tag` of the output events and,
/// after a first start, the ACP session to load; return the agent's answer. Then the session ends where it runs and
/// releases its dossier (in `dossiers`).
///
/// # Errors
/// The adapter does not start or speak ACP, or the prompt fails.
pub async fn run(
    launch: Launch,
    prompt: &str,
    (job, input): (i64, &serde_json::Value),
    stop: tokio::sync::oneshot::Receiver<()>,
    dossiers: &Dossiers,
) -> Result<RunOutput> {
    Ok(run_within(
        launch,
        prompt,
        (job, input),
        (stop, Budget::default()),
        dossiers,
    )
    .await?
    .0)
}

/// The limits of one run (a member's wake, design 23 B5): the answer's tokens and the run's seconds.
#[derive(Default)]
pub struct Budget {
    pub tokens: Option<u64>,
    pub seconds: Option<u64>,
}

impl Budget {
    /// The limit that an answer of `text` after `seconds` went past.
    fn reached(&self, text: &str, seconds: u64) -> Option<&'static str> {
        let past = |limit: Option<u64>, used: u64| limit.is_some_and(|l| used > l);
        if past(self.tokens, legionctl_core::pool::tokens(text)) {
            Some("tokens")
        } else if past(self.seconds, seconds) {
            Some("seconds")
        } else {
            None
        }
    }
}

/// `run` within `budget`: a run that reaches a limit ends there (its adapter stops, as at an offload), and the
/// answer so far comes back with the limit's name (`tokens`, `seconds`).
///
/// # Errors
/// The adapter does not start or speak ACP, or the prompt fails.
pub async fn run_within(
    launch: Launch,
    prompt: &str,
    (job, input): (i64, &serde_json::Value),
    (stop, budget): (tokio::sync::oneshot::Receiver<()>, Budget),
    dossiers: &Dossiers,
) -> Result<(RunOutput, Option<&'static str>)> {
    let (id, inside) = (launch.id.clone(), launch.inside);
    let out = drive(launch, prompt, (job, input.clone()), (stop, budget)).await;
    // the adapter can outlive its closed stdio, and a container or VM outlives its killed client
    match session::active(&id, inside) {
        Ok(true) => {
            if let Err(e) = session::control(&id, inside, session::Control::Stop) {
                eprintln!("operator: session {id}: cannot stop it after its run: {e:#}");
            }
        }
        Ok(false) => {}
        Err(e) => eprintln!("operator: session {id}: not stopped after its run: {e:#}"),
    }
    // the dossier is free at once, also when the run failed in the hold's first minute
    if let Err(e) = dossiers.release(&id) {
        eprintln!("operator: session {id}: cannot release its dossier: {e:#}");
    }
    out
}

/// What a run saw so far: the answer (from its prompt on; a loaded session replays its history first) and the
/// permissions it allowed.
struct Live {
    db: Db,
    session: String,
    tag: serde_json::Value,
    prompting: bool,
    text: String,
    permissions: Vec<String>,
    /// The answer's token limit, and the signal when the answer reaches it.
    limit: Option<u64>,
    over: Arc<tokio::sync::Notify>,
}

impl Live {
    fn record(&self, kind: &str, mut payload: serde_json::Value) {
        payload["session"] = self.session.clone().into();
        if let Err(e) = self.db.record("acp", kind, &payload) {
            eprintln!(
                "operator: session {}: cannot log a {kind} event: {e:#}",
                self.session
            );
        }
    }

    /// Allow `request` once, and log it.
    fn allow(&mut self, request: &RequestPermissionRequest) -> RequestPermissionResponse {
        let call = &request.tool_call;
        let title = call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| call.tool_call_id.to_string());
        let kind = call.fields.kind.unwrap_or_default();
        self.record(
            "session.permission",
            serde_json::json!({ "tool": title, "decision": "allow_once" }),
        );
        self.permissions
            .push(format!("{title} ({kind:?}): allowed"));
        let offered = |kind| request.options.iter().find(|o| o.kind == kind);
        let allow = offered(PermissionOptionKind::AllowOnce)
            .or_else(|| offered(PermissionOptionKind::AllowAlways));
        RequestPermissionResponse::new(allow.map_or(RequestPermissionOutcome::Cancelled, |o| {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(o.option_id.clone()))
        }))
    }

    /// The answer's chunks and the tool calls, as they come.
    fn update(&mut self, update: SessionUpdate) {
        // the agent sends its commands after session/new or session/load, before the prompt
        if let SessionUpdate::AvailableCommandsUpdate(list) = &update {
            let commands: Vec<AgentCommand> = list
                .available_commands
                .iter()
                .map(|c| AgentCommand {
                    name: c.name.clone(),
                    description: c.description.clone(),
                    hint: match &c.input {
                        Some(AvailableCommandInput::Unstructured(input)) => {
                            Some(input.hint.clone())
                        }
                        _ => None,
                    },
                })
                .collect();
            self.record(
                "session.commands",
                serde_json::json!({ "commands": commands }),
            );
            return;
        }
        if !self.prompting {
            return;
        }
        let tag = self.tag.clone();
        match update {
            SessionUpdate::AgentMessageChunk(ContentChunk {
                content: ContentBlock::Text(chunk),
                ..
            }) => {
                self.record(
                    "session.output",
                    serde_json::json!({ "tag": tag, "text": chunk.text }),
                );
                self.text.push_str(&chunk.text);
                if self
                    .limit
                    .is_some_and(|l| legionctl_core::pool::tokens(&self.text) > l)
                {
                    self.over.notify_one();
                }
            }
            SessionUpdate::ToolCall(call) => self.record(
                "session.output",
                serde_json::json!({ "tag": tag, "tool": call.title }),
            ),
            _ => {}
        }
    }
}

/// Ends after `seconds`, or never.
async fn time_limit(seconds: Option<u64>) {
    match seconds {
        Some(s) => tokio::time::sleep(std::time::Duration::from_secs(s)).await,
        None => std::future::pending().await,
    }
}

impl Live {
    /// The run's output; its ACP session id from the job, also when the connection was dropped.
    fn output(&self, id: String, job: i64, started: Instant) -> Result<RunOutput> {
        Ok(RunOutput {
            id,
            text: self.text.clone(),
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            permissions: self.permissions.clone(),
            acp: self
                .db
                .job(job)?
                .and_then(|j| j.input["acp"].as_str().map(String::from)),
        })
    }
}

fn lock(live: &Mutex<Live>) -> MutexGuard<'_, Live> {
    live.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn drive(
    launch: Launch,
    prompt: &str,
    (job, mut input): (i64, serde_json::Value),
    (stop, budget): (tokio::sync::oneshot::Receiver<()>, Budget),
) -> Result<(RunOutput, Option<&'static str>)> {
    let started = Instant::now();
    let (id, workdir) = (launch.id.clone(), launch.workdir.clone());
    let servers = servers(&launch)?;
    let agent = adapter(launch)?;
    let over = Arc::new(tokio::sync::Notify::new());
    let live = Arc::new(Mutex::new(Live {
        db: Db::open_default()?,
        session: id.clone(),
        tag: input["tag"].clone(),
        prompting: false,
        text: String::new(),
        permissions: Vec::new(),
        limit: budget.tokens,
        over: over.clone(),
    }));
    let (asks, updates) = (live.clone(), live.clone());
    let client = Client
        .builder()
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _| {
                responder.respond(lock(&asks).allow(&request))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: SessionNotification, _| {
                lock(&updates).update(notification.update);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(agent, async |cx| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            if input["load"] == true && !init.agent_capabilities.load_session {
                return Err(anyhow::anyhow!(
                    "the agent cannot load an ACP session, so the run cannot go on in {}",
                    input["acp"]
                )
                .into());
            }
            let loading = input["acp"]
                .as_str()
                .filter(|_| init.agent_capabilities.load_session)
                .map(|id| SessionId::new(id.to_string()));
            let acp = if let Some(acp) = loading {
                cx.send_request(
                    LoadSessionRequest::new(acp.clone(), &workdir).mcp_servers(servers),
                )
                .block_task()
                .await?;
                acp
            } else {
                let acp = cx
                    .send_request(NewSessionRequest::new(&workdir).mcp_servers(servers))
                    .block_task()
                    .await?
                    .session_id;
                input["acp"] = acp.to_string().into();
                if let Err(e) = lock(&live).db.job_input(job, &input) {
                    eprintln!("operator: session {id}: cannot keep its ACP session id: {e:#}");
                }
                acp
            };
            lock(&live).prompting = true;
            cx.send_request(PromptRequest::new(
                acp.clone(),
                vec![ContentBlock::Text(TextContent::new(prompt))],
            ))
            .block_task()
            .await?;
            if init.agent_capabilities.session_capabilities.close.is_some() {
                cx.send_request(CloseSessionRequest::new(acp))
                    .block_task()
                    .await?;
            }
            Ok(())
        });
    let seconds = time_limit(budget.seconds);
    // a stop (offload) or a budget drops the connection, which ends the adapter
    let reached = tokio::select! {
        _ = stop => anyhow::bail!("offloaded: memory is under pressure"),
        () = over.notified() => Some("tokens"),
        () = seconds => Some("seconds"),
        done = client => { done?; None }
    };
    let live = lock(&live);
    // an answer that came in whole within one poll reached its limit too
    let reached = reached.or_else(|| budget.reached(&live.text, started.elapsed().as_secs()));
    Ok((live.output(id, job, started)?, reached))
}

/// The session's MCP servers as ACP's `session/new` takes them.
fn servers(launch: &Launch) -> Result<Vec<McpServer>> {
    launch.mcp["mcpServers"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, server)| {
            let mut stdio = server.clone();
            stdio["name"] = name.clone().into();
            stdio["env"] = serde_json::json!([]);
            serde_json::from_value::<McpServer>(stdio).with_context(|| format!("MCP server {name}"))
        })
        .collect()
}

/// The adapter's process: the session's scope and sandbox, the git gate and the agent's ACP command.
fn adapter(launch: Launch) -> Result<AcpAgent> {
    // legionctl is the git gate on the host and a VM's runner; a container's image has its own gate
    let mut sandbox = launch.sandbox;
    let mut gate = Vec::new();
    if launch.inside != Inside::Container {
        let legionctl = legionctl_machine::policy::which("legionctl")
            .context("legionctl is not on PATH (the git gate and the VM runner are its binary)")?
            .display()
            .to_string();
        if launch.inside == Inside::Vm {
            sandbox.insert(0, legionctl);
        } else {
            gate.extend([
                "--ro-bind".to_string(),
                legionctl,
                "/usr/bin/git".into(),
                "--setenv".into(),
                "LEGIONCTL_REAL_GIT".into(),
                launch.real_git.display().to_string(),
            ]);
        }
    }
    let argv = [launch.scope, sandbox, gate, launch.agent].concat();
    Ok(AcpAgent::new(McpServer::Stdio(
        McpServerStdio::new("agent", &argv[0]).args(argv[1..].to_vec()),
    )))
}
