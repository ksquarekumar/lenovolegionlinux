//! MCP server over stdio with three tools over the operation registry: `legionctl_find` (operations by words or
//! group), `legionctl_describe` (one operation's help and input schema) and `legionctl_run` (a call to
//! `legionctl-operator`). The operator's class gates apply to each run: machine changes need a human's approval in the
//! polkit dialog on this machine, so an agent can propose them but never apply them silently.

use std::collections::BTreeMap;
use std::sync::Arc;

use legionctl_api::{ApiError, Class, Code, OPS, OpInfo};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

use legionctl_api::cli::fill_client_fields;
use legionctl_api::client::Client;

/// Rows of `legionctl_find` when the caller gives no `limit`.
const FIND_LIMIT: usize = 20;

struct Legion {
    client: Client,
    /// The operations offered: the public ones, or the bound ones of `--only`.
    ops: Vec<&'static OpInfo>,
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

/// The words of an operation name: `workspace.checkpoint`, `workspace_checkpoint` and `workspace checkpoint` have the
/// same two words.
fn name_words(name: &str) -> Vec<String> {
    name.split(['.', '_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

impl Legion {
    fn op(&self, name: &str) -> Result<&'static OpInfo, ApiError> {
        self.ops
            .iter()
            .copied()
            .find(|op| op.name == name)
            .ok_or_else(|| ApiError {
                code: Code::NotFound,
                message: self.unknown(name),
                op: String::new(),
            })
    }

    /// One sentence for a name that is not an offered operation, with the closest names.
    fn unknown(&self, name: &str) -> String {
        let words = name_words(name);
        let mut close: Vec<(usize, &str)> = self
            .ops
            .iter()
            .map(|op| {
                let shared = name_words(op.name)
                    .iter()
                    .filter(|w| words.contains(w))
                    .count();
                (shared, op.name)
            })
            .filter(|(shared, _)| *shared > 0)
            .collect();
        close.sort_by_key(|&(shared, name)| (std::cmp::Reverse(shared), name));
        let close: Vec<&str> = close.iter().take(3).map(|(_, name)| *name).collect();
        if close.is_empty() {
            format!("No operation {name}. Use legionctl_find to find one.")
        } else {
            format!("No operation {name}. The closest: {}.", close.join(", "))
        }
    }

    fn find(&self, args: &Value) -> String {
        let words: Vec<String> = args["words"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let group = args["group"].as_str();
        if words.is_empty() && group.is_none() {
            let mut groups: BTreeMap<&str, usize> = BTreeMap::new();
            for op in &self.ops {
                *groups.entry(group_of(op)).or_default() += 1;
            }
            let rows: Vec<String> = groups.iter().map(|(g, n)| format!("{g} {n}")).collect();
            return format!(
                "{} operations in {} groups (group, count). Give words, a group or both.\n{}",
                self.ops.len(),
                groups.len(),
                rows.join("\n")
            );
        }
        let limit = args["limit"]
            .as_u64()
            .and_then(|l| usize::try_from(l).ok())
            .unwrap_or(FIND_LIMIT);
        // a word in the name counts more than a word in the help
        let mut hits: Vec<(usize, &OpInfo)> = self
            .ops
            .iter()
            .copied()
            .filter(|op| group.is_none_or(|g| g == group_of(op)))
            .map(|op| {
                let (name, help) = (op.name.to_lowercase(), op.text().to_lowercase());
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
                (score, op)
            })
            .filter(|(score, _)| words.is_empty() || *score > 0)
            .collect();
        hits.sort_by_key(|&(score, op)| (std::cmp::Reverse(score), op.name));
        let shown: Vec<String> = hits
            .iter()
            .take(limit)
            .map(|(_, op)| format!("{} · {} · {}", op.name, class_name(op.class), op.summary()))
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
        std::iter::once(head)
            .chain(shown)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn describe(op: &OpInfo) -> String {
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
        format!(
            "{} · {}. {gate}{asks}\n\n{}\n\nInput: {schema}\nOutput: {}",
            op.name,
            class_name(op.class),
            op.text(),
            (op.output)()["title"].as_str().unwrap_or("JSON")
        )
    }

    async fn run(&self, args: &Value) -> Result<String, ApiError> {
        let op = self.op(args["op"].as_str().unwrap_or_default())?;
        let fail = |e: anyhow::Error| {
            e.downcast_ref::<ApiError>().cloned().unwrap_or(ApiError {
                code: legionctl_api::code_of(&e),
                message: format!("{e:#}"),
                op: op.name.to_string(),
            })
        };
        let mut input = match &args["input"] {
            Value::Null => json!({}),
            input => input.clone(),
        };
        fill_client_fields(op, &mut input, false).map_err(fail)?;
        let output = self
            .client
            .call_value(op.name, &input)
            .await
            .map_err(fail)?;
        serde_json::to_string_pretty(&output).map_err(|e| fail(e.into()))
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
        let op = json!({"type": "string", "description": "An operation name from legionctl_find (workspace.checkpoint)."});
        let tools = vec![
            tool(
                "legionctl_find",
                "Find legionctl operations (machine, dossiers, workspaces, sessions, threads, code). Give words, a \
                 group or both; with no input you get the groups and their counts. Each row: name · class · what it \
                 does.",
                json!({"type": "object", "properties": {
                    "words": {"type": "string", "description": "Words to find in the name or help (fan curve)."},
                    "group": {"type": "string", "description": "Only this group (the first word of a name: fan)."},
                    "limit": {"type": "integer", "minimum": 1, "description": "The most rows; default 20."}
                }}),
            ),
            tool(
                "legionctl_describe",
                "Show one operation: its help, its input JSON schema, its output shape and its class (what it can \
                 change and who approves it).",
                json!({"type": "object", "properties": {"op": op}, "required": ["op"]}),
            ),
            tool(
                "legionctl_run",
                "Run one operation with its input (the fields of legionctl_describe). You get the operation's JSON \
                 output, or {\"error\": {code, message, op}}. A workspace or machine operation waits for the user's \
                 approval.",
                json!({"type": "object", "properties": {
                    "op": op,
                    "input": {"type": "object", "description": "The operation's input; default {}."}
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
        let answer = match request.name.as_ref() {
            "legionctl_find" => Ok(self.find(&args)),
            "legionctl_describe" => self
                .op(args["op"].as_str().unwrap_or_default())
                .map(Legion::describe),
            "legionctl_run" => self.run(&args).await,
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool {other}"),
                    None,
                ));
            }
        };
        Ok(match answer {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            // the same envelope as the socket: {"error": {code, message, op}}
            Err(e) => {
                CallToolResult::error(vec![ContentBlock::text(json!({"error": e}).to_string())])
            }
        }
        .into())
    }
}

/// Serve the public operations, or only those named in `only`.
///
/// # Errors
/// A name of `only` is not a public operation, or the transport fails.
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
    Legion {
        client: Client::new("mcp")?,
        ops,
    }
    .serve(rmcp::transport::stdio())
    .await?
    .waiting()
    .await?;
    Ok(())
}
