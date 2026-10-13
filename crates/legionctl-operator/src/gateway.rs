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
use legionctl_core::config::{Dirs, GatewayAccount, GatewayConfig, resolve};
use legionctl_core::db::{Db, GatewayUsage};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Api {
    Anthropic,
    OpenAi,
}

impl Api {
    fn provider(self) -> &'static str {
        match self {
            Api::Anthropic => "anthropic",
            Api::OpenAi => "openai",
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
        .route(
            "/v1/messages",
            post(|s, h, b| call(s, Api::Anthropic, "/v1/messages", h, b)),
        )
        .route(
            "/v1/chat/completions",
            post(|s, h, b| call(s, Api::OpenAi, "/v1/chat/completions", h, b)),
        )
        .route(
            "/v1/responses",
            post(|s, h, b| call(s, Api::OpenAi, "/v1/responses", h, b)),
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

/// The accounts that may serve `model` for `api`: the named one, or the pool entry (then `*`).
fn candidates(
    config: &GatewayConfig,
    api: Api,
    model: &str,
) -> Result<Vec<(String, String)>, String> {
    if let Some((account, bare)) = model.split_once('/') {
        let a = config
            .accounts
            .get(account)
            .ok_or(format!("no gateway account {account}"))?;
        if a.provider != api.provider() {
            return Err(format!(
                "account {account} speaks the {} API; translating between the OpenAI and Anthropic APIs is not built yet",
                a.provider
            ));
        }
        return Ok(vec![(account.into(), bare.into())]);
    }
    let pool = config
        .pool
        .get(model)
        .or_else(|| config.pool.get("*"))
        .ok_or(format!("no pool for {model}"))?;
    let accounts: Vec<(String, String)> = pool
        .iter()
        .filter(|a| {
            config
                .accounts
                .get(*a)
                .is_some_and(|a| a.provider == api.provider())
        })
        .map(|a| (a.clone(), model.to_string()))
        .collect();
    if accounts.is_empty() {
        return Err(format!(
            "the pool for {model} has no {} account",
            api.provider()
        ));
    }
    Ok(accounts)
}

async fn key(account: &str) -> Result<String> {
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
    path: &str,
    account: &GatewayAccount,
    name: &str,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> Result<reqwest::Response> {
    let mut request = gw
        .http
        .post(format!("{}{path}", account.base_url.trim_end_matches('/')))
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
    if account.keyring {
        let key = key(name).await?;
        request = match api {
            Api::Anthropic => request.header("x-api-key", key),
            Api::OpenAi => request.bearer_auth(key),
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

/// Why `name` cannot serve `bare` now: its key needs the gateway token, or its budget for the day is used.
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
    let used = Db::open_default()
        .and_then(|db| db.gateway_tokens_today(name, bare))
        .unwrap_or(0);
    (used >= *budget).then(|| format!("{name}/{bare}: the day's budget of {budget} tokens is used"))
}

/// The kept answer for `key`, recorded as a cached call of `model`.
fn replay(key: &str, model: &str) -> Option<Response> {
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
        .insert("x-legionctl-cache", HeaderValue::from_static("hit"));
    Some(response)
}

async fn call(
    State(gw): State<Gateway>,
    api: Api,
    path: &'static str,
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
    // the key: the API and the request (keys sorted), not the account: any account's answer replays
    let key = format!(
        "{:x}",
        Blake2b::<U16>::digest(format!("{path}{request}").as_bytes())
    );
    if cache_mode == "on"
        && config.cache_ttl_s > 0
        && let Some(response) = replay(&key, &accounts[0].1)
    {
        return response;
    }
    let explicit = model.contains('/');
    let mut refusals = Vec::new();
    for (name, bare) in accounts {
        let account = &config.accounts[&name];
        if let Some(why) = blocked(&config, account, &name, &bare, &headers, &gw.token) {
            refusals.push(why);
            continue;
        }
        request["model"] = Value::String(bare.clone());
        let upstream = match send(
            &gw,
            api,
            path,
            account,
            &name,
            &headers,
            request.to_string().into_bytes(),
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
        return respond(
            upstream,
            name,
            bare,
            key,
            config.cache_ttl_s,
            cache_mode != "off",
            started,
        );
    }
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

/// Stream the provider's answer to the caller; at its end, record usage and keep it for replay.
fn respond(
    upstream: reqwest::Response,
    account: String,
    model: String,
    key: String,
    ttl_s: u64,
    keep: bool,
    started: Instant,
) -> Response {
    let status = upstream.status().as_u16();
    let content_type = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let mut chunks = upstream.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    let content_type_kept = content_type.clone();
    tokio::spawn(async move {
        let mut body = Vec::new();
        let mut complete = true;
        while let Some(chunk) = chunks.next().await {
            match chunk {
                Ok(bytes) => {
                    body.extend_from_slice(&bytes);
                    if tx.send(Ok(bytes)).await.is_err() {
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
        let (tokens_in, tokens_out) = usage(&body);
        let ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        record(&GatewayUsage {
            account,
            model,
            tokens_in,
            tokens_out,
            cached: false,
            status,
            ms,
            ..Default::default()
        });
        if keep
            && complete
            && ttl_s > 0
            && (200..300).contains(&status)
            && let Err(e) = Db::open_default()
                .and_then(|db| db.gateway_keep(&key, status, &content_type_kept, &body, ttl_s))
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
