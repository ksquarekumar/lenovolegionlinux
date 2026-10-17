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
    ACTOR_HEADER, AGENT_HEADER, API_HEADER, Acted, ApiError, Class, Code, Ctx, DispatchError, OPS,
    Op, OpInfo, Principal, api_fingerprint, op, user_token,
};
use legionctl_core::db::Db;
use serde_json::{Value, json};

use crate::engine::Operator;

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
    match Db::open_default().and_then(|db| db.token_session(token)) {
        Ok(Some(session)) => Ok(Principal::Session(session)),
        Ok(None) => Err(denied("the token is not valid (an ended session?)")),
        Err(e) => Err(denied(&format!("checking the token: {e:#}"))),
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
    let mut fields: Vec<&String> = known.into_iter().flatten().map(|(f, _)| f).collect();
    fields.sort();
    Err(ApiError {
        code: Code::InvalidInput,
        message: format!(
            "{} has no field {unknown:?}; its fields: {fields:?}",
            op.name
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

/// A sandboxed agent's call outside its own session (a `workspace` op, or a field marked `x-approval` set), and an
/// unsandboxed agent's change of what runs or is said in every session (hooks, reminders), go ahead only when the
/// user allows it; the user's own calls and machine changes (polkit asks) go ahead.
async fn approved(op: &OpInfo, principal: &Principal, input: &Value) -> Result<(), ApiError> {
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
        Principal::Agent(_) => {
            op.class == Class::Workspace
                && matches!(op.name.split('.').next(), Some("hooks" | "reminders"))
        }
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
    let what = format!(
        "{}\nAgent session {short}{dossier} asks for it.",
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
) -> (StatusCode, [(&'static str, &'static str); 1], Json<Value>) {
    let (status, body) = run(operator, name, &headers, input).await;
    (status, [(API_HEADER, api_fingerprint())], body)
}

async fn run(
    operator: Arc<Operator>,
    name: String,
    headers: &HeaderMap,
    mut input: Value,
) -> (StatusCode, Json<Value>) {
    let principal = match principal(headers) {
        Ok(p) => p,
        Err(mut e) => {
            e.op.clone_from(&name);
            return refusal(&e);
        }
    };
    let op = OPS.iter().find(|op| op.name == name);
    if let Some(op) = op {
        if let Err(mut e) = known_fields(op, &input) {
            e.op.clone_from(&name);
            return refusal(&e);
        }
        own_session(op, &principal, &mut input);
        if let Err(mut e) = approved(op, &principal, &input).await {
            e.op.clone_from(&name);
            return refusal(&e);
        }
    }
    let ctx = Ctx {
        actor: headers
            .get(ACTOR_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string(),
        principal,
    };
    let audit = op.filter(|op| op.class != Class::Read).map(|op| {
        (
            ctx.actor.clone(),
            ctx.principal.clone(),
            redacted(op, &input),
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
    let code = answer.as_ref().err().map(|e| e.code);
    count(op.map_or("unknown", |op| op.name), code, ms);
    if let Some((actor, principal, input)) = audit {
        let payload = json!({"principal": principal, "input": input, "outcome": code, "ms": ms});
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
    let (status, Json(output)) = Box::pin(run(
        operator,
        acted.op.clone(),
        headers,
        acted.input.clone(),
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
