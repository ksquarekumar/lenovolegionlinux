//! Calls to `legionctl-operator` over its socket: HTTP/JSON on `$XDG_RUNTIME_DIR/legionctl/api.sock`.

use std::path::PathBuf;

use crate::{ACTOR_HEADER, Op, SOCKET};
use anyhow::{Context, Result, bail};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use serde_json::Value;

pub struct Client {
    /// Which surface calls: cli, mcp or tui. The operator records it with every change.
    actor: &'static str,
    socket: PathBuf,
}

impl Client {
    /// # Errors
    /// `XDG_RUNTIME_DIR` is not set.
    pub fn new(actor: &'static str) -> Result<Client> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
        Ok(Client {
            actor,
            socket: PathBuf::from(runtime).join(SOCKET),
        })
    }

    /// Run the operation `name`.
    ///
    /// # Errors
    /// The operator does not answer, or refuses: the error carries its message.
    pub async fn call_value(&self, name: &str, input: &Value) -> Result<Value> {
        let stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .with_context(|| {
                format!(
                    "legionctl-operator does not answer on {}; start it with \
                 `systemctl --user enable --now legionctl-operator.socket`",
                    self.socket.display()
                )
            })?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await?;
        tokio::spawn(connection);
        let request = hyper::Request::post(format!("/v1/ops/{name}"))
            .header("host", "legionctl")
            .header("content-type", "application/json")
            .header(ACTOR_HEADER, self.actor)
            .body(Full::new(Bytes::from(serde_json::to_vec(input)?)))?;
        let response = sender.send_request(request).await?;
        let status = response.status();
        let body: Value = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())
            .context("the operator's answer is not JSON")?;
        if !status.is_success() {
            bail!("{}", body["error"].as_str().unwrap_or("operation failed"));
        }
        Ok(body)
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
