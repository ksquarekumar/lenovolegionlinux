//! The operation registry over HTTP on the socket that systemd passes in (`legionctl-operator.socket`):
//! `GET /v1/ops` lists the operations, `POST /v1/ops/<name>` runs one with a JSON input.

use std::os::fd::FromRawFd;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use legionctl_api::{ACTOR_HEADER, API_HEADER, Ctx, DispatchError, OPS, api_fingerprint};
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
        .with_state(operator)
}

async fn list() -> Json<Value> {
    Json(Value::Array(
        OPS.iter()
            .map(|op| json!({"name": op.name, "help": op.help, "class": op.class, "input": (op.input)()}))
            .collect(),
    ))
}

/// One telemetry sample in `OpenMetrics` text, for Prometheus or Grafana.
async fn metrics(State(operator): State<Arc<Operator>>) -> (StatusCode, String) {
    let sample =
        tokio::task::spawn_blocking(move || operator.sample(std::time::Duration::from_millis(250)))
            .await;
    match sample {
        Ok(s) => (
            StatusCode::OK,
            legionctl_machine::telemetry::openmetrics(&s),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("sampling failed: {e}\n"),
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
    input: Value,
) -> (StatusCode, Json<Value>) {
    let ctx = Ctx {
        actor: headers
            .get(ACTOR_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string(),
    };
    // engine methods read files and run programs; keep them off the async threads
    let result = tokio::task::spawn_blocking(move || {
        legionctl_api::dispatch(&*operator, &ctx, &name, input)
    })
    .await;
    match result {
        Ok(Ok(output)) => (StatusCode::OK, Json(output)),
        Ok(Err(DispatchError::Unknown(name))) => {
            // after an upgrade the running operator is the old build until it restarts
            let other_build = headers
                .get(API_HEADER)
                .is_some_and(|v| v != api_fingerprint());
            let hint = if other_build {
                "; this operator is a different build than the client: restart it \
                 (systemctl --user restart legionctl-operator)"
            } else {
                ""
            };
            error(
                StatusCode::NOT_FOUND,
                &format!("unknown operation {name}{hint}"),
            )
        }
        Ok(Err(DispatchError::Input(e))) => {
            error(StatusCode::BAD_REQUEST, &format!("invalid input: {e}"))
        }
        Ok(Err(DispatchError::Failed(e))) => {
            error(StatusCode::UNPROCESSABLE_ENTITY, &format!("{e:#}"))
        }
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("operation crashed: {e}"),
        ),
    }
}

fn error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message})))
}
