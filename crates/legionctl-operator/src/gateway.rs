//! The model gateway (note 19, G1): the Anthropic and `OpenAI` APIs on 127.0.0.1, in front of named accounts. A plain
//! model goes to the first account of its pool entry (or `*`) that has quota and budget left; `account/model` goes to
//! exactly that account. Exact responses replay from the cache (streams too); every call is a usage row.
//!
//! An account without a keyring key forwards the caller's own credentials (Claude Code and Codex logins), so it
//! exposes nothing. An account with a key serves only callers that send the gateway token
//! (`x-legionctl-token`, from `~/.local/state/legionctl/gateway-token`): any local user can reach a local port.

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
use legionctl_core::config::{Dirs, GatewayAccount, GatewayConfig, Price, resolve};
use legionctl_core::db::{Db, GatewayUsage};
use serde_json::{Value, json};

use crate::translate::{self, Direction};

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

/// The token a caller sends for accounts with keyring keys; made once, readable only by the user.
fn token() -> Result<String> {
    let path = legionctl_core::db::default_path().with_file_name("gateway-token");
    if let Ok(token) = std::fs::read_to_string(&path) {
        return Ok(token.trim().to_string());
    }
    let mut bytes = [0u8; 16];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut bytes)?;
    let token = bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{b:02x}"));
        hex
    });
    std::fs::create_dir_all(path.parent().context("no state folder")?)?;
    std::fs::write(&path, &token)?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(token)
}

/// Serve the gateway on 127.0.0.1:`port` until the operator stops.
///
/// # Errors
/// The port is taken or the token cannot be made.
pub async fn serve(port: u16) -> Result<()> {
    let gateway = Gateway {
        http: reqwest::Client::new(),
        token: token()?,
    };
    let app = axum::Router::new()
        .route("/v1/messages", post(|s, h, b| call(s, Api::Messages, h, b)))
        .route(
            "/v1/chat/completions",
            post(|s, h, b| call(s, Api::Chat, h, b)),
        )
        .route(
            "/v1/responses",
            post(|s, h, b| call(s, Api::Responses, h, b)),
        )
        .with_state(gateway);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("gateway port {port}"))?;
    eprintln!("operator: model gateway on 127.0.0.1:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(json!({ "error": { "type": "legionctl_gateway", "message": message } })),
    )
        .into_response()
}

/// Tokens in and out from a response: JSON, or every `data:` event of a stream (Anthropic's `message_start` and
/// `message_delta`, the usage chunk of `OpenAI`).
fn usage(body: &[u8]) -> (i64, i64) {
    let text = String::from_utf8_lossy(body);
    let objects: Vec<Value> = match serde_json::from_str::<Value>(&text) {
        Ok(v) => vec![v],
        Err(_) => text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect(),
    };
    let mut tokens = (0, 0);
    for object in &objects {
        for usage in [
            &object["usage"],
            &object["message"]["usage"],
            &object["response"]["usage"],
        ] {
            let get = |keys: [&str; 2]| {
                keys.iter()
                    .filter_map(|k| usage[*k].as_i64())
                    .max()
                    .unwrap_or(0)
            };
            tokens.0 = tokens.0.max(get(["input_tokens", "prompt_tokens"]));
            tokens.1 = tokens.1.max(get(["output_tokens", "completion_tokens"]));
        }
    }
    tokens
}

fn record(row: &GatewayUsage) {
    if let Err(e) = Db::open_default().and_then(|db| db.gateway_usage_add(row)) {
        eprintln!("operator: gateway usage not recorded: {e:#}");
    }
}

/// The accounts that may serve `model` for `api`: the named one, or the pool entry (then `*`), with the translation
/// each needs.
fn candidates(
    config: &GatewayConfig,
    api: Api,
    model: &str,
) -> Result<Vec<(String, String, Option<Direction>)>, String> {
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
        let Some(account) = config.accounts.get(name) else {
            continue;
        };
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

/// Why `name` cannot serve `bare` now: its key needs the gateway token, or a money budget is used.
fn blocked(
    config: &GatewayConfig,
    account: &GatewayAccount,
    name: &str,
    bare: &str,
    headers: &HeaderMap,
    token: &str,
) -> Option<String> {
    if account.keyring
        && headers
            .get("x-legionctl-token")
            .and_then(|v| v.to_str().ok())
            != Some(token)
    {
        return Some(format!(
            "{name}: this account's key needs the gateway token (x-legionctl-token)"
        ));
    }
    let budget = config.budgets.get(&format!("{name}/{bare}"))?;
    let db = Db::open_default().ok()?;
    [
        ("day", budget.day_usd, 86_400),
        ("30 days", budget.month_usd, 30 * 86_400),
    ]
    .into_iter()
    .find_map(|(period, limit, seconds)| {
        let limit = limit?;
        let spent = db.gateway_spent(name, bare, seconds).unwrap_or(0.0);
        (spent >= limit).then(|| {
            format!("{name}/{bare}: the budget of ${limit:.2} per {period} is used (${spent:.2})")
        })
    })
}

/// The kept answer for `key`, recorded as a cached call of `model`; `label` is the cache header (hit, semantic).
fn replay(key: &str, model: &str, label: &'static str) -> Option<Response> {
    let (status, content_type, body) = Db::open_default()
        .and_then(|db| db.gateway_cached(key))
        .ok()??;
    let (tokens_in, tokens_out) = usage(&body);
    record(&GatewayUsage {
        account: "cache".into(),
        model: model.into(),
        tokens_in,
        tokens_out,
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

/// The request as the account's API takes it.
fn provider_body(direction: Option<Direction>, request: &Value) -> Result<Value, String> {
    direction.map_or_else(|| Ok(request.clone()), |d| translate::request(d, request))
}

async fn call(State(gw): State<Gateway>, api: Api, headers: HeaderMap, body: Bytes) -> Response {
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
        (api, &model, &accounts[0].1),
        (&request, &headers),
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
        if let Some(why) = blocked(&config, account, &name, &bare, &headers, &gw.token) {
            refusals.push(why);
            continue;
        }
        request["model"] = Value::String(bare.clone());
        let body = match provider_body(direction, &request) {
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
            Err(e) => {
                refusals.push(format!("{name}: {e:#}"));
                continue;
            }
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
    (api, model, served): (Api, &str, &str),
    (request, headers): (&Value, &HeaderMap),
) -> Result<Response, ToKeep> {
    // the key: the API and the request (keys sorted), not the account: any account's answer replays
    let key = format!(
        "{:x}",
        Blake2b::<U16>::digest(format!("{}{request}", api.path()).as_bytes())
    );
    if (cache_mode == "on" || cache_mode == "semantic")
        && config.cache_ttl_s > 0
        && let Some(response) = replay(&key, served, "hit")
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
    if let Some(why) = config.accounts.get(&semantic.account).and_then(|account| {
        blocked(
            config,
            account,
            &semantic.account,
            &semantic.model,
            headers,
            &gw.token,
        )
    }) {
        eprintln!("operator: semantic cache skipped: {why}");
        return Err((key, None));
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
        "{}{model}{:x}",
        api.path(),
        Blake2b::<U16>::digest(rest.to_string().as_bytes())
    );
    let nearest = Db::open_default()
        .and_then(|db| db.gateway_semantic_nearest(&scope, &vector, semantic.threshold))
        .ok()
        .flatten();
    match nearest.and_then(|kept| replay(&kept, served, "semantic")) {
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
        (row.tokens_in, row.tokens_out) = usage(&raw);
        row.cost_usd = price.map_or(0.0, |p| p.cost(row.tokens_in, row.tokens_out));
        row.ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        record(&row);
        if exact
            && complete
            && ttl_s > 0
            && (200..300).contains(&status)
            && let Err(e) = Db::open_default().and_then(|db| {
                db.gateway_keep(&key, status, &content_type_kept, &body, ttl_s)?;
                match &semantic {
                    Some((scope, vector)) => db.gateway_semantic_keep(&key, scope, vector),
                    None => Ok(()),
                }
            })
        {
            eprintln!("operator: gateway response not kept: {e:#}");
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
            None => serde_json::from_slice::<Value>(raw).map_or_else(
                |_| raw.to_vec(),
                |v| {
                    translate::response(self.direction, &v)
                        .to_string()
                        .into_bytes()
                },
            ),
        }
    }
}
