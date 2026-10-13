//! MCP server over stdio: one tool per operation of the registry (`workspace.open` is `workspace_open`), each a
//! call to `legionctl-operator`. Machine changes need a human's approval in the polkit dialog on this machine, so an
//! agent can propose them but never apply them silently.

use std::sync::Arc;

use legionctl_api::{OPS, OpInfo};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;

use crate::cli::fill_client_fields;
use legionctl_api::client::Client;

fn tool_name(op: &OpInfo) -> String {
    op.name.replace(['.', '-'], "_")
}

struct Legion {
    client: Client,
}

impl Legion {
    async fn run(
        &self,
        op: &OpInfo,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> anyhow::Result<String> {
        let mut input = Value::Object(arguments.unwrap_or_default());
        fill_client_fields(op, &mut input, false)?;
        let output = self.client.call_value(op.name, &input).await?;
        Ok(serde_json::to_string_pretty(&output)?)
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
        let tools = OPS
            .iter()
            .filter(|op| !op.internal())
            .map(|op| {
                let schema = match (op.input)() {
                    Value::Object(schema) => schema,
                    _ => rmcp::model::JsonObject::new(),
                };
                Tool::new(tool_name(op), op.text(), Arc::new(schema))
            })
            .collect();
        std::future::ready(Ok(ListToolsResult::with_all_items(tools)))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(op) = OPS
            .iter()
            .find(|op| !op.internal() && tool_name(op) == request.name)
        else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {}", request.name),
                None,
            ));
        };
        Ok(match self.run(op, request.arguments).await {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            // the same envelope as the socket: {"error": {code, message, op}}
            Err(e) => CallToolResult::error(vec![ContentBlock::text(
                serde_json::json!({"error": e.downcast_ref::<legionctl_api::ApiError>().cloned().unwrap_or(
                    legionctl_api::ApiError {
                        code: legionctl_api::code_of(&e),
                        message: format!("{e:#}"),
                        op: op.name.to_string(),
                    }
                )})
                .to_string(),
            )]),
        }
        .into())
    }
}

pub async fn serve() -> anyhow::Result<()> {
    Legion {
        client: Client::new("mcp")?,
    }
    .serve(rmcp::transport::stdio())
    .await?
    .waiting()
    .await?;
    Ok(())
}
