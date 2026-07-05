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

use std::time::Duration;

use braze_mcp_client::McpToolProvider;
use braze_permissions::{
    ActionDescriptor, ConfirmationPrompt, DefaultClassifier, PermissionGuard, WorkdirAllowlist,
};
use braze_tools_core::{ToolError, ToolProvider};
use braze_types::ToolCall;

fn toy_server_path() -> String {
    env!("CARGO_BIN_EXE_toy_mcp_server").to_string()
}

/// Always answers "yes" — every `McpToolProvider::connect` call in this
/// file needs a guard now that `braze-mcp-client` gates every `invoke`
/// through `PermissionGuard`, and `McpToolCall` is always classified
/// Irreversible (see `braze-permissions::classifier`), so a guard that
/// denies everything would break every non-permission-focused test here.
struct AlwaysAllow;

#[async_trait::async_trait]
impl ConfirmationPrompt for AlwaysAllow {
    async fn confirm(&self, _action: &ActionDescriptor) -> bool {
        true
    }
}

/// Always answers "no" — used by the dedicated denial test below.
struct AlwaysDeny;

#[async_trait::async_trait]
impl ConfirmationPrompt for AlwaysDeny {
    async fn confirm(&self, _action: &ActionDescriptor) -> bool {
        false
    }
}

/// Always answers "yes", and records every action it was asked to confirm
/// — used to inspect the `ActionDescriptor::McpToolCall` that `invoke`
/// actually constructs (D5: the `tool` field must stay the bare,
/// server-reported name, not the namespaced one, or permissions recorded
/// before namespacing existed would stop matching on `--resume`). Kept
/// behind `Arc` (not owned outright by the guard) so the test can still
/// read `seen` after handing a `SharedRecordingPrompt` to `PermissionGuard`.
struct RecordingPrompt {
    seen: std::sync::Mutex<Vec<ActionDescriptor>>,
}

impl RecordingPrompt {
    fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }
}

/// Local newtype around `Arc<RecordingPrompt>` — the orphan rule blocks
/// implementing `ConfirmationPrompt` (foreign trait) directly for `Arc`
/// (foreign type) from this integration-test crate.
struct SharedRecordingPrompt(std::sync::Arc<RecordingPrompt>);

#[async_trait::async_trait]
impl ConfirmationPrompt for SharedRecordingPrompt {
    async fn confirm(&self, action: &ActionDescriptor) -> bool {
        self.0.seen.lock().unwrap().push(action.clone());
        true
    }
}

fn allow_guard() -> PermissionGuard {
    let cwd = std::env::temp_dir();
    PermissionGuard::new(
        WorkdirAllowlist::new(cwd.clone()),
        Box::new(DefaultClassifier::new(WorkdirAllowlist::new(cwd))),
        Box::new(AlwaysAllow),
    )
}

fn deny_guard() -> PermissionGuard {
    let cwd = std::env::temp_dir();
    PermissionGuard::new(
        WorkdirAllowlist::new(cwd.clone()),
        Box::new(DefaultClassifier::new(WorkdirAllowlist::new(cwd))),
        Box::new(AlwaysDeny),
    )
}

async fn connect() -> McpToolProvider {
    McpToolProvider::connect(
        "toy".to_string(),
        toy_server_path(),
        Vec::new(),
        allow_guard(),
    )
    .await
    .expect("toy MCP server should spawn and complete the handshake")
}

/// Like [`connect`], but with an explicit tool-catalog cache TTL — used by
/// the TTL-behavior tests below so they don't have to wait on (or somehow
/// mock) the real 60-second production default.
async fn connect_with_ttl(ttl: Duration) -> McpToolProvider {
    McpToolProvider::connect_with_ttl(
        "toy".to_string(),
        toy_server_path(),
        Vec::new(),
        allow_guard(),
        ttl,
    )
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

/// Invokes the toy server's hidden `call_count` tool (see
/// `src/bin/toy_mcp_server.rs`) and parses the number of `tools/list`
/// requests it has answered so far.
async fn list_tools_call_count(provider: &McpToolProvider) -> u64 {
    let call = tool_call("count", "mcp__toy__call_count", serde_json::json!({}));
    let result = provider
        .invoke(&call)
        .await
        .expect("call_count invocation should succeed");
    result
        .content
        .parse()
        .expect("call_count should return a plain integer")
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
    assert_eq!(
        names,
        vec![
            "mcp__toy__echo",
            "mcp__toy__add",
            "mcp__toy__fail",
            "mcp__toy__verbose",
            "mcp__toy__call_count",
        ]
    );

    for stub in &stubs {
        assert_eq!(stub.source, "mcp:toy");
        assert!(
            stub.input_schema.is_none(),
            "MCP stubs stay deferred (D3) — only local built-ins carry a real schema up front"
        );
    }
}

#[tokio::test]
async fn list_stubs_summary_matches_the_documented_truncation_criteria() {
    let provider = connect().await;
    let stubs = provider
        .list_stubs()
        .await
        .expect("list_stubs should succeed");

    let echo = stubs
        .iter()
        .find(|s| s.name == "mcp__toy__echo")
        .expect("echo stub");
    assert_eq!(
        echo.summary,
        "Echoes back the exact text it was given, for round-trip testing."
    );

    let verbose = stubs
        .iter()
        .find(|s| s.name == "mcp__toy__verbose")
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
        .resolve_schema("mcp__toy__echo")
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
        .resolve_schema("mcp__toy__add")
        .await
        .expect("resolve_schema should succeed")
        .expect("add is a real tool");
    assert_eq!(schema.name, "add");
}

#[tokio::test]
async fn invoke_echo_round_trips_the_text_argument() {
    let provider = connect().await;
    let call = tool_call(
        "call-1",
        "mcp__toy__echo",
        serde_json::json!({"text": "hello mcp"}),
    );
    let result = provider.invoke(&call).await.expect("invoke should succeed");

    assert_eq!(result.tool_call_id, "call-1");
    assert_eq!(result.content, "hello mcp");
    assert!(!result.is_error);
}

#[tokio::test]
async fn invoke_add_computes_the_sum() {
    let provider = connect().await;
    let call = tool_call(
        "call-2",
        "mcp__toy__add",
        serde_json::json!({"a": 2, "b": 3}),
    );
    let result = provider.invoke(&call).await.expect("invoke should succeed");

    assert_eq!(result.content, "5");
    assert!(!result.is_error);
}

#[tokio::test]
async fn invoke_fail_maps_the_mcp_tool_level_error_flag_to_is_error() {
    let provider = connect().await;
    let call = tool_call("call-3", "mcp__toy__fail", serde_json::json!({}));
    let result = provider.invoke(&call).await.expect("invoke should succeed");

    assert!(result.is_error);
    assert!(result.content.contains("intentional failure"));
}

#[tokio::test]
async fn invoke_rejects_non_object_arguments_without_touching_the_server() {
    let provider = connect().await;
    let call = tool_call(
        "call-4",
        "mcp__toy__echo",
        serde_json::json!("not an object"),
    );
    let err = provider
        .invoke(&call)
        .await
        .expect_err("non-object arguments must be rejected");

    match err {
        ToolError::InvocationFailed { name, message } => {
            assert_eq!(name, "mcp__toy__echo");
            assert!(message.contains("JSON object"));
        }
        other => panic!("expected InvocationFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn invoke_records_the_permission_action_with_the_bare_tool_name() {
    let cwd = std::env::temp_dir();
    let recorder = RecordingPrompt::new();
    let guard = PermissionGuard::new(
        WorkdirAllowlist::new(cwd.clone()),
        Box::new(DefaultClassifier::new(WorkdirAllowlist::new(cwd))),
        Box::new(SharedRecordingPrompt(std::sync::Arc::clone(&recorder))),
    );
    let provider = McpToolProvider::connect("toy".to_string(), toy_server_path(), Vec::new(), guard)
        .await
        .expect("toy MCP server should spawn and complete the handshake");

    let call = tool_call(
        "call-5",
        "mcp__toy__add",
        serde_json::json!({"a": 2, "b": 3}),
    );
    provider
        .invoke(&call)
        .await
        .expect("invoke should succeed");

    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    match &seen[0] {
        ActionDescriptor::McpToolCall { server, tool } => {
            assert_eq!(server, "toy");
            assert_eq!(
                tool, "add",
                "PermissionKey must key on the bare server-reported name, not \
                 the namespaced one the model invoked with — otherwise \
                 permissions recorded before namespacing existed stop \
                 matching on --resume"
            );
        }
        other => panic!("expected McpToolCall, got {other:?}"),
    }
}

#[tokio::test]
async fn connect_to_a_nonexistent_command_fails_with_provider_unavailable() {
    let err = McpToolProvider::connect(
        "broken".to_string(),
        "this-binary-does-not-exist-12345".to_string(),
        Vec::new(),
        allow_guard(),
    )
    .await
    .expect_err("spawning a nonexistent binary must fail, not hang");

    assert!(matches!(err, ToolError::ProviderUnavailable(_)));
}

#[tokio::test]
async fn invoke_is_denied_by_a_guard_that_always_refuses() {
    // McpToolCall is always classified Irreversible, so a guard whose
    // prompt always answers "no" must block every invoke before the
    // request ever reaches the toy server.
    let provider = McpToolProvider::connect(
        "toy".to_string(),
        toy_server_path(),
        Vec::new(),
        deny_guard(),
    )
    .await
    .expect("toy MCP server should spawn and complete the handshake");

    let call = tool_call(
        "call-denied",
        "mcp__toy__add",
        serde_json::json!({"a": 2, "b": 3}),
    );
    let err = provider
        .invoke(&call)
        .await
        .expect_err("a denying guard must block the call");

    match err {
        ToolError::InvocationFailed { name, message } => {
            assert_eq!(name, "mcp__toy__add");
            assert!(
                message.contains("denied"),
                "expected a permission-denial message, got: {message}"
            );
        }
        other => panic!("expected InvocationFailed, got {other:?}"),
    }
}

// --- Client-side tool-catalog TTL cache (PLAN.md's SOTA-2026-07 roadmap,
// "Grupo 4") ---
//
// These tests confirm, against the real toy server subprocess, that
// `McpToolProvider` caches `tools/list` results for `cache_ttl` and only
// pays a fresh round trip once that window has elapsed — using the toy
// server's `call_count` tool (see `src/bin/toy_mcp_server.rs`) as the
// instrumentation, since there's no other way to observe how many real
// `tools/list` requests reached the server from outside the process.

#[tokio::test]
async fn list_stubs_called_twice_within_the_ttl_only_fetches_once() {
    let provider = connect_with_ttl(Duration::from_secs(30)).await;

    provider
        .list_stubs()
        .await
        .expect("first list_stubs should succeed");
    provider
        .list_stubs()
        .await
        .expect("second list_stubs should succeed");

    assert_eq!(
        list_tools_call_count(&provider).await,
        1,
        "a second list_stubs() call made well within the TTL must be served \
         from cache, not cost another tools/list round trip"
    );
}

#[tokio::test]
async fn list_stubs_refetches_once_the_ttl_has_elapsed() {
    let provider = connect_with_ttl(Duration::from_millis(20)).await;

    provider
        .list_stubs()
        .await
        .expect("first list_stubs should succeed");
    tokio::time::sleep(Duration::from_millis(100)).await;
    provider
        .list_stubs()
        .await
        .expect("second list_stubs should succeed");

    assert_eq!(
        list_tools_call_count(&provider).await,
        2,
        "a list_stubs() call made after the TTL elapsed must trigger a \
         fresh tools/list round trip"
    );
}

#[tokio::test]
async fn resolve_schema_for_a_known_tool_reuses_the_ttl_cache() {
    let provider = connect_with_ttl(Duration::from_secs(30)).await;

    provider
        .list_stubs()
        .await
        .expect("list_stubs should succeed");
    let schema = provider
        .resolve_schema("mcp__toy__echo")
        .await
        .expect("resolve_schema should succeed")
        .expect("echo is a real tool");

    assert_eq!(schema.name, "echo");
    assert_eq!(
        list_tools_call_count(&provider).await,
        1,
        "resolve_schema for a tool already present in the TTL-fresh cache \
         must not cost another tools/list round trip"
    );
}

#[tokio::test]
async fn resolve_schema_bypasses_the_ttl_and_refetches_when_the_tool_is_unknown() {
    let provider = connect_with_ttl(Duration::from_secs(30)).await;

    provider
        .list_stubs()
        .await
        .expect("list_stubs should succeed");
    let schema = provider
        .resolve_schema("does_not_exist")
        .await
        .expect("resolve_schema should succeed even for an unknown tool");

    assert!(schema.is_none());
    assert_eq!(
        list_tools_call_count(&provider).await,
        2,
        "an unresolved tool name must force a fresh, TTL-bypassing \
         re-fetch (it might be a brand new tool the TTL-fresh cache \
         predates) before answering None"
    );
}
