//! Calls to `legionctl-operator` over its socket: HTTP/JSON on `$XDG_RUNTIME_DIR/legionctl/api.sock`.

use std::path::{Path, PathBuf};

use crate::{
    ACTOR_HEADER, AGENT_HEADER, API_HEADER, ApiError, CALL_HEADER, Code, FOLDER_HEADER, Op, SOCKET,
    TOKEN_ENV, api_fingerprint, coded, user_token,
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
    /// The folder every call works in (the call's context): this process's working folder until
    /// [`Client::set_folder`] sets another (the app's project, the hook's `cwd`).
    folder: std::sync::Mutex<Option<PathBuf>>,
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
            folder: std::sync::Mutex::new(None),
        }
    }

    /// Every call from now on works in `folder`.
    pub fn set_folder(&self, folder: &Path) {
        *self
            .folder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(folder.to_path_buf());
    }

    /// The folder of the calls.
    ///
    /// # Errors
    /// No folder is set and this process's working folder cannot be read.
    pub fn folder(&self) -> Result<PathBuf> {
        match &*self
            .folder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            Some(folder) => Ok(folder.clone()),
            None => std::env::current_dir().context("reading the working folder"),
        }
    }

    /// The runtime folder (`$XDG_RUNTIME_DIR`) of this client's operator.
    #[must_use]
    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    /// Run the operation `name`.
    ///
    /// # Errors
    /// An [`ApiError`]: `unavailable` when the operator does not answer, else the operator's answer.
    pub async fn call_value(&self, name: &str, input: &Value) -> Result<Value> {
        self.call_recorded(name, input, None).await.0
    }

    /// Run the operation `name` as the call `call` (a retry of it: the operator answers with the recorded result) or
    /// as a new call: the answer, and the call id when the operator keeps the call in its record.
    pub async fn call_recorded(
        &self,
        name: &str,
        input: &Value,
        call: Option<&str>,
    ) -> (Result<Value>, Option<String>) {
        let mut id = None;
        let answer = async {
            let mut request = hyper::Request::post(format!("/v1/ops/{name}"))
                .header("content-type", "application/json");
            if let Some(call) = call {
                request = request.header(CALL_HEADER, call);
            }
            let request = request.body(Full::new(Bytes::from(serde_json::to_vec(input)?)))?;
            let response = self.send(request).await?;
            id = response
                .headers()
                .get(CALL_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let status = response.status();
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await?.to_bytes())
                    .context("the operator's answer is not JSON")?;
            if !status.is_success() {
                return Err(refusal(status, &body, name).into());
            }
            Ok(body)
        }
        .await;
        (answer, id)
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
                        "legionctl-operator is off (no answer on {}: {e}); turn it on with \
                     `systemctl --user enable --now legionctl-operator.socket`",
                        self.socket.display()
                    ),
                )
            })?;
        // a session's sandbox has its own token; everywhere else the user's, which an agent's client marks as the
        // agent's (its MCP server and hooks always run for one; the CLI does in an agent's shell)
        let (token, agent) = match std::env::var(TOKEN_ENV) {
            Ok(t) if !t.is_empty() => (t, None),
            _ => (
                user_token(&self.runtime)?,
                crate::session_from_env().or_else(|| {
                    matches!(self.actor, "mcp" | "hook").then(|| self.actor.to_string())
                }),
            ),
        };
        let agent = agent.filter(|_| self.actor != "app");
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
        if let Some(agent) = agent {
            headers.insert(AGENT_HEADER, agent.parse()?);
        }
        let folder = std::path::absolute(self.folder()?)?;
        headers.insert(
            FOLDER_HEADER,
            hyper::header::HeaderValue::from_bytes(folder.as_os_str().as_encoded_bytes())
                .with_context(|| {
                    format!(
                        "the folder {} has a line break in its name",
                        folder.display()
                    )
                })?,
        );
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
