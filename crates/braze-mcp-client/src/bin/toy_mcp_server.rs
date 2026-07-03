//! Minimal MCP server used only by this crate's own integration tests (see
//! `tests/mcp_toy_server.rs`), spawned as a real stdio subprocess by
//! [`braze_mcp_client::McpToolProvider::connect`] — reused instead of
//! depending on an external MCP server binary being installed on the test
//! machine. Not part of `braze-mcp-client`'s public API: it links against
//! `rmcp`'s server-side surface (`ServerHandler`), which the library itself
//! never uses (see the Cargo.toml comment next to the `rmcp` dependency for
//! why that surface is still a normal, not dev, dependency feature).
//!
//! Exposes four fixed tools chosen to exercise `McpToolProvider` end to
//! end: a happy-path round-trip (`echo`), a two-argument computation
//! (`add`), a deliberate tool-level failure (`fail`, to exercise
//! `ToolResult.is_error`), and a tool whose description is long enough
//! with no early sentence boundary to exercise the word-boundary+ellipsis
//! branch of `summary::summarize` end to end against a real server
//! (`verbose`).

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ListToolsResult, PaginatedRequestParams,
    Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Map, Value};

/// Deliberately > 160 chars on a single line with no `.`/`!`/`?` before the
/// limit, so `summary::summarize` must fall back to its word-boundary +
/// ellipsis branch rather than cutting at a sentence end.
const VERBOSE_DESCRIPTION: &str = "This tool exists purely to exercise the ToolStub summary \
    truncation logic implemented in braze-mcp-client by providing a description whose first \
    line is deliberately longer than the configured maximum summary length so the word \
    boundary truncation branch gets exercised end to end without ever hitting a period";

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("toy_mcp_server: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let service = ToyServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

struct ToyServer;

impl ServerHandler for ToyServer {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(all_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        Ok(dispatch(&request))
    }
}

fn all_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "echo",
            "Echoes back the exact text it was given, for round-trip testing.",
            object_schema(&[("text", type_schema("string"))], &["text"]),
        ),
        Tool::new(
            "add",
            "Adds two integers, `a` and `b`, and returns their sum as text.",
            object_schema(
                &[("a", type_schema("integer")), ("b", type_schema("integer"))],
                &["a", "b"],
            ),
        ),
        Tool::new(
            "fail",
            "Always returns a tool-level error result, for testing ToolResult.is_error mapping.",
            object_schema(&[], &[]),
        ),
        Tool::new("verbose", VERBOSE_DESCRIPTION, object_schema(&[], &[])),
    ]
}

fn dispatch(request: &CallToolRequestParams) -> CallToolResult {
    match request.name.as_ref() {
        "echo" => {
            let text = string_arg(request, "text").unwrap_or_default();
            CallToolResult::success(vec![ContentBlock::text(text)])
        }
        "add" => match (int_arg(request, "a"), int_arg(request, "b")) {
            (Some(a), Some(b)) => {
                CallToolResult::success(vec![ContentBlock::text((a + b).to_string())])
            }
            _ => CallToolResult::error(vec![ContentBlock::text(
                "missing or non-integer 'a'/'b' argument",
            )]),
        },
        "fail" => {
            CallToolResult::error(vec![ContentBlock::text("intentional failure for testing")])
        }
        "verbose" => CallToolResult::success(vec![ContentBlock::text("verbose tool invoked")]),
        other => CallToolResult::error(vec![ContentBlock::text(format!("unknown tool: {other}"))]),
    }
}

fn string_arg(request: &CallToolRequestParams, key: &str) -> Option<String> {
    request
        .arguments
        .as_ref()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

fn int_arg(request: &CallToolRequestParams, key: &str) -> Option<i64> {
    request.arguments.as_ref()?.get(key)?.as_i64()
}

fn type_schema(json_type: &str) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String(json_type.to_string()));
    Value::Object(schema)
}

fn object_schema(properties: &[(&str, Value)], required: &[&str]) -> Map<String, Value> {
    let mut props = Map::new();
    for (key, schema) in properties {
        props.insert((*key).to_string(), schema.clone());
    }
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(props));
    if !required.is_empty() {
        schema.insert(
            "required".to_string(),
            Value::Array(
                required
                    .iter()
                    .map(|s| Value::String((*s).to_string()))
                    .collect(),
            ),
        );
    }
    schema
}
