//! Calls to `legionctl-operator` over its socket: HTTP/JSON on `$XDG_RUNTIME_DIR/legionctl/api.sock`.

use std::path::{Path, PathBuf};

use crate::{
    ACTOR_HEADER, API_HEADER, ApiError, Code, Op, SOCKET, TOKEN_ENV, api_fingerprint, coded,
    user_token,
};
use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use serde_json::Value;

/// The build warning is printed once per process.
static BUILD_WARNED: std::sync::Once = std::sync::Once::new();

pub struct Client {
    /// Which surface calls: cli, mcp or app. The operator records it with every change.
    actor: &'static str,
    socket: PathBuf,
    runtime: PathBuf,
}

impl Client {
    /// # Errors
    /// `XDG_RUNTIME_DIR` is not set.
    pub fn new(actor: &'static str) -> Result<Client> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
        Ok(Client::at(actor, Path::new(&runtime)))
    }

    /// The operator of the runtime folder `runtime` (`$XDG_RUNTIME_DIR`).
    #[must_use]
    pub fn at(actor: &'static str, runtime: &Path) -> Client {
        Client {
            actor,
            socket: runtime.join(SOCKET),
            runtime: runtime.to_path_buf(),
        }
    }

    /// Run the operation `name`.
    ///
    /// # Errors
    /// An [`ApiError`]: `unavailable` when the operator does not answer, else the operator's answer.
    pub async fn call_value(&self, name: &str, input: &Value) -> Result<Value> {
        let request = hyper::Request::post(format!("/v1/ops/{name}"))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(serde_json::to_vec(input)?)))?;
        let response = self.send(request).await?;
        let status = response.status();
        let body: Value = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())
            .context("the operator's answer is not JSON")?;
        if !status.is_success() {
            return Err(refusal(status, &body, name).into());
        }
        Ok(body)
    }

    /// Follow the operator's event log from `offset` (an event id, or `now`, which is fixed when this returns).
    ///
    /// # Errors
    /// As [`Client::call_value`].
    pub async fn follow(&self, offset: &str) -> Result<Events> {
        let request = hyper::Request::get(format!("/v1/events?offset={offset}"))
            .body(Full::new(Bytes::new()))?;
        let response = self.send(request).await?;
        let status = response.status();
        let body = response.into_body();
        if !status.is_success() {
            let body: Value =
                serde_json::from_slice(&body.collect().await?.to_bytes()).unwrap_or(Value::Null);
            return Err(refusal(status, &body, "events").into());
        }
        Ok(Events(body))
    }

    /// Send `request` to the operator with this client's surface, build and token.
    async fn send(
        &self,
        mut request: hyper::Request<Full<Bytes>>,
    ) -> Result<hyper::Response<hyper::body::Incoming>> {
        let stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .map_err(|e| {
                coded(
                    Code::Unavailable,
                    format!(
                        "legionctl-operator does not answer on {} ({e}); start it with \
                     `systemctl --user enable --now legionctl-operator.socket`",
                        self.socket.display()
                    ),
                )
            })?;
        // a session's sandbox has its own token; everywhere else the user's
        let token = match std::env::var(TOKEN_ENV) {
            Ok(t) if !t.is_empty() => t,
            _ => user_token(&self.runtime)?,
        };
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await?;
        tokio::spawn(connection);
        let headers = request.headers_mut();
        headers.insert("host", hyper::header::HeaderValue::from_static("legionctl"));
        headers.insert(
            ACTOR_HEADER,
            hyper::header::HeaderValue::from_static(self.actor),
        );
        headers.insert(
            API_HEADER,
            hyper::header::HeaderValue::from_static(api_fingerprint()),
        );
        headers.insert("authorization", format!("Bearer {token}").parse()?);
        let response = sender.send_request(request).await?;
        if response
            .headers()
            .get(API_HEADER)
            .is_some_and(|b| b.as_bytes() != api_fingerprint().as_bytes())
        {
            // legionctl's own parts must not guess at another build's wire format; a person gets the warning
            if matches!(self.actor, "hook" | "gate" | "mcp-proxy") {
                return Err(coded(
                    Code::VersionMismatch,
                    "the running legionctl-operator is a different build than this client; restart it \
                     (systemctl --user restart legionctl-operator)",
                ));
            }
            BUILD_WARNED.call_once(|| {
                eprintln!(
                    "legionctl: the running operator is a different build than this client; restart it \
                     (systemctl --user restart legionctl-operator)"
                );
            });
        }
        Ok(response)
    }

    /// Run the typed operation `O`.
    ///
    /// # Errors
    /// As [`Client::call_value`], or the answer does not fit `O::Out`.
    pub async fn call<O: Op>(&self, input: &O::In) -> Result<O::Out> {
        let output = self
            .call_value(O::NAME, &serde_json::to_value(input)?)
            .await?;
        Ok(serde_json::from_value(output)?)
    }
}

/// The operator's error envelope as an [`ApiError`].
fn refusal(status: hyper::StatusCode, body: &Value, op: &str) -> ApiError {
    serde_json::from_value(body["error"].clone()).unwrap_or(ApiError {
        code: Code::Failed,
        message: format!("{op} failed with HTTP {status}"),
        op: op.to_string(),
    })
}

/// The operator's event stream, from [`Client::follow`].
pub struct Events(hyper::body::Incoming);

impl Events {
    /// `each` gets every event as it comes, until the stream ends.
    ///
    /// # Errors
    /// The stream breaks, an event is not JSON, or the operator ends it with an `error` event (it cannot read its log).
    pub async fn each(mut self, mut each: impl FnMut(Value)) -> Result<()> {
        // server-sent events: blocks end with an empty line; the event is the `data:` line
        let mut bytes = Vec::new();
        while let Some(frame) = self.0.frame().await {
            if let Some(data) = frame?.data_ref() {
                bytes.extend_from_slice(data);
            }
            while let Some(end) = bytes.windows(2).position(|w| w == b"\n\n") {
                let block: Vec<u8> = bytes.drain(..end + 2).collect();
                let block = String::from_utf8_lossy(&block);
                let error = block.lines().any(|l| l.trim() == "event: error");
                for data in block.lines().filter_map(|l| l.strip_prefix("data:")) {
                    let value: Value = serde_json::from_str(data.trim())?;
                    if error {
                        anyhow::bail!(
                            "the event stream ended: {}",
                            value["error"].as_str().unwrap_or_default()
                        );
                    }
                    each(value);
                }
            }
        }
        Ok(())
    }
}
