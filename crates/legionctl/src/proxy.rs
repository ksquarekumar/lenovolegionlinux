//! `legionctl mcp-proxy`: the only way a session's agent reaches an MCP server (note 14). It starts the server as its
//! child, offers the agent only the allowed tools, refuses the others, and logs every call to the operator
//! (`tools.record`): session, server, tool, a hash of the arguments, duration, result size and outcome.

use std::time::Instant;

use blake2::digest::consts::U16;
use blake2::{Blake2b, Digest};
use legionctl_api::client::Client;
use legionctl_api::{ToolCall, op};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt};

struct Proxy {
    server: String,
    /// Empty: every tool.
    allow: Vec<String>,
    upstream: RunningService<RoleClient, ()>,
    client: Client,
    session: String,
}

impl Proxy {
    fn allowed(&self, tool: &str) -> bool {
        self.allow.is_empty() || self.allow.iter().any(|a| a == tool)
    }

    async fn log(
        &self,
        tool: &str,
        args_hash: String,
        started: Instant,
        bytes: usize,
        outcome: &str,
    ) {
        let call = ToolCall {
            session: self.session.clone(),
            server: self.server.clone(),
            tool: tool.to_string(),
            args_hash,
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            bytes: u64::try_from(bytes).unwrap_or(u64::MAX),
            outcome: outcome.into(),
        };
        // shortcut: a call is not refused when the log fails (the operator is down); the failure goes to stderr,
        // which the agent shows in its MCP log
        if let Err(e) = self.client.call::<op::ToolsRecord>(&call).await {
            eprintln!(
                "legionctl mcp-proxy: cannot log {}.{tool}: {e:#}",
                self.server
            );
        }
    }
}

impl ServerHandler for Proxy {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self
            .upstream
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| ErrorData::internal_error(format!("{}: {e}", self.server), None))?;
        Ok(ListToolsResult::with_all_items(
            tools
                .into_iter()
                .filter(|t| self.allowed(&t.name))
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let started = Instant::now();
        let tool = request.name.to_string();
        let args = serde_json::to_vec(&request.arguments).unwrap_or_default();
        let args_hash = format!("{:x}", Blake2b::<U16>::digest(&args));
        if !self.allowed(&tool) {
            self.log(&tool, args_hash, started, 0, "refused").await;
            return Err(ErrorData::invalid_params(
                format!(
                    "{tool} is not allowed for {} in this session (mcp.servers.{}.allow)",
                    self.server, self.server
                ),
                None,
            ));
        }
        match self.upstream.peer().call_tool(request).await {
            Ok(result) => {
                let bytes = serde_json::to_vec(&result).map_or(0, |b| b.len());
                let outcome = if result.is_error == Some(true) {
                    "error"
                } else {
                    "ok"
                };
                self.log(&tool, args_hash, started, bytes, outcome).await;
                Ok(result.into())
            }
            Err(e) => {
                self.log(&tool, args_hash, started, 0, "failed").await;
                Ok(
                    CallToolResult::error(vec![ContentBlock::text(format!(
                        "{}: {e}",
                        self.server
                    ))])
                    .into(),
                )
            }
        }
    }
}

/// Serve the agent on stdio, forwarding to `command`.
///
/// # Errors
/// The server does not start, or the stdio transport fails.
pub async fn serve(server: String, allow: Vec<String>, command: Vec<String>) -> anyhow::Result<()> {
    let mut child = tokio::process::Command::new(&command[0]);
    child.args(&command[1..]);
    let upstream = ().serve(TokioChildProcess::new(child)?).await?;
    let proxy = Proxy {
        server,
        allow,
        upstream,
        client: Client::new("mcp-proxy")?,
        session: std::env::var("LEGIONCTL_SESSION").unwrap_or_default(),
    };
    proxy
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
