//! MCP server over stdio with three tools (DESIGN/22 rule 1): `legionctl_find` (operations and brokered tools by
//! words or group), `legionctl_describe` (one of them: help, input schema, class) and `legionctl_run` (a call to
//! `legionctl-operator`, or to a brokered server through its `legionctl mcp-proxy`). The operator's class gates apply
//! to each run: machine changes need a human's approval in the polkit dialog on this machine, so an agent can propose
//! them but never apply them silently.

use std::collections::BTreeMap;
use std::sync::Arc;

use legionctl_api::{ApiError, Class, Code, OPS, OpInfo};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

use legionctl_api::cli::fill_client_fields;
use legionctl_api::client::Client;
use legionctl_core::session::BROKER_ENV;

/// Rows of `legionctl_find` when the caller gives no `limit`.
const FIND_LIMIT: usize = 20;

/// A server that `legionctl mcp` brokers (`mcp.servers`): its proxy and the tools the proxy offers.
struct Brokered {
    server: String,
    tools: Vec<Tool>,
    proxy: RunningService<RoleClient, ()>,
}

/// The brokered servers that started, and one sentence for each that did not.
type Started = (Vec<Brokered>, Vec<String>);

struct Legion {
    client: Client,
    /// The operations offered: the public ones, or the bound ones of `--only`.
    ops: Vec<&'static OpInfo>,
    /// The servers to broker: name -> `{args, env}` of its `legionctl mcp-proxy` ([`BROKER_ENV`]).
    broker: serde_json::Map<String, Value>,
    /// The proxies, started at the first find, describe or run that needs them.
    started: tokio::sync::OnceCell<Started>,
}

/// One row of `legionctl_find`: an operation or a brokered tool.
struct Row {
    name: String,
    group: String,
    class: String,
    summary: String,
    help: String,
}

fn tool(name: &str, description: &str, schema: Value) -> Tool {
    let Value::Object(schema) = schema else {
        unreachable!("a tool schema is an object")
    };
    Tool::new(name.to_string(), description.to_string(), Arc::new(schema))
}

fn class_name(class: Class) -> String {
    format!("{class:?}").to_lowercase()
}

fn group_of(op: &OpInfo) -> &'static str {
    op.name.split('.').next().unwrap_or(op.name)
}

fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    text.split_once(". ").map_or(text, |(first, _)| first)
}

/// The words of a name: `workspace.checkpoint`, `workspace_checkpoint` and `workspace checkpoint` have the same two
/// words.
fn name_words(name: &str) -> Vec<String> {
    name.split(['.', '_', '-', ' ', '/'])
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn failed(code: Code, message: String, name: &str) -> ApiError {
    ApiError {
        code,
        message,
        op: name.to_string(),
    }
}

/// Start the proxy of `server` as this binary's `mcp-proxy`, and list the tools it offers.
async fn start(server: &str, entry: &Value) -> anyhow::Result<Brokered> {
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(String::from))
            .collect()
    };
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    // the broker list holds every server's keyring headers: no proxy, and no server it starts, may see it
    command.args(strings(&entry["args"])).env_remove(BROKER_ENV);
    for (name, value) in entry["env"].as_object().into_iter().flatten() {
        command.env(name, value.as_str().unwrap_or_default());
    }
    let proxy = ().serve(TokioChildProcess::new(command)?).await?;
    let tools = proxy.peer().list_all_tools().await?;
    Ok(Brokered {
        server: server.to_string(),
        tools,
        proxy,
    })
}

impl Legion {
    async fn brokered(&self) -> &Started {
        self.started
            .get_or_init(|| async {
                let mut started = (Vec::new(), Vec::new());
                for (server, entry) in &self.broker {
                    match start(server, entry).await {
                        Ok(b) => started.0.push(b),
                        Err(e) => started
                            .1
                            .push(format!("MCP server {server} did not start: {e:#}.")),
                    }
                }
                started
            })
            .await
    }

    /// The brokered tool `<server>/<tool>`, with its server.
    async fn brokered_tool(&self, name: &str) -> Option<(&Brokered, &Tool)> {
        let (server, tool) = name.split_once('/')?;
        let b = self
            .brokered()
            .await
            .0
            .iter()
            .find(|b| b.server == server)?;
        Some((b, b.tools.iter().find(|t| t.name == tool)?))
    }

    async fn rows(&self) -> Vec<Row> {
        let ops = self.ops.iter().map(|op| Row {
            name: op.name.to_string(),
            group: group_of(op).to_string(),
            class: class_name(op.class),
            summary: op.summary().to_string(),
            help: op.text().to_string(),
        });
        let brokered = self.brokered().await.0.iter().flat_map(|b| {
            b.tools.iter().map(|t| {
                let help = t.description.as_deref().unwrap_or_default();
                Row {
                    name: format!("{}/{}", b.server, t.name),
                    group: b.server.clone(),
                    class: "mcp".into(),
                    summary: first_sentence(help).to_string(),
                    help: help.to_string(),
                }
            })
        });
        ops.chain(brokered).collect()
    }

    /// One sentence for a name that is not offered, with the closest names.
    async fn unknown(&self, name: &str) -> ApiError {
        let words = name_words(name);
        let mut close: Vec<(usize, String)> = self
            .rows()
            .await
            .into_iter()
            .map(|row| {
                let shared = name_words(&row.name)
                    .iter()
                    .filter(|w| words.contains(w))
                    .count();
                (shared, row.name)
            })
            .filter(|(shared, _)| *shared > 0)
            .collect();
        close.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let close: Vec<String> = close.into_iter().take(3).map(|(_, name)| name).collect();
        let message = if close.is_empty() {
            format!("No operation {name}. Use legionctl_find to find one.")
        } else {
            format!("No operation {name}. The closest: {}.", close.join(", "))
        };
        failed(Code::NotFound, message, "")
    }

    fn op(&self, name: &str) -> Option<&'static OpInfo> {
        self.ops.iter().copied().find(|op| op.name == name)
    }

    async fn find(&self, args: &Value) -> String {
        let words: Vec<String> = args["words"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let group = args["group"].as_str();
        let rows = self.rows().await;
        let not_started = self.brokered().await.1.join("\n");
        let with_failures = |text: String| {
            if not_started.is_empty() {
                text
            } else {
                format!("{text}\n{not_started}")
            }
        };
        if words.is_empty() && group.is_none() {
            let mut groups: BTreeMap<&str, usize> = BTreeMap::new();
            for row in &rows {
                *groups.entry(&row.group).or_default() += 1;
            }
            let lines: Vec<String> = groups.iter().map(|(g, n)| format!("{g} {n}")).collect();
            return with_failures(format!(
                "{} operations in {} groups (group, count). Give words, a group or both.\n{}",
                rows.len(),
                groups.len(),
                lines.join("\n")
            ));
        }
        let limit = args["limit"]
            .as_u64()
            .and_then(|l| usize::try_from(l).ok())
            .unwrap_or(FIND_LIMIT);
        // a word in the name counts more than a word in the help
        let mut hits: Vec<(usize, &Row)> = rows
            .iter()
            .filter(|row| group.is_none_or(|g| g == row.group))
            .map(|row| {
                let (name, help) = (row.name.to_lowercase(), row.help.to_lowercase());
                let score = words
                    .iter()
                    .map(|w| {
                        if name.contains(w.as_str()) {
                            2
                        } else {
                            usize::from(help.contains(w.as_str()))
                        }
                    })
                    .sum();
                (score, row)
            })
            .filter(|(score, _)| words.is_empty() || *score > 0)
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        let shown: Vec<String> = hits
            .iter()
            .take(limit)
            .map(|(_, row)| format!("{} · {} · {}", row.name, row.class, row.summary))
            .collect();
        let left = hits.len() - shown.len();
        let head = if left == 0 {
            format!("{} operations match.", hits.len())
        } else {
            format!(
                "{} operations match; {left} left out. Give more words or a higher limit.",
                hits.len()
            )
        };
        with_failures(
            std::iter::once(head)
                .chain(shown)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    async fn describe(&self, name: &str) -> Result<String, ApiError> {
        if let Some((b, t)) = self.brokered_tool(name).await {
            return Ok(format!(
                "{name} · mcp. A tool of the MCP server {} (mcp.servers.{}): legionctl logs each call.\n\n{}\n\n\
                 Input: {}",
                b.server,
                b.server,
                t.description.as_deref().unwrap_or_default(),
                Value::Object((*t.input_schema).clone())
            ));
        }
        let Some(op) = self.op(name) else {
            return Err(self.unknown(name).await);
        };
        let schema = (op.input)();
        let gate = match op.class {
            Class::Read => "It reads only.",
            Class::Session => "It changes only your own session.",
            Class::Workspace => {
                "It changes your files, settings or other sessions: when an agent runs it, the user approves it first."
            }
            Class::Machine => "It changes the machine: the user approves it in the polkit dialog.",
        };
        let asks: Vec<&str> = schema["properties"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, prop)| prop["x-approval"] == true)
            .map(|(field, _)| field.as_str())
            .collect();
        let asks = if asks.is_empty() {
            String::new()
        } else {
            format!(
                " When {} is true, the user approves it first.",
                asks.join(", ")
            )
        };
        Ok(format!(
            "{} · {}. {gate}{asks}\n\n{}\n\nInput: {schema}\nOutput: {}",
            op.name,
            class_name(op.class),
            op.text(),
            (op.output)()["title"].as_str().unwrap_or("JSON")
        ))
    }

    async fn run(&self, args: &Value) -> Result<CallToolResult, ApiError> {
        let name = args["op"].as_str().unwrap_or_default();
        let mut input = match &args["input"] {
            Value::Null => json!({}),
            input => input.clone(),
        };
        if let Some((b, t)) = self.brokered_tool(name).await {
            let Value::Object(arguments) = input else {
                return Err(failed(
                    Code::InvalidInput,
                    format!("{name}: input must be an object"),
                    name,
                ));
            };
            let request = CallToolRequestParams::new(t.name.clone()).with_arguments(arguments);
            return b
                .proxy
                .peer()
                .call_tool(request)
                .await
                .map_err(|e| failed(Code::Failed, format!("{e:#}"), name));
        }
        let Some(op) = self.op(name) else {
            return Err(self.unknown(name).await);
        };
        let fail = |e: anyhow::Error| {
            e.downcast_ref::<ApiError>()
                .cloned()
                .unwrap_or_else(|| failed(legionctl_api::code_of(&e), format!("{e:#}"), op.name))
        };
        fill_client_fields(op, &mut input, false).map_err(fail)?;
        let output = self
            .client
            .call_value(op.name, &input)
            .await
            .map_err(fail)?;
        let text = serde_json::to_string_pretty(&output).map_err(|e| fail(e.into()))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

impl ServerHandler for Legion {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        let op = json!({"type": "string", "description": "A name from legionctl_find (workspace.checkpoint, or <server>/<tool>)."});
        let tools = vec![
            tool(
                "legionctl_find",
                "Find legionctl operations (machine, dossiers, workspaces, sessions, threads, code) and the tools of \
                 the MCP servers legionctl brokers. Give words, a group or both; with no input you get the groups and \
                 their counts. Each row: name · class · what it does.",
                json!({"type": "object", "properties": {
                    "words": {"type": "string", "description": "Words to find in the name or help (fan curve)."},
                    "group": {"type": "string", "description": "Only this group (the first word of a name: fan; or a server)."},
                    "limit": {"type": "integer", "minimum": 1, "description": "The most rows; default 20."}
                }}),
            ),
            tool(
                "legionctl_describe",
                "Show one operation or tool: its help, its input JSON schema, its output shape and its class (what it \
                 can change and who approves it).",
                json!({"type": "object", "properties": {"op": op}, "required": ["op"]}),
            ),
            tool(
                "legionctl_run",
                "Run one operation or tool with its input (the fields of legionctl_describe). You get its output, or \
                 {\"error\": {code, message, op}}. A workspace or machine operation waits for the user's approval.",
                json!({"type": "object", "properties": {
                    "op": op,
                    "input": {"type": "object", "description": "The input; default {}."}
                }, "required": ["op"]}),
            ),
        ];
        std::future::ready(Ok(ListToolsResult::with_all_items(tools)))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        let text = |text: String| CallToolResult::success(vec![ContentBlock::text(text)]);
        let answer = match request.name.as_ref() {
            "legionctl_find" => Ok(text(self.find(&args).await)),
            "legionctl_describe" => self
                .describe(args["op"].as_str().unwrap_or_default())
                .await
                .map(text),
            "legionctl_run" => self.run(&args).await,
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool {other}"),
                    None,
                ));
            }
        };
        // the same envelope as the socket: {"error": {code, message, op}}
        Ok(answer
            .unwrap_or_else(|e| {
                CallToolResult::error(vec![ContentBlock::text(json!({"error": e}).to_string())])
            })
            .into())
    }
}

/// Serve the public operations, or only those named in `only`, and broker the servers of [`BROKER_ENV`].
///
/// # Errors
/// A name of `only` is not a public operation, the broker list does not parse, or the transport fails.
pub async fn serve(only: &[String]) -> anyhow::Result<()> {
    let public = || OPS.iter().filter(|op| !op.internal());
    let ops = if only.is_empty() {
        public().collect()
    } else {
        only.iter()
            .map(|name| {
                public()
                    .find(|op| op.name == name)
                    .ok_or_else(|| anyhow::anyhow!("--only {name}: no such public operation"))
            })
            .collect::<anyhow::Result<_>>()?
    };
    let broker = match std::env::var(BROKER_ENV) {
        Ok(json) => serde_json::from_str(&json)
            .map_err(|e| anyhow::anyhow!("{BROKER_ENV} is not a JSON object of servers: {e}"))?,
        Err(_) => serde_json::Map::new(),
    };
    Legion {
        client: Client::new("mcp")?,
        ops,
        broker,
        started: tokio::sync::OnceCell::new(),
    }
    .serve(rmcp::transport::stdio())
    .await?
    .waiting()
    .await?;
    Ok(())
}
