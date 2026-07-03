//! Integration tests against a real MCP server subprocess.
//!
//! Approach: rather than hand-rolling raw JSON-RPC frames (fragile, easy to
//! drift from the real wire format) or depending on an external MCP server
//! being installed on the test machine, this crate ships its own minimal
//! MCP server (`src/bin/toy_mcp_server.rs`) built on the same `rmcp` crate
//! this library uses, guaranteeing wire compatibility. Cargo sets
//! `CARGO_BIN_EXE_toy_mcp_server` to its compiled path for every
//! integration test in this package, so `McpToolProvider::connect` spawns
//! and talks to a *real* separate process over stdio here — this is not a
//! mock of the transport.

use braze_mcp_client::McpToolProvider;
use braze_tools_core::{ToolError, ToolProvider};
use braze_types::ToolCall;

fn toy_server_path() -> String {
    env!("CARGO_BIN_EXE_toy_mcp_server").to_string()
}

async fn connect() -> McpToolProvider {
    McpToolProvider::connect("toy".to_string(), toy_server_path(), Vec::new())
        .await
        .expect("toy MCP server should spawn and complete the handshake")
}

fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    }
}

#[tokio::test]
async fn provider_id_is_namespaced_by_server_name() {
    let provider = connect().await;
    assert_eq!(provider.provider_id(), "mcp:toy");
}

#[tokio::test]
async fn list_stubs_reflects_every_tool_the_server_advertises() {
    let provider = connect().await;
    let stubs = provider
        .list_stubs()
        .await
        .expect("list_stubs should succeed");

    let names: Vec<&str> = stubs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "add", "fail", "verbose"]);

    for stub in &stubs {
        assert_eq!(stub.source, "mcp:toy");
    }
}

#[tokio::test]
async fn list_stubs_summary_matches_the_documented_truncation_criteria() {
    let provider = connect().await;
    let stubs = provider
        .list_stubs()
        .await
        .expect("list_stubs should succeed");

    let echo = stubs.iter().find(|s| s.name == "echo").expect("echo stub");
    assert_eq!(
        echo.summary,
        "Echoes back the exact text it was given, for round-trip testing."
    );

    let verbose = stubs
        .iter()
        .find(|s| s.name == "verbose")
        .expect("verbose stub");
    // Long, no early sentence boundary in the source description -> must be
    // word-boundary truncated with a trailing ellipsis (see summary.rs).
    assert!(verbose.summary.ends_with('…'));
    assert!(verbose.summary.chars().count() <= 161);
}

#[tokio::test]
async fn resolve_schema_returns_the_real_json_schema_for_a_known_tool() {
    let provider = connect().await;
    let schema = provider
        .resolve_schema("echo")
        .await
        .expect("resolve_schema should succeed")
        .expect("echo is a real tool the server advertised");

    assert_eq!(schema.name, "echo");
    assert!(schema.description.contains("Echoes back"));
    assert_eq!(schema.input_schema["properties"]["text"]["type"], "string");
    assert_eq!(schema.input_schema["required"][0], "text");
}

#[tokio::test]
async fn resolve_schema_returns_none_for_an_unknown_tool() {
    let provider = connect().await;
    let schema = provider
        .resolve_schema("does_not_exist")
        .await
        .expect("resolve_schema should succeed even for an unknown tool");
    assert!(schema.is_none());
}

#[tokio::test]
async fn resolve_schema_works_without_a_prior_list_stubs_call() {
    // Exercises the cache-miss self-heal path documented on
    // `McpToolProvider::tool_cache`: no `list_stubs` has run yet, so the
    // cache starts empty and `resolve_schema` must fetch on its own.
    let provider = connect().await;
    let schema = provider
        .resolve_schema("add")
        .await
        .expect("resolve_schema should succeed")
        .expect("add is a real tool");
    assert_eq!(schema.name, "add");
}

#[tokio::test]
async fn invoke_echo_round_trips_the_text_argument() {
    let provider = connect().await;
    let call = tool_call("call-1", "echo", serde_json::json!({"text": "hello mcp"}));
    let result = provider.invoke(&call).await.expect("invoke should succeed");

    assert_eq!(result.tool_call_id, "call-1");
    assert_eq!(result.content, "hello mcp");
    assert!(!result.is_error);
}

#[tokio::test]
async fn invoke_add_computes_the_sum() {
    let provider = connect().await;
    let call = tool_call("call-2", "add", serde_json::json!({"a": 2, "b": 3}));
    let result = provider.invoke(&call).await.expect("invoke should succeed");

    assert_eq!(result.content, "5");
    assert!(!result.is_error);
}

#[tokio::test]
async fn invoke_fail_maps_the_mcp_tool_level_error_flag_to_is_error() {
    let provider = connect().await;
    let call = tool_call("call-3", "fail", serde_json::json!({}));
    let result = provider.invoke(&call).await.expect("invoke should succeed");

    assert!(result.is_error);
    assert!(result.content.contains("intentional failure"));
}

#[tokio::test]
async fn invoke_rejects_non_object_arguments_without_touching_the_server() {
    let provider = connect().await;
    let call = tool_call("call-4", "echo", serde_json::json!("not an object"));
    let err = provider
        .invoke(&call)
        .await
        .expect_err("non-object arguments must be rejected");

    match err {
        ToolError::InvocationFailed { name, message } => {
            assert_eq!(name, "echo");
            assert!(message.contains("JSON object"));
        }
        other => panic!("expected InvocationFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn connect_to_a_nonexistent_command_fails_with_provider_unavailable() {
    let err = McpToolProvider::connect(
        "broken".to_string(),
        "this-binary-does-not-exist-12345".to_string(),
        Vec::new(),
    )
    .await
    .expect_err("spawning a nonexistent binary must fail, not hang");

    assert!(matches!(err, ToolError::ProviderUnavailable(_)));
}
