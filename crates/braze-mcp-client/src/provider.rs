use std::time::Duration;

use async_trait::async_trait;
use braze_tools_core::{ToolError, ToolProvider, ToolSchema};
use braze_types::{ToolCall, ToolResult, ToolStub};
use rmcp::model::{CallToolRequestParams, ContentBlock, ResourceContents, Tool};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use tokio::sync::RwLock;

use crate::error::McpClientError;
use crate::summary::summarize;

/// How long to wait for the subprocess to spawn and the MCP `initialize`
/// handshake to complete before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for any single MCP request (`tools/list`, `tools/call`)
/// before treating the server as unavailable. A hung/dead subprocess must
/// never leave a `ToolProvider` caller blocked indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// One connection to one external MCP server, spawned as a stdio
/// subprocess (`command args...`). Implements
/// [`ToolProvider`](braze_tools_core::ToolProvider) so it composes into a
/// `ToolRegistry` as a sibling of `braze-tools-local`'s built-ins — see
/// PLAN.md, dependency graph ("neither implementer depends on the other").
#[derive(Debug)]
pub struct McpToolProvider {
    /// `format!("mcp:{name}")`, computed once at connect time.
    provider_id: String,
    service: RunningService<RoleClient, ()>,
    /// Last full `tools/list` result. `None` until the first successful
    /// fetch.
    ///
    /// Caching trade-off (see [`McpToolProvider::list_stubs`] and
    /// [`McpToolProvider::resolve_schema`]): MCP has no "fetch one tool's
    /// schema" call, only a bulk `tools/list` — so every `resolve_schema`
    /// would otherwise cost a full round trip to re-list every tool just to
    /// read one. Instead, `list_stubs` (which the registry calls at least
    /// once per turn to build the flat stub list, per PLAN.md's deferred
    /// loading design) always fetches fresh from the server and refreshes
    /// this cache as a side effect; `resolve_schema` then almost always
    /// serves from that cache with zero network cost. The trade-off is a
    /// staleness window: if the server's tool set changes between a
    /// `list_stubs` call and a `resolve_schema` call for a *new* tool that
    /// wasn't in the cached list yet, `resolve_schema` would wrongly report
    /// `Ok(None)`. To close that gap without paying the round-trip cost on
    /// every call, `resolve_schema` treats a cache miss as "maybe stale"
    /// and re-fetches once before finally answering `None`.
    tool_cache: RwLock<Option<Vec<Tool>>>,
}

impl McpToolProvider {
    /// Spawns `command args...` as a stdio subprocess and completes the MCP
    /// client handshake against it.
    pub async fn connect(
        name: String,
        command: String,
        args: Vec<String>,
    ) -> Result<Self, ToolError> {
        let provider_id = format!("mcp:{name}");
        tracing::info!(
            provider = %provider_id,
            command = %command,
            args = ?args,
            "spawning MCP server subprocess"
        );

        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(&args);
        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| McpClientError::Spawn(e).into_tool_error(&provider_id))?;

        let service = tokio::time::timeout(CONNECT_TIMEOUT, ().serve(transport))
            .await
            .map_err(|_| McpClientError::Timeout(CONNECT_TIMEOUT).into_tool_error(&provider_id))?
            .map_err(|e| McpClientError::Initialize(Box::new(e)).into_tool_error(&provider_id))?;

        tracing::info!(provider = %provider_id, "connected to MCP server");

        Ok(Self {
            provider_id,
            service,
            tool_cache: RwLock::new(None),
        })
    }

    /// Fetches the full tool list from the server, replacing the cache.
    async fn list_tools_fresh(&self) -> Result<Vec<Tool>, ToolError> {
        let tools = tokio::time::timeout(REQUEST_TIMEOUT, self.service.list_all_tools())
            .await
            .map_err(|_| {
                McpClientError::Timeout(REQUEST_TIMEOUT).into_tool_error(&self.provider_id)
            })?
            .map_err(|e| McpClientError::Request(e).into_tool_error(&self.provider_id))?;

        *self.tool_cache.write().await = Some(tools.clone());
        Ok(tools)
    }

    /// Looks up `name` in the current cache without touching the network.
    async fn find_cached(&self, name: &str) -> Option<Tool> {
        self.tool_cache
            .read()
            .await
            .as_ref()
            .and_then(|tools| tools.iter().find(|t| t.name.as_ref() == name).cloned())
    }
}

impl Drop for McpToolProvider {
    fn drop(&mut self) {
        // `RunningService`'s own `Drop` asynchronously kills the subprocess
        // right after this runs (fields drop in declaration order after the
        // body of this impl finishes) — this log line is what makes that
        // teardown visible under `RUST_LOG=braze=debug`, matching the
        // connect-time `tracing::info!` above.
        tracing::info!(
            provider = %self.provider_id,
            "disconnecting from MCP server (subprocess will be terminated)"
        );
    }
}

#[async_trait]
impl ToolProvider for McpToolProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        let tools = self.list_tools_fresh().await?;
        Ok(tools
            .iter()
            .map(|tool| ToolStub {
                name: tool.name.to_string(),
                summary: summarize(tool.description.as_deref().unwrap_or("")),
                source: self.provider_id.clone(),
            })
            .collect())
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        if let Some(tool) = self.find_cached(name).await {
            tracing::debug!(
                provider = %self.provider_id,
                tool = name,
                "resolved full tool schema from cache"
            );
            return Ok(Some(to_schema(&tool)));
        }

        tracing::debug!(
            provider = %self.provider_id,
            tool = name,
            "tool not in cache, resolving full schema from MCP server on demand"
        );
        let tools = self.list_tools_fresh().await?;
        Ok(tools
            .into_iter()
            .find(|t| t.name.as_ref() == name)
            .map(|t| to_schema(&t)))
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let arguments = match &call.arguments {
            serde_json::Value::Null => None,
            serde_json::Value::Object(map) => Some(map.clone()),
            other => {
                // Not a provider-availability problem — the MCP wire format
                // requires call arguments to be a JSON object, and this
                // caller sent something else. `ToolError` has no dedicated
                // "bad arguments" variant, so this is reported the same way
                // any other tool-side invocation failure is.
                return Err(ToolError::InvocationFailed {
                    name: call.name.clone(),
                    message: format!("MCP tool arguments must be a JSON object, got: {other}"),
                });
            }
        };

        let mut params = CallToolRequestParams::new(call.name.clone());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }

        let result = tokio::time::timeout(REQUEST_TIMEOUT, self.service.call_tool(params))
            .await
            .map_err(|_| {
                McpClientError::Timeout(REQUEST_TIMEOUT).into_tool_error(&self.provider_id)
            })?
            .map_err(|e| McpClientError::Request(e).into_tool_error(&self.provider_id))?;

        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: render_content(&result.content),
            is_error: result.is_error.unwrap_or(false),
        })
    }
}

fn to_schema(tool: &Tool) -> ToolSchema {
    ToolSchema {
        name: tool.name.to_string(),
        description: tool
            .description
            .clone()
            .map(|d| d.to_string())
            .unwrap_or_default(),
        input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
    }
}

/// Flattens a (possibly multi-block, multi-modal) MCP tool result into the
/// single `content: String` that `braze_types::ToolResult` expects.
///
/// Criterion, block by block, joined with `\n`:
/// - `Text` contributes its text verbatim.
/// - `Image`/`Audio` contribute a `[image: <mime> (<n> bytes base64)]`
///   placeholder rather than the raw base64 payload — dumping kilobytes of
///   base64 into the model's context would defeat the whole point of this
///   crate's summary/schema-deferral design, and the MVP's plain-text
///   `ToolResult.content` has no dedicated multimodal channel for the model
///   to actually consume image/audio bytes.
/// - `Resource` (embedded) contributes its URI, and for text resources the
///   text itself (actionable context); blob resources get the same
///   byte-count placeholder as images/audio.
/// - `ResourceLink` contributes just its URI.
/// - Any future `ContentBlock` variant (the enum is `#[non_exhaustive]`)
///   falls back to a generic placeholder rather than failing the call.
///
/// An empty block list becomes `""`.
fn render_content(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .map(render_block)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_block(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(text) => text.text.clone(),
        ContentBlock::Image(image) => format!(
            "[image: {} ({} bytes base64)]",
            image.mime_type,
            image.data.len()
        ),
        ContentBlock::Audio(audio) => format!(
            "[audio: {} ({} bytes base64)]",
            audio.mime_type,
            audio.data.len()
        ),
        ContentBlock::Resource(resource) => render_resource_contents(&resource.resource),
        ContentBlock::ResourceLink(link) => format!("[resource: {}]", link.uri),
        _ => "[unsupported MCP content block]".to_string(),
    }
}

fn render_resource_contents(resource: &ResourceContents) -> String {
    match resource {
        ResourceContents::TextResourceContents { uri, text, .. } => {
            format!("[resource: {uri}]\n{text}")
        }
        ResourceContents::BlobResourceContents {
            uri,
            mime_type,
            blob,
            ..
        } => format!(
            "[resource: {uri}, {} ({} bytes base64)]",
            mime_type.as_deref().unwrap_or("application/octet-stream"),
            blob.len()
        ),
        _ => "[unsupported MCP resource contents]".to_string(),
    }
}
