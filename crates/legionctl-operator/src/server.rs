//! The operation registry over HTTP on the socket that systemd passes in (`legionctl-operator.socket`):
//! `GET /v1/ops` lists the operations, `POST /v1/ops/<name>` runs one with a JSON input.

use std::os::fd::FromRawFd;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use std::fmt::Write as _;

use legionctl_api::{
    ACTOR_HEADER, AGENT_HEADER, API_HEADER, Acted, ApiError, Class, Code, Ctx, DispatchError,
    FOLDER_HEADER, HARNESS_HEADER, OPS, Op, OpInfo, Principal, api_fingerprint, op, user_token,
};
use legionctl_core::db::Db;
use serde_json::{Value, json};

use crate::calls::{self, Start};
use crate::engine::Operator;
use legionctl_api::CALL_HEADER;

/// The listening socket from systemd socket activation; there is no other way to start the API.
pub fn activated_listener() -> Result<tokio::net::UnixListener> {
    let mut fds = sd_notify::listen_fds().context("reading LISTEN_FDS")?;
    ensure!(
        fds.len() == 1,
        "start legionctl-operator through legionctl-operator.socket (systemd socket activation passes one socket)"
    );
    let fd = fds.next().context("no socket")?;
    // SAFETY: systemd passed this descriptor to this process for it to own; nothing else in it uses the fd.
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) };
    listener.set_nonblocking(true)?;
    Ok(tokio::net::UnixListener::from_std(listener)?)
}

pub fn router(operator: Arc<Operator>) -> axum::Router {
    axum::Router::new()
        .route("/v1/ops", get(list))
        .route("/v1/ops/{name}", post(call))
        .route("/v1/metrics", get(metrics))
        .route("/v1/events", get(events))
        .with_state(operator)
}

/// The principal of a call's bearer token: the user token file (an agent's when its client names the agent), else a
/// session's token; no valid token is denied.
fn principal(headers: &HeaderMap) -> Result<Principal, ApiError> {
    let denied = |message: &str| ApiError {
        code: Code::Denied,
        message: message.to_string(),
        op: String::new(),
    };
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| denied("no token: a call needs `authorization: Bearer <token>` (the legionctl client sends it)"))?;
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| denied("XDG_RUNTIME_DIR is not set"))?;
    let user = user_token(std::path::Path::new(&runtime))
        .map_err(|e| denied(&format!("the user token: {e:#}")))?;
    // hashes, so the comparison takes the same time for every token
    if legionctl_core::db::token_hash(token) == legionctl_core::db::token_hash(&user) {
        return Ok(
            match headers.get(AGENT_HEADER).and_then(|v| v.to_str().ok()) {
                Some(agent) => Principal::Agent(agent.to_string()),
                None => Principal::User,
            },
        );
    }
    // a session in a scope or a container: its token is valid while it runs
    let checked = Db::open_default().and_then(|db| {
        let Some((session, inside)) = db.token_session(token)? else {
            return Ok(None);
        };
        let runs = match &inside {
            Some(inside) => legionctl_core::session::active(
                &session,
                legionctl_core::session::Inside::from_db(inside),
            )?,
            None => true,
        };
        if !runs {
            db.revoke_token(&session)?;
        }
        Ok(Some((session, runs)))
    });
    match checked {
        Ok(Some((session, true))) => Ok(Principal::Session(session)),
        Ok(Some((session, false))) => Err(denied(&format!(
            "session {session} ended, so its token is not valid now"
        ))),
        Ok(None) => Err(denied(
            "the token is not valid (it is not a legionctl token, or its session ended)",
        )),
        Err(e) => Err(denied(&format!("checking the token: {e:#}"))),
    }
}

/// The folder a call works in (its context, DESIGN-20 rule 7): the one its client sends; a sandboxed session works
/// in its own workspace (in the folder of it that the call comes from). An agent's MCP server works in the folder its
/// agent works in now, which the agent's hooks give (Claude Code starts it in the folder the agent started in): its
/// session's, or for a server with no session (Codex's) the one Codex session that works in the server's folder.
fn folder(principal: &Principal, headers: &HeaderMap) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let sent = headers
        .get(FOLDER_HEADER)
        .map(|v| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(v.as_bytes())))
        .filter(|p| p.is_absolute());
    let actor = headers.get(ACTOR_HEADER).and_then(|v| v.to_str().ok());
    match principal {
        Principal::Session(id) => {
            let own = crate::engine::dossiers()
                .and_then(|d| d.session(id))
                .ok()?
                .workdir;
            Some(match sent {
                Some(p) if p.starts_with(&own) => p,
                _ => own,
            })
        }
        Principal::Agent(id) if actor == Some("mcp") => {
            let now = match id.as_str() {
                "mcp" => sent.as_deref().and_then(|within| {
                    let mut found = Db::open_default()
                        .and_then(|db| db.agent_folders_in("codex", &within.to_string_lossy()))
                        .ok()?;
                    (found.len() == 1).then(|| found.remove(0))
                }),
                session => Db::open_default()
                    .and_then(|db| db.agent_folder(session))
                    .ok()
                    .flatten(),
            };
            now.map(std::path::PathBuf::from).or(sent)
        }
        Principal::Agent(_) | Principal::User => sent,
    }
}

/// A hook call says where its agent session (the input's `session`: the payload's, which Codex's hooks know only
/// there) works now: that session's MCP server works there too; a session's end forgets it.
fn agent_seen(
    (op, input): (&str, &Value),
    principal: &Principal,
    headers: &HeaderMap,
    folder: Option<&std::path::Path>,
) {
    let hook = headers.get(ACTOR_HEADER).and_then(|v| v.to_str().ok()) == Some("hook");
    let session = input["session"].as_str().filter(|s| !s.is_empty());
    let (Principal::Agent(_), true, Some(session)) = (principal, hook, session) else {
        return;
    };
    let harness = headers
        .get(HARNESS_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("claude");
    let done = Db::open_default().and_then(|db| match (op, folder) {
        ("dossier.release", _) => db.agent_folder_end(session),
        (_, Some(folder)) => db.agent_folder_set(session, &folder.to_string_lossy(), harness),
        (_, None) => Ok(()),
    });
    if let Err(e) = done {
        eprintln!("operator: the folder of agent session {session} is not kept: {e:#}");
    }
}

/// Only the fields the operation's input has: a misspelled field would otherwise be dropped, and the operation would
/// run with that field's default (an edit with `lines` for `content` deleted the lines it was to change).
pub(crate) fn known_fields(op: &OpInfo, input: &Value) -> Result<(), ApiError> {
    let schema = (op.input)();
    let known = schema["properties"].as_object();
    let unknown: Vec<&String> = input
        .as_object()
        .into_iter()
        .flatten()
        .map(|(field, _)| field)
        .filter(|field| !known.is_some_and(|k| k.contains_key(*field)))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    // a folder of its own conflicts with the call's one context (DESIGN-20 rule 7)
    if let Some(field) = unknown
        .iter()
        .find(|f| ["repo", "project", "folder"].contains(&f.as_str()))
    {
        return Err(ApiError {
            code: Code::InvalidInput,
            message: format!(
                "{} works in the folder of the call and takes no {field}: run it in that folder, or give \
                 legionctl -C <folder>",
                op.name
            ),
            op: String::new(),
        });
    }
    let mut fields: Vec<&String> = known.into_iter().flatten().map(|(f, _)| f).collect();
    fields.sort();
    Err(ApiError {
        code: Code::InvalidInput,
        message: format!(
            "{} has no field {}. Its fields are: {}.",
            op.name,
            unknown
                .iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            fields
                .iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        op: String::new(),
    })
}

/// For a session's call, its own id in every session field of the input: a sandbox cannot act as another session.
pub(crate) fn own_session(op: &OpInfo, principal: &Principal, input: &mut Value) {
    let (Principal::Session(id), Some(map)) = (principal, input.as_object_mut()) else {
        return;
    };
    let schema = (op.input)();
    for (field, prop) in schema["properties"].as_object().into_iter().flatten() {
        let is_session = prop["format"] == "session"
            || prop["anyOf"]
                .as_array()
                .is_some_and(|a| a.iter().any(|s| s["format"] == "session"));
        if is_session {
            map.insert(field.clone(), Value::String(id.clone()));
        }
    }
}

/// An agent's call of a public operation outside its toolsets asks the user for the toolset (DESIGN/22 rule 2); the
/// user's own calls, legionctl's internal ones and the hooks of an unsandboxed agent (legionctl's own lifecycle steps
/// for it, such as the dossier release at its end) have no scope. True: the user was asked and allowed it.
async fn in_scope(
    op: &OpInfo,
    (principal, folder): (&Principal, Option<&std::path::Path>),
    actor: Option<&str>,
    input: &Value,
) -> Result<bool, ApiError> {
    let hook = matches!(principal, Principal::Agent(_)) && actor == Some("hook");
    match principal.agent() {
        Some(id) if !op.internal() && !hook => {
            crate::scope::forbidden(op.name, folder)?;
            crate::scope::check(id, op.name, &op.sentence(&redacted(op, input)), folder).await
        }
        _ => Ok(false),
    }
}

/// Whether an agent's call changes a setting of the powerful or machine class (the settings operations, and the
/// operations that change hooks and reminders, which are such settings): the user is asked each time.
fn powerful_setting(op: &OpInfo, input: &Value, folder: Option<&std::path::Path>) -> bool {
    use legionctl_core::settings::Class as Kind;
    // the class on this computer (with its overrides); a setting that cannot be read asks
    let resolved = crate::engine::config(folder).ok();
    let class = |key: &str| {
        resolved
            .as_ref()
            .and_then(|r| r.declared(key))
            .map_or(Kind::Powerful, |(_, class)| class)
    };
    match op.name {
        "config.set" | "config.unset" | "config.add" | "config.remove" => {
            input["key"].as_str().is_none_or(|k| class(k) != Kind::Safe)
        }
        "config.revert" => input["id"]
            .as_i64()
            .and_then(|id| Db::open_default().ok()?.settings_change(id).ok()?)
            .is_none_or(|c| class(&c.key) != Kind::Safe),
        _ => {
            op.class == Class::Workspace
                && matches!(op.name.split('.').next(), Some("hooks" | "reminders"))
        }
    }
}

/// A sandboxed agent's call outside its own session (a `workspace` op, or a field marked `x-approval` set), and an
/// unsandboxed agent's change of what runs or is said in every session (hooks, reminders), go ahead only when the
/// user allows it; the user's own calls and machine changes (polkit asks) go ahead.
async fn approved(
    op: &OpInfo,
    (principal, folder): (&Principal, Option<&std::path::Path>),
    input: &Value,
) -> Result<(), ApiError> {
    let asks = match principal {
        Principal::User => false,
        Principal::Session(_) => {
            let schema = (op.input)();
            op.class == Class::Workspace
                || schema["properties"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .any(|(field, prop)| prop["x-approval"] == true && input[field] == true)
        }
        // it is the user's own process otherwise: the gate is on what reaches every later session
        Principal::Agent(_) => powerful_setting(op, input, folder),
    };
    let Some(session) = principal.agent().filter(|_| asks) else {
        return Ok(());
    };
    let denied = |message: String| ApiError {
        code: Code::Denied,
        message,
        op: String::new(),
    };
    let short = &session[..session.len().min(8)];
    let dossier = crate::engine::dossiers()
        .and_then(|d| d.active(session))
        .ok()
        .flatten()
        .map(|d| format!(", dossier {d}"))
        .unwrap_or_default();
    let place = folder
        .map(|f| format!(" in {}", f.display()))
        .unwrap_or_default();
    let what = format!(
        "{}\nAgent session {short}{dossier} asks for it{place}.",
        op.sentence(&redacted(op, input))
    );
    let asked = crate::desktop::ask("legionctl: an agent asks for your approval", &what).await;
    match asked {
        Ok(true) => Ok(()),
        Ok(false) => Err(denied(format!(
            "{} changes more than this session, and the user did not allow it",
            op.name
        ))),
        Err(e) => Err(denied(format!(
            "{} changes more than this session and needs the user's approval, but nobody could be asked: {e:#}",
            op.name
        ))),
    }
}

async fn list(headers: HeaderMap) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    principal(&headers).map_err(|e| refusal(&e))?;
    Ok(Json(Value::Array(
        OPS.iter()
            .map(|op| json!({"name": op.name, "help": op.help, "class": op.class, "input": (op.input)()}))
            .collect(),
    )))
}

fn refusal(error: &ApiError) -> (StatusCode, Json<Value>) {
    (
        StatusCode::from_u16(error.code.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({"error": error})),
    )
}

/// The event log as a stream (Durable Streams semantics): `GET /v1/events?offset=<id>` sends every event after that
/// id, then each new one as it comes, as server-sent events whose id is the event id; `offset=now` starts at the
/// newest, and a reconnect with `Last-Event-ID` goes on where it stopped. Not for a sandbox: the log holds every
/// session's calls.
async fn events(
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Result<
    axum::response::sse::Sse<
        impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    (StatusCode, Json<Value>),
> {
    if matches!(
        principal(&headers).map_err(|e| refusal(&e))?,
        Principal::Session(_)
    ) {
        return Err(refusal(&ApiError {
            code: Code::Denied,
            message: "the event stream is for the user; a session reads its events with the events operation".into(),
            op: String::new(),
        }));
    }
    let offset = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or_else(|| {
            uri.query()?
                .split('&')
                .find_map(|kv| kv.strip_prefix("offset="))
                .map(String::from)
        });
    let start = match offset.as_deref() {
        Some("now") => tokio::task::spawn_blocking(|| Db::open_default()?.last_event_id())
            .await
            .map_err(|e| refusal(&internal(&e.to_string())))?
            .map_err(|e| refusal(&internal(&format!("{e:#}"))))?,
        Some(id) => id.parse().map_err(|_| {
            refusal(&ApiError {
                code: Code::InvalidInput,
                message: format!("offset {id} is not an event id or now"),
                op: String::new(),
            })
        })?,
        None => 0,
    };
    let stream = futures_util::stream::unfold(
        (start, std::collections::VecDeque::new(), false),
        |(mut last, mut pending, ended): (
            i64,
            std::collections::VecDeque<legionctl_core::db::EventRow>,
            bool,
        )| async move {
            if ended {
                return None;
            }
            loop {
                if let Some(row) = pending.pop_front() {
                    last = row.id;
                    let row: legionctl_api::EventRow = legionctl_api::IntoWire::into_wire(row);
                    let event = axum::response::sse::Event::default()
                        .id(row.id.to_string())
                        .data(serde_json::to_string(&row).unwrap_or_default());
                    return Some((Ok(event), (last, pending, false)));
                }
                // shortcut: a new event reaches the stream within 250 ms (it reads the log), wake it from Db::record if that is slow
                let read = tokio::task::spawn_blocking(move || {
                    Db::open_default()?.events_page(
                        &legionctl_core::db::EventFilter::default(),
                        last,
                        500,
                    )
                })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|page| page);
                // a log that cannot be read ends the stream with the reason, never silence that looks like no events
                let rows = match read {
                    Ok((rows, _)) => rows,
                    Err(e) => {
                        let event = axum::response::sse::Event::default()
                            .event("error")
                            .data(serde_json::json!({"error": format!("{e:#}")}).to_string());
                        return Some((Ok(event), (last, pending, true)));
                    }
                };
                if rows.is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                pending.extend(rows);
            }
        },
    );
    Ok(axum::response::sse::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}

fn internal(message: &str) -> ApiError {
    ApiError {
        code: Code::Internal,
        message: message.to_string(),
        op: String::new(),
    }
}

/// One telemetry sample in `OpenMetrics` text, for Prometheus or Grafana.
async fn metrics(
    State(operator): State<Arc<Operator>>,
    headers: HeaderMap,
) -> (StatusCode, String) {
    if let Err(e) = principal(&headers) {
        return (StatusCode::FORBIDDEN, format!("{}\n", e.message));
    }
    let sample =
        tokio::task::spawn_blocking(move || operator.sample(std::time::Duration::from_millis(250)))
            .await;
    match sample.map_err(anyhow::Error::from).and_then(|s| s) {
        Ok(s) => (
            StatusCode::OK,
            op_metrics() + &legionctl_machine::telemetry::openmetrics(&s),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("sampling failed: {e:#}\n"),
        ),
    }
}

async fn call(
    State(operator): State<Arc<Operator>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> (StatusCode, HeaderMap, Json<Value>) {
    let mut reply = HeaderMap::new();
    reply.insert(
        API_HEADER,
        axum::http::HeaderValue::from_static(api_fingerprint()),
    );
    let (status, body) = Box::pin(run(operator, name, &headers, input, &mut reply)).await;
    (status, reply, body)
}

/// Run one call; a call the record keeps puts its call id into the `reply` headers.
async fn run(
    operator: Arc<Operator>,
    name: String,
    headers: &HeaderMap,
    mut input: Value,
    reply: &mut HeaderMap,
) -> (StatusCode, Json<Value>) {
    let principal = match principal(headers) {
        Ok(p) => p,
        Err(mut e) => {
            e.op.clone_from(&name);
            return refusal(&e);
        }
    };
    let op = OPS.iter().find(|op| op.name == name);
    let folder = folder(&principal, headers);
    agent_seen((&name, &input), &principal, headers, folder.as_deref());
    let mut recording: Option<calls::OpCall> = None;
    if let Some(op) = op {
        if let Err(mut e) = known_fields(op, &input) {
            e.op.clone_from(&name);
            return refusal(&e);
        }
        own_session(op, &principal, &mut input);
        match admit(op, (&principal, folder.as_deref()), headers, &input, reply).await {
            Ok(call) => recording = call,
            Err(answer) => return answer,
        }
    }
    let ctx = Ctx {
        actor: headers
            .get(ACTOR_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string(),
        folder,
        principal,
    };
    // the call record is the record of its own begin and end
    let audit = op
        .filter(|op| op.class != Class::Read && !op.name.starts_with("internal.calls."))
        .map(|op| {
            (
                ctx.actor.clone(),
                ctx.principal.clone(),
                (ctx.folder.clone(), redacted(op, &input)),
            )
        });
    let started = std::time::Instant::now();
    // engine methods read files and run programs; keep them off the async threads
    let call = name.clone();
    let engine = operator.clone();
    let result =
        tokio::task::spawn_blocking(move || legionctl_api::dispatch(&*engine, &ctx, &call, input))
            .await;
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let answer = match result {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => {
            let mut error = e.api_error(&name);
            // after an upgrade the running operator is the old build until it restarts
            if matches!(e, DispatchError::Unknown(_))
                && headers
                    .get(API_HEADER)
                    .is_some_and(|v| v != api_fingerprint())
            {
                error.code = Code::VersionMismatch;
                error.message.push_str(
                    "; this operator is a different build than the client: restart it \
                     (systemctl --user restart legionctl-operator)",
                );
            }
            Err(error)
        }
        Err(e) => Err(ApiError {
            code: Code::Internal,
            message: format!("operation crashed: {e}"),
            op: name.clone(),
        }),
    };
    if let Some(call) = &recording {
        calls::end_op(call, &answer);
    }
    let code = answer.as_ref().err().map(|e| e.code);
    count(op.map_or("unknown", |op| op.name), code, ms);
    if let Some((actor, principal, (folder, input))) = audit {
        let payload = json!({"principal": principal, "folder": folder, "input": input, "outcome": code, "ms": ms});
        if let Err(e) = Db::open_default().and_then(|db| db.record(&actor, &name, &payload)) {
            eprintln!("operator: audit of {name} not recorded: {e:#}");
        }
    }
    match answer {
        Ok(output) if name == op::ResourcesAct::NAME => routed(operator, headers, output).await,
        Ok(output) => (StatusCode::OK, Json(output)),
        Err(error) => refusal(&error),
    }
}

/// What runs before an operation: its start in the call record (a retry of an ended call gets the recorded answer),
/// then the toolset scope and the approval of its class. `Ok`: run it (and end its row, when the record keeps it);
/// `Err`: the answer to give instead.
async fn admit(
    op: &OpInfo,
    (principal, folder): (&Principal, Option<&std::path::Path>),
    headers: &HeaderMap,
    input: &Value,
    reply: &mut HeaderMap,
) -> Result<Option<calls::OpCall>, (StatusCode, Json<Value>)> {
    let named = |mut e: ApiError| {
        e.op = op.name.to_string();
        e
    };
    let given = headers.get(CALL_HEADER).and_then(|v| v.to_str().ok());
    let recording = match calls::begin_op(op, (principal, folder), given, input) {
        Ok(None) => None,
        Ok(Some(Start::Answer(id, recorded))) => {
            tag(reply, &id);
            return Err(match recorded {
                Ok(output) => (StatusCode::OK, Json(output)),
                Err(e) => refusal(&e),
            });
        }
        Ok(Some(Start::Run(call))) => {
            tag(reply, &call.id);
            Some(call)
        }
        Err(e) => return Err(refusal(&named(e))),
    };
    let refused = |e: ApiError| {
        let e = named(e);
        if let Some(call) = &recording {
            calls::end_op(call, &Err(e.clone()));
        }
        refusal(&e)
    };
    // the user who just allowed the toolset for this call is not asked again for the same call
    let actor = headers.get(ACTOR_HEADER).and_then(|v| v.to_str().ok());
    let asked = in_scope(op, (principal, folder), actor, input)
        .await
        .map_err(refused)?;
    if !asked {
        approved(op, (principal, folder), input)
            .await
            .map_err(refused)?;
    }
    Ok(recording)
}

/// The call id of a recorded call, as the reply's header.
fn tag(reply: &mut HeaderMap, id: &str) {
    if let Ok(value) = axum::http::HeaderValue::from_str(id) {
        reply.insert(CALL_HEADER, value);
    }
}

/// `resources.act` found the route only (note 22): the routed operation runs here as a call of its own, with its
/// input check, its approval and its event, and its output goes back inside the route.
async fn routed(
    operator: Arc<Operator>,
    headers: &HeaderMap,
    route: Value,
) -> (StatusCode, Json<Value>) {
    let mut acted: Acted = match serde_json::from_value(route) {
        Ok(acted) => acted,
        Err(e) => return refusal(&internal(&format!("the route: {e}"))),
    };
    if !acted.run {
        return (StatusCode::OK, Json(json!(acted)));
    }
    // the routed operation is a call of its own: it does not take the route's call id
    let mut inner = headers.clone();
    inner.remove(CALL_HEADER);
    let (status, Json(output)) = Box::pin(run(
        operator,
        acted.op.clone(),
        &inner,
        acted.input.clone(),
        &mut HeaderMap::new(),
    ))
    .await;
    if status != StatusCode::OK {
        return (status, Json(output));
    }
    acted.output = Some(output);
    (StatusCode::OK, Json(json!(acted)))
}

/// The input as the audit keeps it: fields whose schema says `x-secret` become `<secret>`, and strings over 512
/// bytes become their size.
fn redacted(op: &OpInfo, input: &Value) -> Value {
    fn shorten(value: &Value) -> Value {
        match value {
            Value::String(s) if s.len() > 512 => Value::String(format!("<{} bytes>", s.len())),
            Value::Array(items) => items.iter().map(shorten).collect(),
            Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), shorten(v))).collect(),
            other => other.clone(),
        }
    }
    let schema = (op.input)();
    let mut out = shorten(input);
    if let (Some(map), Some(props)) = (out.as_object_mut(), schema["properties"].as_object()) {
        for (field, prop) in props {
            if prop["x-secret"] == true && map.contains_key(field) {
                map.insert(field.clone(), Value::String("<secret>".into()));
            }
        }
    }
    out
}

/// Calls, failures by code and total time of each operation since the operator started.
#[derive(Default)]
struct Counts {
    calls: u64,
    ms: u64,
    errors: std::collections::BTreeMap<&'static str, u64>,
}

static COUNTS: std::sync::Mutex<std::collections::BTreeMap<&'static str, Counts>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn count(op: &'static str, code: Option<Code>, ms: u64) {
    let mut counts = COUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let c = counts.entry(op).or_default();
    c.calls += 1;
    c.ms += ms;
    if let Some(code) = code {
        let name = code.as_str();
        *c.errors.entry(name).or_default() += 1;
    }
}

/// The counters in `OpenMetrics` text.
fn op_metrics() -> String {
    let counts = COUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut out = String::from(
        "# TYPE legionctl_op_calls counter\n# TYPE legionctl_op_errors counter\n# TYPE legionctl_op_ms counter\n",
    );
    for (op, c) in counts.iter() {
        let _ = writeln!(out, "legionctl_op_calls_total{{op=\"{op}\"}} {}", c.calls);
        let _ = writeln!(out, "legionctl_op_ms_total{{op=\"{op}\"}} {}", c.ms);
        for (code, n) in &c.errors {
            let _ = writeln!(
                out,
                "legionctl_op_errors_total{{op=\"{op}\",code=\"{code}\"}} {n}"
            );
        }
    }
    out
}
