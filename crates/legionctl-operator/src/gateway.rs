//! The model gateway (note 19, G1): the Anthropic and `OpenAI` APIs on 127.0.0.1, in front of named accounts. A plain
//! model goes to the first account of its pool entry (or `*`) that has quota and budget left; `account/model` goes to
//! exactly that account. Exact responses replay from the cache (streams too); every call is a usage row.
//!
//! An account without a keyring key forwards the caller's own credentials (Claude Code and Codex logins), so it
//! exposes nothing. An account with a key serves only callers that send the gateway token
//! (`x-legionctl-token`, from `~/.local/state/legionctl/gateway-token`): any local user can reach a local port.

use std::os::unix::fs::OpenOptionsExt;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use blake2::digest::consts::U16;
use blake2::{Blake2b, Digest};
use futures_util::StreamExt;
use legionctl_core::cache::{Cache, Entry};
use legionctl_core::config::{Dirs, GatewayAccount, GatewayConfig, Price, Tokens, resolve};
use legionctl_core::db::{Db, GatewayUsage};
use serde_json::{Value, json};

use crate::translate::{self, Direction};

/// Who calls the gateway: a session (its process is in the session's scope, from the socket's peer), else the host.
#[derive(Clone, Debug)]
struct Caller {
    who: String,
}

impl
    axum::extract::connect_info::Connected<
        axum::serve::IncomingStream<'_, tokio::net::UnixListener>,
    > for Caller
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Caller {
        let session = stream
            .io()
            .peer_cred()
            .ok()
            .and_then(|c| c.pid())
            .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok())
            .and_then(|cgroup| session_of(&cgroup));
        Caller {
            who: session.map_or_else(|| "host".into(), |id| format!("session:{id}")),
        }
    }
}

/// The session of a process in `cgroup`: its scope on the host or in a VM (`legionctl-session-<id>.scope`), else its
/// container (Docker's `docker-<container>.scope` or `/docker/<container>`), whose name legionctl gave it.
fn session_of(cgroup: &str) -> Option<String> {
    let parts: Vec<&str> = cgroup.split(['/', '\n']).collect();
    if let Some(id) = parts.iter().find_map(|part| {
        part.strip_prefix("legionctl-session-")?
            .strip_suffix(".scope")
    }) {
        return Some(id.to_string());
    }
    let container = parts.windows(2).find_map(|w| match w {
        [_, part] if part.starts_with("docker-") => {
            part.strip_prefix("docker-")?.strip_suffix(".scope")
        }
        ["docker", part] => Some(*part),
        _ => None,
    })?;
    container_session(container)
}

/// The session of container `id`: from its name (`legionctl-session-<session>`), which the agent in it cannot change;
/// kept, since each connection asks.
fn container_session(id: &str) -> Option<String> {
    static NAMES: std::sync::Mutex<std::collections::BTreeMap<String, Option<String>>> =
        std::sync::Mutex::new(std::collections::BTreeMap::new());
    let mut names = NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(known) = names.get(id) {
        return known.clone();
    }
    let out = std::process::Command::new("docker")
        .args(["inspect", "-f", "{{.Name}}", id])
        .output()
        .ok()?;
    // a container that docker does not know (gone) is no session; not kept, a new one may take the id
    if !out.status.success() {
        return None;
    }
    let session = String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_start_matches('/')
        .strip_prefix("legionctl-session-")
        .map(String::from);
    names.insert(id.to_string(), session.clone());
    session
}

impl
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>>
    for Caller
{
    fn connect_info(_: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Caller {
        Caller { who: "host".into() }
    }
}

/// The finding of a gateway cache that cannot be used (user decision Q3).
const GATEWAY_CACHE: &str = "the gateway cache fails, so its calls go upstream";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Api {
    Messages,
    Chat,
    Responses,
}

impl Api {
    fn provider(self) -> &'static str {
        match self {
            Api::Messages => "anthropic",
            Api::Chat | Api::Responses => "openai",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Api::Messages => "/v1/messages",
            Api::Chat => "/v1/chat/completions",
            Api::Responses => "/v1/responses",
        }
    }

    /// The direction to `account`'s API: `None` when it speaks the caller's.
    ///
    /// # Errors
    /// The two APIs do not translate, or the account forwards the caller's own key, which is for the other provider.
    fn translation(
        self,
        name: &str,
        account: &GatewayAccount,
    ) -> Result<Option<Direction>, String> {
        if account.provider == self.provider() {
            return Ok(None);
        }
        let direction = match self {
            Api::Messages => Direction::MessagesToChat,
            Api::Chat => Direction::ChatToMessages,
            Api::Responses => {
                return Err(format!(
                    "account {name} speaks the {} API; translating the OpenAI Responses API is not built",
                    account.provider
                ));
            }
        };
        if !account.keyring {
            return Err(format!(
                "account {name} speaks the {} API and has no keyring key; the caller's own key is for the other provider",
                account.provider
            ));
        }
        Ok(Some(direction))
    }

    /// The provider's API for a translated call.
    fn of(direction: Direction) -> Self {
        match direction {
            Direction::MessagesToChat => Api::Chat,
            Direction::ChatToMessages => Api::Messages,
        }
    }
}

#[derive(Clone)]
struct Gateway {
    http: reqwest::Client,
    token: String,
}

/// The token a caller sends for accounts with keyring keys; made once, readable only by the user. A file without at
/// least 32 hex characters (empty, cut short) gets a new token: any local user could send a short one.
fn token() -> Result<String> {
    let path = legionctl_core::db::default_path().with_file_name("gateway-token");
    // a token file that cannot be read (not missing, not cut short) fails the gateway: a new token would lock out
    // every client that has the old one, with no reason
    if let Some(token) = legionctl_machine::sysroot::read_file(&path)?.map(|t| t.trim().to_string())
        && token.len() >= 32
        && token.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Ok(token);
    }
    let token = legionctl_api::random_token()?;
    std::fs::create_dir_all(path.parent().context("no state folder")?)?;
    // 0600 from the first byte, and whole or absent for a reader: never readable by others, never half written
    let part = path.with_extension("part");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&part)?;
    std::io::Write::write_all(&mut file, token.as_bytes())?;
    std::fs::rename(&part, &path)?;
    Ok(token)
}

/// The gateway's token and listeners, made before the operator answers its first call: a session that a call starts
/// finds the gateway's socket there (a connection waits in the queue until the gateway serves it, none is refused),
/// and a caller that finds the port open finds the token file too.
pub struct Bound {
    token: String,
    tcp: tokio::net::TcpListener,
    local: tokio::net::UnixListener,
}

/// Make the token, then bind 127.0.0.1:`port` and the gateway's socket in the runtime folder.
///
/// # Errors
/// The token cannot be made, the port is taken, or the socket cannot be bound.
pub async fn bind(port: u16) -> Result<Bound> {
    let token = token()?;
    let tcp = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("gateway port {port}"))?;
    // sessions reach the gateway through this socket (their 127.0.0.1 is their own)
    let socket = std::path::PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?,
    )
    .join("legionctl")
    .join(legionctl_core::session::GATEWAY_SOCKET);
    let _ = std::fs::remove_file(&socket);
    let local = tokio::net::UnixListener::bind(&socket)
        .with_context(|| format!("binding {}", socket.display()))?;
    eprintln!(
        "operator: model gateway on 127.0.0.1:{port} and {}",
        socket.display()
    );
    Ok(Bound { token, tcp, local })
}

/// Serve the gateway on its bound listeners until the operator stops.
///
/// # Errors
/// A listener fails.
pub async fn serve(bound: Bound, ready: crate::Ready) -> Result<()> {
    let Bound { token, tcp, local } = bound;
    let gateway = Gateway {
        http: reqwest::Client::new(),
        token,
    };
    let app = axum::Router::new()
        .route(
            "/v1/messages",
            post(|s, c, h, b| call(s, c, Api::Messages, h, b)),
        )
        .route(
            "/v1/chat/completions",
            post(|s, c, h, b| call(s, c, Api::Chat, h, b)),
        )
        .route(
            "/v1/responses",
            post(|s, c, h, b| call(s, c, Api::Responses, h, b)),
        )
        .fallback(tunnel)
        .with_state(gateway);
    ready.set();
    tokio::try_join!(
        axum::serve(
            tcp,
            app.clone()
                .layer(axum::middleware::from_fn(local_only))
                .into_make_service_with_connect_info::<Caller>()
        )
        .into_future(),
        axum::serve(local, app.into_make_service_with_connect_info::<Caller>()).into_future()
    )?;
    Ok(())
}

/// A session's HTTPS tunnel (`CONNECT host:port`, its `HTTPS_PROXY`; DESIGN-12): open only to what its grant allows
/// ([`legionctl_core::runnables::Connections::refusal`]), to the addresses checked. Each tunnel is an event with its
/// host and bytes (`gateway.tunnel`), each refusal one with the reason (`gateway.refused`): the gateway does not see
/// inside TLS. Every other method or path is not the gateway's.
async fn tunnel(
    axum::extract::ConnectInfo(caller): axum::extract::ConnectInfo<Caller>,
    request: axum::extract::Request,
) -> Response {
    if request.method() != axum::http::Method::CONNECT {
        return refuse(StatusCode::NOT_FOUND, "the gateway has no such path");
    }
    let Some((host, port)) = request
        .uri()
        .authority()
        .map(|a| (a.host().to_ascii_lowercase(), a.port_u16().unwrap_or(443)))
    else {
        return refuse(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
    };
    let event = json!({ "who": caller.who, "host": host, "port": port });
    let mut upstream = match open(&caller.who, &host, port).await {
        Ok(stream) => stream,
        Err((status, why)) => {
            log("gateway.refused", json!({ "why": why }), &event);
            return refuse(status, &format!("{host}:{port}: {why}"));
        }
    };
    tokio::spawn(async move {
        let started = Instant::now();
        let bytes = match hyper::upgrade::on(request).await {
            Ok(upgraded) => tokio::io::copy_bidirectional(
                &mut hyper_util::rt::TokioIo::new(upgraded),
                &mut upstream,
            )
            .await
            .unwrap_or_default(),
            Err(_) => (0, 0),
        };
        log(
            "gateway.tunnel",
            json!({ "up": bytes.0, "down": bytes.1, "ms": started.elapsed().as_millis() }),
            &event,
        );
    });
    StatusCode::OK.into_response()
}

/// The upstream of a tunnel for `who` to `host`:`port`, when its grant allows it; else the status and the reason.
async fn open(
    who: &str,
    host: &str,
    port: u16,
) -> Result<tokio::net::TcpStream, (StatusCode, String)> {
    let Some(session) = who.strip_prefix("session:") else {
        return Err((
            StatusCode::FORBIDDEN,
            "the tunnel is for legionctl sessions; this program is not in one".into(),
        ));
    };
    let connections = Db::open_default()
        .and_then(|db| legionctl_core::runnables::connections_of(&db, session))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?
        .ok_or((
            StatusCode::FORBIDDEN,
            format!("session {session} has no granted connections"),
        ))?;
    if let Some(why) = connections.refusal(host, port) {
        return Err((StatusCode::FORBIDDEN, why));
    }
    let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("cannot find the host: {e}"),
            )
        })?
        .collect();
    // web read: the addresses checked are the ones connected to (a name that turns local later gets no tunnel)
    if !connections.declares(host, port)
        && !addresses
            .iter()
            .all(|a| legionctl_core::runnables::public(a.ip()))
    {
        return Err((
            StatusCode::FORBIDDEN,
            "web read reaches public hosts only; this one is local or private".into(),
        ));
    }
    tokio::net::TcpStream::connect(&addresses[..])
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("cannot connect: {e}")))
}

/// An event of the gateway: `fields` with the caller and the target of `event`.
fn log(kind: &str, mut fields: Value, event: &Value) {
    if let (Some(f), Some(e)) = (fields.as_object_mut(), event.as_object()) {
        f.extend(e.clone());
    }
    if let Err(e) = Db::open_default().and_then(|db| db.record("gateway", kind, &fields)) {
        eprintln!("operator: {kind} not recorded: {e:#}");
    }
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(json!({ "error": { "type": "legionctl_gateway", "message": message } })),
    )
        .into_response()
}

/// The tokens of a response: JSON, or every `data:` event of a stream (Anthropic's `message_start` and
/// `message_delta`, the usage chunk of `OpenAI`). Anthropic counts cache reads and writes beside `input_tokens`;
/// `OpenAI` counts its cache reads (`*_tokens_details.cached_tokens`) inside `prompt_tokens`/`input_tokens`.
fn usage(body: &[u8]) -> Tokens {
    let text = String::from_utf8_lossy(body);
    let objects: Vec<Value> = match serde_json::from_str::<Value>(&text) {
        Ok(v) => vec![v],
        Err(_) => text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect(),
    };
    let (mut input, mut cached_inside, mut tokens) = (0, 0, Tokens::default());
    for object in &objects {
        for usage in [
            &object["usage"],
            &object["message"]["usage"],
            &object["response"]["usage"],
        ] {
            let get = |keys: &[&str]| {
                keys.iter()
                    .filter_map(|k| usage.pointer(k).and_then(Value::as_i64))
                    .max()
                    .unwrap_or(0)
            };
            input = input.max(get(&["/input_tokens", "/prompt_tokens"]));
            cached_inside = cached_inside.max(get(&[
                "/prompt_tokens_details/cached_tokens",
                "/input_tokens_details/cached_tokens",
            ]));
            tokens.cache_read = tokens.cache_read.max(get(&["/cache_read_input_tokens"]));
            tokens.cache_write = tokens
                .cache_write
                .max(get(&["/cache_creation_input_tokens"]));
            tokens.output = tokens
                .output
                .max(get(&["/output_tokens", "/completion_tokens"]));
        }
    }
    tokens.input = input - cached_inside;
    tokens.cache_read += cached_inside;
    tokens
}

/// The usage row of a model call, and its row in the call record (DESIGN/22 rule 3: one record, the gateway's own
/// cache stays apart).
fn record(row: &GatewayUsage) {
    let outcome = match (row.cached, row.status) {
        (true, _) => "cached",
        (false, 200..=299) => "ok",
        _ => "error",
    };
    let written = Db::open_default().and_then(|db| {
        db.gateway_usage_add(row)?;
        let ended = legionctl_core::calls::now_ms();
        let call = legionctl_core::calls::Call {
            call: &legionctl_core::calls::new_id()?,
            principal: "gateway",
            session: "",
            kind: "model",
            server: &row.account,
            name: &row.model,
            input_hash: "",
            scope: "",
            folder: "",
        };
        db.call_add(&call, (ended - row.ms, ended), outcome)
    });
    if let Err(e) = written {
        eprintln!("operator: gateway usage not recorded: {e:#}");
    }
}

/// An account that may serve a call: its name, the model there, and the translation to its API.
type Candidate = (String, String, Option<Direction>);

/// The accounts that may serve `model` for `api`: the named one, or the pool entry (then `*`), with the translation
/// each needs.
fn candidates(config: &GatewayConfig, api: Api, model: &str) -> Result<Vec<Candidate>, String> {
    if let Some((account, bare)) = model.split_once('/') {
        let a = config
            .accounts
            .get(account)
            .ok_or(format!("no gateway account {account}"))?;
        let direction = api.translation(account, a)?;
        return Ok(vec![(account.into(), bare.into(), direction)]);
    }
    let pool = config
        .pool
        .get(model)
        .or_else(|| config.pool.get("*"))
        .ok_or(format!("no pool for {model}"))?;
    let mut reasons = Vec::new();
    let mut accounts = Vec::new();
    for name in pool {
        // a pool entry that names no account is a config error, never a silent gap
        let account = config
            .accounts
            .get(name)
            .ok_or(format!("pool {model}: no gateway account {name}"))?;
        match api.translation(name, account) {
            Ok(direction) => accounts.push((name.clone(), model.to_string(), direction)),
            Err(why) => reasons.push(why),
        }
    }
    if accounts.is_empty() {
        return Err(format!(
            "the pool for {model} has no account for the {} API: {}",
            api.provider(),
            reasons.join("; ")
        ));
    }
    Ok(accounts)
}

pub(crate) async fn key(account: &str) -> Result<String> {
    let out = tokio::process::Command::new("secret-tool")
        .args(["lookup", "legionctl-account", account])
        .output()
        .await
        .context("secret-tool")?;
    anyhow::ensure!(
        out.status.success() && !out.stdout.is_empty(),
        "no keyring entry legionctl-account {account}"
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn send(
    gw: &Gateway,
    api: Api,
    account: &GatewayAccount,
    name: &str,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> Result<reqwest::Response> {
    let mut request = gw
        .http
        .post(format!(
            "{}{}",
            account.base_url.trim_end_matches('/'),
            api.path()
        ))
        .header("content-type", "application/json")
        .body(body);
    for header in [
        "anthropic-version",
        "anthropic-beta",
        "openai-beta",
        "accept",
    ] {
        if let Some(value) = headers.get(header) {
            request = request.header(header, value);
        }
    }
    if api == Api::Messages && !headers.contains_key("anthropic-version") {
        // an OpenAI caller sends none; Anthropic needs one
        request = request.header("anthropic-version", "2023-06-01");
    }
    if account.keyring {
        let key = key(name).await?;
        request = match api {
            Api::Messages => request.header("x-api-key", key),
            Api::Chat | Api::Responses => request.bearer_auth(key),
        };
    } else {
        for header in ["x-api-key", "authorization"] {
            if let Some(value) = headers.get(header) {
                request = request.header(header, value);
            }
        }
    }
    Ok(request.send().await?)
}

/// Why `name` cannot serve `bare` now: its key needs the gateway token, or a money budget is used. `Err`: the budget
/// cannot be read, so the call cannot be held to it (19-gateway: never too little).
fn blocked(
    config: &GatewayConfig,
    account: &GatewayAccount,
    name: &str,
    bare: &str,
    headers: &HeaderMap,
    token: &str,
) -> Result<Option<String>, String> {
    if account.keyring
        && headers
            .get("x-legionctl-token")
            .and_then(|v| v.to_str().ok())
            != Some(token)
    {
        return Ok(Some(format!(
            "{name}: this account's key needs the gateway token (x-legionctl-token)"
        )));
    }
    let Some(budget) = config.budgets.get(&format!("{name}/{bare}")) else {
        return Ok(None);
    };
    let unreadable =
        |e: anyhow::Error| format!("the budget for {name}/{bare} cannot be read: {e:#}");
    let db = Db::open_default().map_err(unreadable)?;
    for (period, limit, seconds) in [
        ("day", budget.day_usd, 86_400),
        ("30 days", budget.month_usd, 30 * 86_400),
    ] {
        let Some(limit) = limit else { continue };
        let spent = db.gateway_spent(name, bare, seconds).map_err(unreadable)?;
        if spent >= limit {
            return Ok(Some(format!(
                "{name}/{bare}: the budget of ${limit:.2} per {period} is used (${spent:.2})"
            )));
        }
    }
    Ok(None)
}

/// The kept answer for `key`, recorded as a cached call of `model`; `label` is the cache header (hit, semantic). A
/// cache that cannot be read is a reported miss (user decision Q3).
fn replay(key: &str, model: &str, label: &'static str) -> Option<Response> {
    let kept = match Cache::open().and_then(|c| c.get("gateway", key)) {
        Ok(kept) => {
            crate::faults::works("cache-gateway");
            kept?
        }
        Err(e) => {
            crate::faults::failed("cache-gateway", GATEWAY_CACHE, &e);
            return None;
        }
    };
    let status = kept.meta["status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
        .unwrap_or(200);
    let content_type = kept.meta["content_type"]
        .as_str()
        .unwrap_or("application/json")
        .to_string();
    let body = kept.value;
    let tokens = usage(&body);
    record(&GatewayUsage {
        account: "cache".into(),
        model: model.into(),
        tokens_in: tokens.all_input(),
        tokens_out: tokens.output,
        cached: true,
        status,
        ..Default::default()
    });
    let mut response =
        (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body).into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_str(&content_type)
            .unwrap_or(HeaderValue::from_static("application/json")),
    );
    response
        .headers_mut()
        .insert("x-legionctl-cache", HeaderValue::from_static(label));
    Some(response)
}

/// The request as the account's API takes it. A Chat Completions stream asks for its usage (the last chunk): without
/// it the provider sends no token counts, and a budget would count the call as free.
fn provider_body(api: Api, direction: Option<Direction>, request: &Value) -> Result<Value, String> {
    let Some(d) = direction else {
        let mut body = request.clone();
        if api == Api::Chat && body["stream"] == true {
            body["stream_options"]["include_usage"] = Value::Bool(true);
        }
        return Ok(body);
    };
    translate::request(d, request)
}

async fn call(
    State(gw): State<Gateway>,
    axum::extract::ConnectInfo(caller): axum::extract::ConnectInfo<Caller>,
    api: Api,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    let Ok(mut request) = serde_json::from_slice::<Value>(&body) else {
        return refuse(StatusCode::BAD_REQUEST, "the body is not JSON");
    };
    let model = request["model"].as_str().unwrap_or_default().to_string();
    let config = match resolve(&Dirs::from_env(), None) {
        Ok(r) => r.config.gateway,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    };
    let accounts = match candidates(&config, api, &model) {
        Ok(a) => a,
        Err(why) => return refuse(StatusCode::BAD_REQUEST, &why),
    };
    let cache_mode = headers
        .get("x-legionctl-cache")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("on")
        .to_string();
    let (key, semantic) = match cached(
        &gw,
        &config,
        &cache_mode,
        (api, &model, &accounts),
        (&request, &headers, &caller.who),
    )
    .await
    {
        Ok(response) => return response,
        Err(keep) => keep,
    };
    let explicit = model.contains('/');
    let mut refusals = Vec::new();
    for (name, bare, direction) in accounts {
        let account = &config.accounts[&name];
        match blocked(&config, account, &name, &bare, &headers, &gw.token) {
            Ok(None) => {}
            Ok(Some(why)) => {
                refusals.push(why);
                continue;
            }
            Err(why) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &why),
        }
        request["model"] = Value::String(bare.clone());
        let body = match provider_body(api, direction, &request) {
            Ok(body) => body,
            Err(why) => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    &format!("{name}/{bare}: cannot translate the request: {why}"),
                );
            }
        };
        let target = direction.map_or(api, Api::of);
        let upstream = match send(
            &gw,
            target,
            account,
            &name,
            &headers,
            body.to_string().into_bytes(),
        )
        .await
        {
            Ok(r) => r,
            // failover is for a rate limit only (19-gateway): this call fails, with its reason
            Err(e) => return refuse(StatusCode::BAD_GATEWAY, &format!("{name}: {e:#}")),
        };
        let status = upstream.status().as_u16();
        if status == 429 && !explicit {
            refusals.push(format!("{name}: rate limited"));
            record(&GatewayUsage {
                account: name,
                model: bare,
                status,
                ..Default::default()
            });
            continue;
        }
        let price = config.prices.get(&bare).copied();
        return respond(
            upstream,
            GatewayUsage {
                account: name,
                model: bare,
                status,
                ..Default::default()
            },
            price,
            direction.map(|d| Translator::new(d, &request)),
            Keep {
                key,
                ttl_s: config.cache_ttl_s,
                exact: cache_mode != "off",
                semantic,
            },
            started,
        );
    }
    no_account(&model, &refusals)
}

/// The TCP port serves local programs only: a browser's request has an Origin, a rebound name another Host.
async fn local_only(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let headers = request.headers();
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|h| h.rsplit_once(':').map_or(h, |(name, _)| name));
    if headers.contains_key("origin") || !matches!(host, Some("127.0.0.1" | "localhost")) {
        return refuse(
            StatusCode::FORBIDDEN,
            "the gateway takes calls from local programs only (Host 127.0.0.1 or localhost, no Origin)",
        );
    }
    next.run(request).await
}

/// The model of the first of `accounts` that would serve this caller now; none when each is blocked.
///
/// # Errors
/// A budget cannot be read.
fn first_open(
    config: &GatewayConfig,
    accounts: &[Candidate],
    headers: &HeaderMap,
    token: &str,
) -> Result<Option<String>, String> {
    for (name, bare, _) in accounts {
        if blocked(config, &config.accounts[name], name, bare, headers, token)?.is_none() {
            return Ok(Some(bare.clone()));
        }
    }
    Ok(None)
}

/// No account served the call: 401 when a key needed the gateway token, else 429; every account's reason.
fn no_account(model: &str, refusals: &[String]) -> Response {
    let status = if refusals.iter().any(|r| r.contains("gateway token")) {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::TOO_MANY_REQUESTS
    };
    refuse(
        status,
        &format!("no account could serve {model}: {}", refusals.join("; ")),
    )
}

/// What to keep of an answer: under the exact key for `ttl_s` (unless the caller said off), and the prompt's
/// embedding within its scope for semantic replay.
struct Keep {
    key: String,
    ttl_s: u64,
    exact: bool,
    semantic: Option<(String, Vec<f64>)>,
}

/// The exact key of `request`, and the scope and embedding to keep with a new answer.
type ToKeep = (String, Option<(String, Vec<f64>)>);

/// A kept answer as `Ok`: the exact one, else (with `x-legionctl-cache: semantic` and `gateway.semantic` set) the
/// nearest by the prompt's embedding; else `Err` with what to keep with the new answer. An embedding failure is
/// logged and skips the semantic cache; the call goes on.
async fn cached(
    gw: &Gateway,
    config: &GatewayConfig,
    cache_mode: &str,
    (api, model, accounts): (Api, &str, &[Candidate]),
    (request, headers, caller): (&Value, &HeaderMap, &str),
) -> Result<Response, ToKeep> {
    // the key: the caller, the API and the request (keys sorted), not the account: any account's answer replays, to
    // the caller that asked only (another session's answers are not its to read or to poison)
    let key = format!(
        "{:x}",
        Blake2b::<U16>::digest(format!("{caller}\n{}{request}", api.path()).as_bytes())
    );
    // a kept answer replays only to a caller that an account would serve now (its token, its budget)
    let Ok(Some(served)) = first_open(config, accounts, headers, &gw.token) else {
        return Err((key, None));
    };
    if (cache_mode == "on" || cache_mode == "semantic")
        && config.cache_ttl_s > 0
        && let Some(response) = replay(&key, &served, "hit")
    {
        return Ok(response);
    }
    let Some(semantic) = config
        .semantic
        .as_ref()
        .filter(|_| cache_mode == "semantic" && config.cache_ttl_s > 0)
    else {
        return Err((key, None));
    };
    // the embedding account is held to the same rules as any account: its key needs the token, its budget counts
    if let Some(account) = config.accounts.get(&semantic.account) {
        // a budget that cannot be read skips the cache too (user decision Q2/Q3: a miss, with the reason)
        if let Some(why) = blocked(
            config,
            account,
            &semantic.account,
            &semantic.model,
            headers,
            &gw.token,
        )
        .unwrap_or_else(Some)
        {
            eprintln!("operator: semantic cache skipped: {why}");
            return Err((key, None));
        }
    }
    let (vector, row) = match crate::semantic::embed(
        &gw.http,
        config,
        semantic,
        &crate::semantic::prompt_text(request),
    )
    .await
    {
        Ok(found) => found,
        Err(e) => {
            eprintln!("operator: semantic cache skipped: {e:#}");
            return Err((key, None));
        }
    };
    record(&row);
    // the same API, model and everything but the prompt (stream mode, tools, parameters): only the prompt may differ
    let mut rest = request.clone();
    if let Some(fields) = rest.as_object_mut() {
        for prompt in ["system", "instructions", "messages", "input"] {
            fields.remove(prompt);
        }
    }
    let scope = format!(
        "{caller}\n{}{model}{:x}",
        api.path(),
        Blake2b::<U16>::digest(rest.to_string().as_bytes())
    );
    let nearest = match Cache::open()
        .and_then(|c| c.nearest("gateway", &scope, &vector, semantic.threshold))
    {
        Ok(nearest) => nearest,
        Err(e) => {
            crate::faults::failed("cache-gateway", GATEWAY_CACHE, &e);
            None
        }
    };
    match nearest.and_then(|kept| replay(&kept, &served, "semantic")) {
        Some(response) => Ok(response),
        None => Err((key, Some((scope, vector)))),
    }
}

/// Stream the provider's answer to the caller; at its end, record usage and keep it for replay.
fn respond(
    upstream: reqwest::Response,
    mut row: GatewayUsage,
    price: Option<Price>,
    mut translator: Option<Translator>,
    Keep {
        key,
        ttl_s,
        exact,
        semantic,
    }: Keep,
    started: Instant,
) -> Response {
    let status = row.status;
    let content_type = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    // a translated error is JSON whatever the request asked
    if let Some(t) = &mut translator
        && !content_type.starts_with("text/event-stream")
    {
        t.stream = None;
    }
    let mut chunks = upstream.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    let content_type_kept = content_type.clone();
    tokio::spawn(async move {
        // the provider's bytes count usage; the caller's bytes are what replays
        let (mut raw, mut body) = (Vec::new(), Vec::new());
        let mut complete = true;
        while let Some(chunk) = chunks.next().await {
            match chunk {
                Ok(bytes) => {
                    raw.extend_from_slice(&bytes);
                    let bytes = match &mut translator {
                        Some(t) => Bytes::from(t.push(&bytes)),
                        None => bytes,
                    };
                    body.extend_from_slice(&bytes);
                    if !bytes.is_empty() && tx.send(Ok(bytes)).await.is_err() {
                        complete = false;
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e))).await;
                    complete = false;
                    break;
                }
            }
        }
        if let Some(t) = &mut translator {
            let tail = Bytes::from(t.finish(&raw));
            body.extend_from_slice(&tail);
            if !tail.is_empty() && tx.send(Ok(tail)).await.is_err() {
                complete = false;
            }
        }
        drop(tx);
        let tokens = usage(&raw);
        (row.tokens_in, row.tokens_out) = (tokens.all_input(), tokens.output);
        row.cost_usd = price.map_or(0.0, |p| p.cost(tokens));
        row.ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        record(&row);
        if exact
            && complete
            && ttl_s > 0
            && (200..300).contains(&status)
            && let Err(e) = Cache::open().and_then(|cache| {
                let entry = Entry {
                    value: body.clone(),
                    meta: serde_json::json!({"status": status, "content_type": content_type_kept}),
                };
                let limit = legionctl_core::config::resolve(
                    &legionctl_core::config::Dirs::from_env(),
                    None,
                )?
                .config
                .cache
                .get("gateway")
                .map_or(0, |l| l.max_bytes);
                cache.put(("gateway", &key), &entry, ttl_s, limit)?;
                match &semantic {
                    Some((scope, vector)) => cache.keep_vector(("gateway", &key), scope, vector),
                    None => Ok(()),
                }
            })
        {
            crate::faults::failed("cache-gateway", GATEWAY_CACHE, &e);
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let mut response = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Body::from_stream(stream),
    )
        .into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_str(&content_type)
            .unwrap_or(HeaderValue::from_static("application/json")),
    );
    response
        .headers_mut()
        .insert("x-legionctl-cache", HeaderValue::from_static("miss"));
    response
}

/// A translated answer: event by event for a stream, whole for JSON.
struct Translator {
    direction: Direction,
    stream: Option<translate::Stream>,
}

impl Translator {
    fn new(direction: Direction, request: &Value) -> Self {
        Self {
            direction,
            stream: (request["stream"] == true).then(|| translate::Stream::new(direction, request)),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.stream
            .as_mut()
            .map_or_else(Vec::new, |s| s.push(bytes))
    }

    /// The rest of the caller's answer, once the provider's `raw` body is complete.
    fn finish(&mut self, raw: &[u8]) -> Vec<u8> {
        match &mut self.stream {
            Some(s) => s.finish(),
            // an answer that cannot be translated is an error in the caller's API, with the reason
            None => serde_json::from_slice::<Value>(raw)
                .map_err(|e| {
                    format!(
                        "the provider's answer is not JSON ({e}): {}",
                        String::from_utf8_lossy(&raw[..raw.len().min(200)])
                    )
                })
                .and_then(|v| translate::response(self.direction, &v))
                .unwrap_or_else(|why| translate::caller_error(self.direction, &why))
                .to_string()
                .into_bytes(),
        }
    }
}
