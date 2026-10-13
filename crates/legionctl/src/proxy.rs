//! `legionctl mcp-proxy`: the only way a session's agent reaches an MCP server (note 14). It starts the server as its
//! child, offers the agent only the allowed tools, refuses the others, and logs every call to the operator
//! (`internal.tools.record`): session, server, tool, a hash of the arguments, duration, result size and outcome. Tools its
//! config lets it cache are answered from the operator's cache while the result is valid (T2).

use std::time::Instant;

use blake2::digest::consts::U16;
use blake2::{Blake2b, Digest};
use legionctl_api::client::Client;
use legionctl_api::{CacheGetIn, CachePutIn, Outcome, ToolCall, op};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt};

/// `--cache tool=ttl[:workspace]`: a tool whose result may be reused for `ttl_s`, keyed also by the git state when it
/// reads the working copy.
#[derive(Debug, Clone)]
pub struct CacheRule {
    pub tool: String,
    pub ttl_s: u64,
    pub workspace: bool,
}

impl std::str::FromStr for CacheRule {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<CacheRule> {
        let (tool, rest) = s
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--cache {s}: want tool=ttl[:workspace]"))?;
        let (ttl, workspace) = match rest.split_once(':') {
            Some((ttl, "workspace")) => (ttl, true),
            Some(_) => anyhow::bail!("--cache {s}: the scope can only be workspace"),
            None => (rest, false),
        };
        Ok(CacheRule {
            tool: tool.into(),
            ttl_s: ttl.parse()?,
            workspace,
        })
    }
}

/// The working copy's state for a cache key: HEAD, status and the diff against HEAD.
async fn git_state() -> String {
    let mut state = Vec::new();
    for args in [
        &["rev-parse", "HEAD"][..],
        &["status", "--porcelain=v2"],
        &["diff", "HEAD"],
    ] {
        if let Ok(out) = tokio::process::Command::new("git")
            .args(args)
            .output()
            .await
        {
            state.extend(out.stdout);
        }
    }
    format!("{:x}", Blake2b::<U16>::digest(&state))
}

struct Proxy {
    server: String,
    /// The server's command: part of each cache key (for a package it names the store version).
    command: Vec<String>,
    /// Empty: every tool.
    allow: Vec<String>,
    cache: Vec<CacheRule>,
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
        outcome: Outcome,
    ) {
        let call = ToolCall {
            session: legionctl_api::SessionId(self.session.clone()),
            server: self.server.clone(),
            tool: tool.to_string(),
            args_hash,
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            bytes: u64::try_from(bytes).unwrap_or(u64::MAX),
            outcome,
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
            self.log(&tool, args_hash, started, 0, Outcome::Refused)
                .await;
            return Err(ErrorData::invalid_params(
                format!(
                    "{tool} is not allowed for {} in this session (mcp.servers.{}.allow)",
                    self.server, self.server
                ),
                None,
            ));
        }
        let rule = self.cache.iter().find(|r| r.tool == tool);
        let key = match rule {
            Some(rule) => {
                let state = if rule.workspace {
                    git_state().await
                } else {
                    String::new()
                };
                let input = serde_json::to_vec(&(&self.command, &tool, &request.arguments, state))
                    .unwrap_or_default();
                Some(format!("{:x}", Blake2b::<U16>::digest(&input)))
            }
            None => None,
        };
        if let Some(key) = &key
            && let Ok(hit) = self
                .client
                .call::<op::CacheGet>(&CacheGetIn { key: key.clone() })
                .await
            && let Some(result) = hit
                .value
                .and_then(|v| serde_json::from_str::<CallToolResult>(&v).ok())
        {
            self.log(
                &tool,
                args_hash,
                started,
                hit_size(&result),
                Outcome::Cached,
            )
            .await;
            return Ok(result.into());
        }
        match self.upstream.peer().call_tool(request).await {
            Ok(result) => {
                let bytes = hit_size(&result);
                let outcome = if result.is_error == Some(true) {
                    Outcome::Error
                } else {
                    Outcome::Ok
                };
                if let (Some(key), Some(rule), Outcome::Ok) = (key, rule, outcome) {
                    let put = CachePutIn {
                        key,
                        value: serde_json::to_string(&result).unwrap_or_default(),
                        ttl_s: rule.ttl_s,
                    };
                    if let Err(e) = self.client.call::<op::CachePut>(&put).await {
                        eprintln!(
                            "legionctl mcp-proxy: cannot cache {}.{tool}: {e:#}",
                            self.server
                        );
                    }
                }
                self.log(&tool, args_hash, started, bytes, outcome).await;
                Ok(result.into())
            }
            Err(e) => {
                self.log(&tool, args_hash, started, 0, Outcome::Failed)
                    .await;
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

fn hit_size(result: &CallToolResult) -> usize {
    serde_json::to_vec(result).map_or(0, |b| b.len())
}

/// The server behind the proxy: a command it starts, or a remote one.
pub enum Upstream {
    Command {
        argv: Vec<String>,
        no_network: bool,
    },
    Url {
        url: String,
        headers: Vec<(String, String)>,
    },
}

async fn connect(upstream: &Upstream) -> anyhow::Result<RunningService<RoleClient, ()>> {
    Ok(match upstream {
        Upstream::Command { argv, no_network } => {
            let mut child = if *no_network {
                // the same files, no network namespace: only loopback, and unix sockets still reach the operator
                let mut c = tokio::process::Command::new("bwrap");
                c.args([
                    "--die-with-parent",
                    "--dev-bind",
                    "/",
                    "/",
                    "--unshare-net",
                    "--",
                ])
                .args(argv);
                c
            } else {
                let mut c = tokio::process::Command::new(&argv[0]);
                c.args(&argv[1..]);
                c
            };
            child.kill_on_drop(true);
            ().serve(TokioChildProcess::new(child)?).await?
        }
        Upstream::Url { url, headers } => {
            let mut config =
                rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(url.as_str());
            for (name, value) in headers {
                config.custom_headers.insert(
                    http::HeaderName::try_from(name.as_str())?,
                    http::HeaderValue::try_from(value.as_str())?,
                );
            }
            ().serve(rmcp::transport::StreamableHttpClientTransport::from_config(
                config,
            ))
            .await?
        }
    })
}

/// Serve the agent on stdio, forwarding to `upstream`.
///
/// # Errors
/// The server does not start, or the stdio transport fails.
pub async fn serve(
    server: String,
    allow: Vec<String>,
    cache: Vec<CacheRule>,
    upstream: Upstream,
) -> anyhow::Result<()> {
    let upstream_service = connect(&upstream).await?;
    // part of each cache key: the command (for a package, its store version) or the URL
    let command = match upstream {
        Upstream::Command { argv, .. } => argv,
        Upstream::Url { url, .. } => vec![url],
    };
    let proxy = Proxy {
        server,
        command,
        allow,
        cache,
        upstream: upstream_service,
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
