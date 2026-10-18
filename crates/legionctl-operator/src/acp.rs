//! Headless runs (note 14, S3): the agent's ACP adapter runs in the session's sandbox with the git gate. The
//! session's MCP servers (legionctl's own and the proxied ones) go to the adapter at `session/new`. Permission asks
//! are allowed, because the sandbox and the gates are the boundary; each is logged. The answer's chunks and tool calls
//! are events (`session.output`) while the run goes on, and the ACP session id goes in the job's input, so a run that
//! a restart cut off goes on with `session/load` when the agent can load sessions.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AvailableCommandInput, ClientCapabilities, CloseSessionRequest, ContentBlock, ContentChunk,
    InitializeRequest, LoadSessionRequest, McpServer, McpServerStdio, NewSessionRequest,
    PermissionOptionKind, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    SessionUpdate, TextContent,
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
    if let Err(e) = Db::open_default().and_then(|db| db.revoke_token(&id)) {
        eprintln!("operator: session {id}: cannot take its token away after its run: {e:#}");
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
    /// The first event that could not be logged: the run fails with it at its end (its audit is not whole).
    unlogged: Option<String>,
    /// The agent's last warning or error while it answered (Codex's typed session failures), and whether its turn
    /// ended with that error.
    failure: (Option<String>, bool),
}

/// The run's `initialize`, with the client capability that makes Codex's adapter send its warnings and errors as
/// typed session failures (`session_info_update` and the prompt answer's `_meta`), never as words of the answer.
fn initialize() -> InitializeRequest {
    let typed = serde_json::json!({"jetbrains": {"air": {"version": 1, "capabilities": ["sessionFailure"]}}});
    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
        ClientCapabilities::new().meta(typed.as_object().cloned().unwrap_or_default()),
    )
}

/// The typed session failure in `meta`: its severity (`warning`, `error`) and its title.
fn failure_of(meta: Option<&agent_client_protocol::schema::v1::Meta>) -> Option<(String, String)> {
    let failure = &meta?.get("jetbrains")?["air"]["sessionFailure"];
    Some((
        failure["severity"].as_str()?.to_string(),
        failure["title"].as_str()?.to_string(),
    ))
}

impl Live {
    fn record(&mut self, kind: &str, mut payload: serde_json::Value) {
        payload["session"] = self.session.clone().into();
        if let Err(e) = self.db.record("acp", kind, &payload)
            && self.unlogged.is_none()
        {
            self.unlogged = Some(format!("a {kind} event could not be logged: {e:#}"));
        }
    }

    /// Allow `request` (once when the agent offers it), and log the option chosen.
    fn allow(&mut self, request: &RequestPermissionRequest) -> RequestPermissionResponse {
        let call = &request.tool_call;
        let title = call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| call.tool_call_id.to_string());
        let kind = call.fields.kind.unwrap_or_default();
        let offered = |kind| request.options.iter().find(|o| o.kind == kind);
        // the log names the option the run chose: once, else always (the agent offers no once), else none
        let (allow, decision) = match offered(PermissionOptionKind::AllowOnce) {
            Some(o) => (Some(o), "allow_once"),
            None => match offered(PermissionOptionKind::AllowAlways) {
                Some(o) => (Some(o), "allow_always"),
                None => (None, "cancelled: no allow option offered"),
            },
        };
        self.record(
            "session.permission",
            serde_json::json!({ "tool": title, "decision": decision }),
        );
        self.permissions
            .push(format!("{title} ({kind:?}): {decision}"));
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
            SessionUpdate::SessionInfoUpdate(info) => {
                if let Some((severity, title)) = failure_of(info.meta.as_ref()) {
                    self.record(
                        "session.output",
                        serde_json::json!({ "tag": tag, severity: title }),
                    );
                    self.failure.0 = Some(title);
                }
            }
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

/// Keep the run's new ACP session id in its job. The error is one text: an ACP error keeps only the top line.
fn keep_acp(live: &Mutex<Live>, job: i64, input: &serde_json::Value) -> Result<()> {
    lock(live).db.job_input(job, input).map_err(|e| {
        anyhow::anyhow!(legionctl_core::error::message_of(
            &e.context("keeping the ACP session id")
        ))
    })
}

impl Live {
    /// The prompt's answer: its typed failure, if any, is the agent's last words; an error ends the turn as failed.
    fn ended(&mut self, meta: Option<&agent_client_protocol::schema::v1::Meta>) {
        if let Some((severity, title)) = failure_of(meta) {
            self.failure = (Some(title), severity == "error");
        }
    }

    /// A turn that ended with the agent's error, or with no words (unless a limit cut it), fails: never a run that is
    /// done with an empty answer.
    fn answered(&self, limited: bool) -> Result<()> {
        if let (Some(error), true) = &self.failure {
            anyhow::bail!("the agent stopped with an error: {error}");
        }
        if !limited && self.text.trim().is_empty() {
            match &self.failure.0 {
                Some(last) => anyhow::bail!("the agent gave no answer; its last words: {last}"),
                None => anyhow::bail!("the agent gave no answer"),
            }
        }
        Ok(())
    }

    /// The run's output; its ACP session id from the job, also when the connection was dropped.
    fn output(&self, id: String, job: i64, started: Instant) -> Result<RunOutput> {
        if let Some(why) = &self.unlogged {
            anyhow::bail!("the run's audit is not whole: {why}");
        }
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

/// The run's ACP session and whether it is new: the loaded one (`acp` of the job), else a new one, kept in the job. A
/// chat's session that the agent does not know (it started again) is logged and replaced, unless the caller asked to
/// go on in it (`load`).
async fn open_session(
    cx: &agent_client_protocol::ConnectionTo<agent_client_protocol::Agent>,
    (new, load): (NewSessionRequest, Option<(SessionId, LoadSessionRequest)>),
    input: &mut serde_json::Value,
    (live, job): (&Mutex<Live>, i64),
) -> Result<(SessionId, bool), agent_client_protocol::Error> {
    if let Some((acp, request)) = load {
        match cx.send_request(request).block_task().await {
            Ok(_) => return Ok((acp, false)),
            Err(e) if input["load"] != true => lock(live).record(
                "session.load-lost",
                serde_json::json!({"acp": acp.to_string(), "error": e.to_string()}),
            ),
            Err(e) => return Err(e),
        }
    }
    let acp = cx.send_request(new).block_task().await?.session_id;
    input["acp"] = acp.to_string().into();
    // without it a resume starts a new ACP session, without the history
    keep_acp(live, job, input)?;
    Ok((acp, true))
}

async fn drive(
    launch: Launch,
    prompt: &str,
    (job, mut input): (i64, serde_json::Value),
    (stop, budget): (tokio::sync::oneshot::Receiver<()>, Budget),
) -> Result<(RunOutput, Option<&'static str>)> {
    let started = Instant::now();
    let (id, workdir) = (launch.id.clone(), launch.workdir.clone());
    let (servers, meta) = (servers(&launch)?, bound(&launch));
    let new = NewSessionRequest::new(&workdir)
        .mcp_servers(servers.clone())
        .meta(meta.clone());
    let load = |acp| {
        LoadSessionRequest::new(acp, &workdir)
            .mcp_servers(servers)
            .meta(meta)
    };
    let (agent, failed) = adapter(launch)?;
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
        unlogged: None,
        failure: (None, false),
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
            let init = cx.send_request(initialize()).block_task().await?;
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
            let (acp, fresh) = open_session(
                &cx,
                (new, loading.map(|id| (id.clone(), load(id)))),
                &mut input,
                (&live, job),
            )
            .await?;
            // the run's model (the ui-composer's: config views.composer_models); an agent that refuses it fails the run
            if let Some(model) = input["model"].as_str() {
                let set = agent_client_protocol::UntypedMessage::new(
                    "session/set_model",
                    serde_json::json!({"sessionId": acp.to_string(), "modelId": model}),
                )?;
                cx.send_request(set).block_task().await?;
            }
            lock(&live).prompting = true;
            let text = match input["history"].as_str() {
                Some(history) if fresh => history,
                _ => prompt,
            };
            let words = vec![ContentBlock::Text(TextContent::new(text))];
            let turn = PromptRequest::new(acp.clone(), words);
            let answer = cx.send_request(turn).block_task().await?;
            lock(&live).ended(answer.meta.as_ref());
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
        done = client => { done.map_err(failed)?; None }
    };
    let live = lock(&live);
    live.answered(reached.is_some())?;
    // an answer that came in whole within one poll reached its limit too
    let reached = reached.or_else(|| budget.reached(&live.text, started.elapsed().as_secs()));
    Ok((live.output(id, job, started)?, reached))
}

/// `e` of a run's connection. An adapter that ended before it answered gives one sentence with its own error, not the
/// JSON-RPC envelope: the last line of its `stderr` file (the sandbox's, the container's, `legionctl vm`'s with the
/// guest's, or the agent's).
fn adapter_error(e: agent_client_protocol::Error, stderr: &std::path::Path) -> anyhow::Error {
    let ended = agent_client_protocol::is_incoming_transport_closed(&e)
        || e.data
            .as_ref()
            .and_then(|d| d["data"].as_str())
            .is_some_and(|why| why.starts_with("Process exited"));
    if !ended {
        return e.into();
    }
    // the adapter wrote it before it ended, so before its output closed
    let text = std::fs::read_to_string(stderr).unwrap_or_default();
    match last_error(&text) {
        Some(why) => anyhow::anyhow!("the agent stopped before it answered: {why}"),
        None => anyhow::anyhow!("the agent stopped before it answered, with no error message"),
    }
}

/// The adapter's error in its stderr `text`: the last `Error:` line of a Rust program with its causes (anyhow's
/// "Caused by:" lines), else the last line.
fn last_error(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    if let Some(at) = lines.iter().rposition(|l| l.starts_with("Error: ")) {
        let mut why = vec![lines[at].trim_start_matches("Error: ")];
        if lines.get(at + 2) == Some(&"Caused by:") {
            why.extend(
                lines[at + 3..]
                    .iter()
                    .take_while(|l| !l.is_empty())
                    .map(|l| {
                        // anyhow numbers the causes when there are several: "0: ..."
                        l.split_once(": ")
                            .filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
                            .map_or(*l, |(_, cause)| cause)
                    }),
            );
        }
        return Some(why.join(": "));
    }
    lines
        .into_iter()
        .rev()
        .find(|l| !l.is_empty())
        .map(String::from)
}

/// A bound run's `_meta` for `session/new` and `session/load` (claude-code-acp's keys; Codex's adapter has its own
/// bound home instead and ignores them): no tools of the adapter's own, and no settings files, so no plugin, hook or
/// MCP server of the user's either; legionctl's MCP server with the bound tools is the only one.
fn bound(launch: &Launch) -> Option<agent_client_protocol::schema::v1::Meta> {
    launch.tools.as_ref().map(|_| {
        serde_json::json!({
            "disableBuiltInTools": true,
            "claudeCode": {"options": {"settingSources": [], "strictMcpConfig": true}}
        })
        .as_object()
        .cloned()
        .unwrap_or_default()
    })
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
            // ACP takes the environment as a list of name and value
            stdio["env"] = server["env"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, value)| serde_json::json!({"name": name, "value": value}))
                .collect();
            serde_json::from_value::<McpServer>(stdio).with_context(|| format!("MCP server {name}"))
        })
        .collect()
}

/// The adapter's process: the session's scope and sandbox, the git gate and the agent's ACP command; and the run's
/// error from an error of its connection ([`adapter_error`]).
fn adapter(
    launch: Launch,
) -> Result<(
    AcpAgent,
    impl FnOnce(agent_client_protocol::Error) -> anyhow::Error,
)> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let stderr = std::path::Path::new(&runtime)
        .join("legionctl/sessions")
        .join(&launch.id)
        .join("adapter.err");
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
    // the adapter's stderr goes to a file of the run: the library's own reader stops with the connection, and a line
    // written just before a closed output would be lost
    let wrap = [
        "sh",
        "-c",
        "f=$1; shift; exec \"$@\" 2>\"$f\"",
        "sh",
        &stderr.to_string_lossy(),
    ]
    .map(String::from);
    let argv = [wrap.to_vec(), launch.scope, sandbox, gate, launch.agent].concat();
    let agent = AcpAgent::new(McpServer::Stdio(
        McpServerStdio::new("agent", &argv[0]).args(argv[1..].to_vec()),
    ));
    let failed = move |e| adapter_error(e, &stderr);
    Ok((agent, failed))
}
