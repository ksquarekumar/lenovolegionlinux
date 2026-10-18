//! `legionctl mcp-proxy`: the only way a session's agent reaches an MCP server (note 14). It starts the server as its
//! child, offers the agent only the allowed tools, refuses the others, and records every call in the operator's call
//! record before it runs (`internal.calls.begin`, which checks the tool's toolset, then `internal.calls.end`): session,
//! server, tool, a hash of the arguments, duration, result size and outcome; a retry with the same call id gets the
//! recorded result. Tools its config lets it cache are answered from the operator's cache while the result is valid (T2).

use blake2::digest::consts::U16;
use blake2::{Blake2b, Digest};
use legionctl_api::client::Client;
use legionctl_api::{
    CacheGetIn, CachePutIn, CachedCall, CallBeginIn, CallEndIn, CallStart, ClientPath, Outcome, op,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt};

/// The `_meta` key of a tool call with its call id (legionctl's MCP server sends it for a retry).
pub const CALL_META: &str = "legionctl/call";

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
    /// The cache key's parts of a call of `tool` that its rule caches: the operator builds the key from the call, the
    /// working copy's git state and this session.
    fn cached_call(
        &self,
        tool: &str,
        request: &CallToolRequestParams,
    ) -> Result<Option<CachedCall>, ErrorData> {
        let Some(rule) = self.cache.iter().find(|r| r.tool == tool) else {
            return Ok(None);
        };
        let failed = |what: String| ErrorData::internal_error(format!("mcp-proxy: {what}"), None);
        Ok(Some(CachedCall {
            server: self.command.join(" "),
            tool: tool.into(),
            args: serde_json::to_value(&request.arguments)
                .map_err(|e| failed(format!("the arguments of {tool}: {e}")))?,
            workspace: if rule.workspace {
                Some(ClientPath(std::env::current_dir().map_err(|e| {
                    failed(format!(
                        "cannot read the working folder for {tool}'s workspace cache key: {e}"
                    ))
                })?))
            } else {
                None
            },
        }))
    }

    /// The kept result of `call`. The operator reports a cache that it cannot read and answers a miss (user decision
    /// Q2); an operator that does not answer is said here, and the call goes upstream.
    async fn cached(&self, tool: &str, call: CachedCall) -> Option<String> {
        match self.client.call::<op::CacheGet>(&CacheGetIn { call }).await {
            Ok(hit) => hit.value,
            Err(e) => {
                eprintln!("mcp-proxy: no cache read for {tool}, the call goes upstream: {e:#}");
                None
            }
        }
    }

    fn allowed(&self, tool: &str) -> bool {
        self.allow.is_empty() || self.allow.iter().any(|a| a == tool)
    }

    /// Start the call in the operator's call record (its toolset is checked there): its id, or the recorded result of
    /// a call id that ended before. A call that cannot be recorded does not run.
    async fn begin(
        &self,
        (request, meta): (&CallToolRequestParams, &rmcp::model::RequestMetaObject),
        args_hash: &str,
        refused: bool,
    ) -> Result<CallStart, ErrorData> {
        // rmcp gives the call's _meta in its request context
        let call = meta
            .0
            .0
            .get(CALL_META)
            .and_then(serde_json::Value::as_str)
            .map(String::from);
        self.client
            .call::<op::CallsBegin>(&CallBeginIn {
                call,
                session: legionctl_api::SessionId(self.session.clone()),
                server: self.server.clone(),
                tool: request.name.to_string(),
                args_hash: args_hash.to_string(),
                refused,
            })
            .await
            .map_err(|e| ErrorData::invalid_request(format!("{e:#}"), None))
    }

    /// End call `call`; its result is kept for a retry. A failed write goes to stderr: the call ran.
    async fn end(&self, call: &str, outcome: Outcome, result: Option<&CallToolResult>) {
        let output = result.and_then(|r| serde_json::to_value(r).ok());
        let end = CallEndIn {
            call: call.to_string(),
            outcome,
            bytes: result.map_or(0, |r| u64::try_from(hit_size(r)).unwrap_or(u64::MAX)),
            output,
        };
        if let Err(e) = self.client.call::<op::CallsEnd>(&end).await {
            eprintln!("legionctl mcp-proxy: the end of call {call} is not recorded: {e:#}");
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool = request.name.to_string();
        let args = serde_json::to_vec(&request.arguments).unwrap_or_default();
        let args_hash = format!("{:x}", Blake2b::<U16>::digest(&args));
        if !self.allowed(&tool) {
            self.begin((&request, &context.meta), &args_hash, true)
                .await?;
            return Err(ErrorData::invalid_params(
                format!(
                    "{tool} is not allowed for {} in this session (mcp.servers.{}.allow)",
                    self.server, self.server
                ),
                None,
            ));
        }
        let start = self
            .begin((&request, &context.meta), &args_hash, false)
            .await?;
        if let Some(replay) = start.replay {
            return serde_json::from_value::<CallToolResult>(replay)
                .map(Into::into)
                .map_err(|e| {
                    ErrorData::internal_error(format!("the recorded result of {tool}: {e}"), None)
                });
        }
        let call = self.cached_call(&tool, &request)?;
        let hit = match &call {
            Some(call) => self.cached(&tool, call.clone()).await,
            None => None,
        };
        if let Some(value) = hit {
            let result = serde_json::from_str::<CallToolResult>(&value).map_err(|e| {
                ErrorData::internal_error(
                    format!("mcp-proxy: the cached result of {tool} does not parse: {e}"),
                    None,
                )
            })?;
            self.end(&start.call, Outcome::Cached, Some(&result)).await;
            return Ok(result.into());
        }
        match self.upstream.peer().call_tool(request).await {
            Ok(result) => {
                let outcome = if result.is_error == Some(true) {
                    Outcome::Error
                } else {
                    Outcome::Ok
                };
                let rule = self.cache.iter().find(|r| r.tool == tool);
                if let (Some(call), Some(rule), Outcome::Ok) = (call, rule, outcome) {
                    let put = CachePutIn {
                        call,
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
                self.end(&start.call, outcome, Some(&result)).await;
                Ok(result.into())
            }
            Err(e) => {
                self.end(&start.call, Outcome::Failed, None).await;
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
        // a proxy that the user runs has no session: its calls are the user's (the audit row has the caller)
        session: std::env::var("LEGIONCTL_SESSION").unwrap_or_default(),
    };
    proxy
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
