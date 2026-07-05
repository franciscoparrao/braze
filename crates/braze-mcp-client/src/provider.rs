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

/// How long a fetched tool catalog is trusted before a fresh `tools/list`
/// round trip is forced again.
///
/// Context (see PLAN.md's SOTA-2026-07 roadmap, "Grupo 4"): the roadmap
/// doc originally cited adopting the MCP spec's TTL/`cacheScope` mechanism
/// (SEP-2549) here. That SEP does not exist yet — it only appears in a
/// spec release-candidate dated 2026-07-28 (in the future relative to this
/// change) and has zero implementation in `rmcp` 2.1.0, the latest version
/// on crates.io and the one this crate already depends on. Meanwhile the
/// actual problem is more pressing than the roadmap doc suggested:
/// `braze-engine::Engine::run_turn` calls `ToolRegistry::all_stubs` (which
/// calls every provider's `list_stubs`) once per model↔tool round inside a
/// single turn, not once per session — up to `MAX_TURN_ITERATIONS` (20)
/// times in the worst case. Every one of those was an unconditional
/// network round trip before this change.
///
/// A client-side, elapsed-time TTL solves the same practical problem
/// without depending on any server-side protocol support: an MCP server's
/// tool catalog essentially never changes mid-session, but a bounded TTL
/// still avoids permanently serving a stale catalog if the user
/// reconnects/reconfigures a server without restarting `braze`.
///
/// 60 seconds is a starting point, not a tuned value — comfortably longer
/// than a single turn's worth of round trips (closing the up-to-20x gap
/// above), short enough that a genuine server-side change is picked up
/// within a minute of the cache going stale. Revisit if a future session
/// shows this is too short/long in practice, or if `rmcp` ever implements
/// the real protocol-level mechanism (see doc comment above).
const TOOL_CACHE_TTL: Duration = Duration::from_secs(60);

/// One connection to one external MCP server, spawned as a stdio
/// subprocess (`command args...`). Implements
/// [`ToolProvider`](braze_tools_core::ToolProvider) so it composes into a
/// `ToolRegistry` as a sibling of `braze-tools-local`'s built-ins — see
/// PLAN.md, dependency graph ("neither implementer depends on the other").
pub struct McpToolProvider {
    /// `format!("mcp:{name}")`, computed once at connect time.
    provider_id: String,
    /// The bare server name (`name` passed to `connect`, before the
    /// `"mcp:"` prefix), kept around separately so `invoke` can populate
    /// `braze_permissions::ActionDescriptor::McpToolCall { server, .. }`
    /// with a plain name rather than re-deriving it from `provider_id`.
    server_name: String,
    /// Every tool call this provider dispatches is gated through this
    /// guard — see PLAN.md's SOTA-2026-07 roadmap ("Grupo 2"): before this
    /// crate had no permission gating at all, unlike `braze-tools-local`.
    guard: braze_permissions::PermissionGuard,
    /// How long a fetched catalog is trusted before [`Self::tools_respecting_ttl`]
    /// forces a fresh fetch. Always [`TOOL_CACHE_TTL`] in production; only
    /// [`Self::connect_with_ttl`] (used by this crate's own tests) can set
    /// it to something else, so expiration behavior can be exercised
    /// without waiting on a real 60-second clock.
    cache_ttl: Duration,
    service: RunningService<RoleClient, ()>,
    /// Last full `tools/list` result plus the instant it was fetched.
    /// `None` until the first successful fetch.
    ///
    /// Caching trade-off (see [`McpToolProvider::list_stubs`] and
    /// [`McpToolProvider::resolve_schema`]): MCP has no "fetch one tool's
    /// schema" call, only a bulk `tools/list` — so every `resolve_schema`
    /// would otherwise cost a full round trip to re-list every tool just to
    /// read one. Instead, both `list_stubs` and `resolve_schema` route
    /// through [`McpToolProvider::tools_respecting_ttl`], which serves the
    /// cached list as long as it's within [`TOOL_CACHE_TTL`] and only pays
    /// the round-trip cost when the cache is empty or stale. The trade-off
    /// is a staleness window bounded by the TTL: if the server's tool set
    /// changes, `braze` can serve the old catalog for up to `TOOL_CACHE_TTL`.
    /// `resolve_schema` additionally treats "the specific tool being looked
    /// up isn't in the list we have" as always worth a forced, TTL-bypassing
    /// re-fetch — the case a bare TTL wouldn't cover on its own (a brand new
    /// tool that just appeared).
    tool_cache: RwLock<Option<ToolCacheEntry>>,
}

/// A cached `tools/list` result, timestamped so [`TOOL_CACHE_TTL`] can be
/// enforced.
struct ToolCacheEntry {
    tools: Vec<Tool>,
    fetched_at: tokio::time::Instant,
}

impl McpToolProvider {
    /// Spawns `command args...` as a stdio subprocess and completes the MCP
    /// client handshake against it.
    pub async fn connect(
        name: String,
        command: String,
        args: Vec<String>,
        guard: braze_permissions::PermissionGuard,
    ) -> Result<Self, ToolError> {
        Self::connect_with_ttl(name, command, args, guard, TOOL_CACHE_TTL).await
    }

    /// Same as [`Self::connect`], but with an explicit tool-catalog cache
    /// TTL instead of the production default ([`TOOL_CACHE_TTL`]).
    ///
    /// Not needed by normal callers — `braze-cli` always uses [`Self::connect`].
    /// This exists so this crate's own integration tests can exercise both
    /// "within TTL" and "TTL expired" behavior deterministically (a short
    /// TTL of a few milliseconds) instead of waiting on, or mocking, a real
    /// 60-second clock.
    pub async fn connect_with_ttl(
        name: String,
        command: String,
        args: Vec<String>,
        guard: braze_permissions::PermissionGuard,
        cache_ttl: Duration,
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
            server_name: name,
            guard,
            cache_ttl,
            service,
            tool_cache: RwLock::new(None),
        })
    }

    /// Fetches the full tool list from the server, unconditionally
    /// replacing the cache with a fresh timestamp. Bypasses the TTL check —
    /// callers that want the TTL respected should go through
    /// [`Self::tools_respecting_ttl`] instead.
    async fn list_tools_fresh(&self) -> Result<Vec<Tool>, ToolError> {
        let tools = tokio::time::timeout(REQUEST_TIMEOUT, self.service.list_all_tools())
            .await
            .map_err(|_| {
                McpClientError::Timeout(REQUEST_TIMEOUT).into_tool_error(&self.provider_id)
            })?
            .map_err(|e| McpClientError::Request(e).into_tool_error(&self.provider_id))?;

        *self.tool_cache.write().await = Some(ToolCacheEntry {
            tools: tools.clone(),
            fetched_at: tokio::time::Instant::now(),
        });
        Ok(tools)
    }

    /// Returns the cached tool list if it's still within `cache_ttl`,
    /// otherwise fetches fresh from the server (refreshing the cache with a
    /// new timestamp as a side effect). Both `list_stubs` and
    /// `resolve_schema` route through this so the TTL policy lives in one
    /// place.
    async fn tools_respecting_ttl(&self) -> Result<Vec<Tool>, ToolError> {
        {
            let cache = self.tool_cache.read().await;
            if let Some(entry) = cache.as_ref()
                && entry.fetched_at.elapsed() < self.cache_ttl
            {
                return Ok(entry.tools.clone());
            }
        }
        self.list_tools_fresh().await
    }

    /// Returns whatever tool list is cached, however stale, only fetching
    /// if the cache has never been populated at all. Used by `invoke` to
    /// resolve a namespaced name back to the server's raw one (D5) —
    /// deliberately *not* `tools_respecting_ttl`: `invoke` follows a
    /// `resolve_schema` call in the normal `ToolRegistry::dispatch` flow
    /// (which already paid for TTL freshness moments earlier), so forcing
    /// another TTL check here would only add a redundant staleness window
    /// for the identity of a tool the caller already committed to — no
    /// freshness is gained, only the risk of a surprise network round trip
    /// on a call whose whole point (from the caller's perspective) is "just
    /// invoke it".
    async fn cached_tools_or_bootstrap(&self) -> Result<Vec<Tool>, ToolError> {
        if let Some(entry) = self.tool_cache.read().await.as_ref() {
            return Ok(entry.tools.clone());
        }
        self.list_tools_fresh().await
    }
}

// `PermissionGuard` doesn't implement `Debug`, so this can no longer be
// `#[derive(Debug)]`. `finish_non_exhaustive` signals that some fields
// (`guard`, `service`, `tool_cache`) are intentionally omitted rather than
// silently dropped by an oversight.
impl std::fmt::Debug for McpToolProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolProvider")
            .field("provider_id", &self.provider_id)
            .finish_non_exhaustive()
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
        let tools = self.tools_respecting_ttl().await?;
        warn_on_intra_provider_collisions(&self.provider_id, &self.server_name, &tools);
        Ok(tools
            .iter()
            .map(|tool| ToolStub {
                name: advertised_name(&self.server_name, &tool.name),
                summary: summarize(tool.description.as_deref().unwrap_or("")),
                source: self.provider_id.clone(),
                // Deferred by design, unlike the local built-ins: an MCP
                // server's tool set is unbounded/dynamic, so including
                // every real schema on every turn would bloat the prompt.
                // Resolved on demand instead, via `resolve_schema` below.
                input_schema: None,
            })
            .collect())
    }

    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
        let tools = self.tools_respecting_ttl().await?;
        if let Some(tool) = tools
            .iter()
            .find(|t| advertised_name(&self.server_name, &t.name) == name)
        {
            tracing::debug!(
                provider = %self.provider_id,
                tool = name,
                "resolved full tool schema from cache (respecting TTL)"
            );
            return Ok(Some(to_schema(tool)));
        }

        // The tool isn't in the list we have. This could be because it
        // genuinely doesn't exist, or because it's brand new and appeared
        // on the server after our cached/TTL-fresh list was taken — a case
        // a bare TTL doesn't cover on its own. Force a real, TTL-bypassing
        // re-fetch before answering `None`, same safety net the MVP had
        // before the TTL was introduced.
        tracing::debug!(
            provider = %self.provider_id,
            tool = name,
            "tool not in TTL-respecting list, forcing a fresh fetch from the MCP server"
        );
        let tools = self.list_tools_fresh().await?;
        Ok(tools
            .into_iter()
            .find(|t| advertised_name(&self.server_name, &t.name) == name)
            .map(|t| to_schema(&t)))
    }

    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        // Forward-map every raw tool name to its advertised (namespaced)
        // form and compare — never invert the namespacing/sanitization by
        // stripping a prefix off `call.name`. Stripping would be lossy
        // whenever the raw tool name itself contains characters the
        // sanitizer rewrites (e.g. `"fs.read"` advertised as
        // `mcp__server__fs_read` — stripping the prefix back off yields
        // `"fs_read"`, which no longer matches the server's real
        // `"fs.read"`). This always calls the server with the exact name
        // it originally reported.
        let tools = self.cached_tools_or_bootstrap().await?;
        let bare_name = tools
            .iter()
            .find(|t| advertised_name(&self.server_name, &t.name) == call.name)
            .map(|t| t.name.to_string())
            .ok_or_else(|| ToolError::InvocationFailed {
                name: call.name.clone(),
                message: format!("no MCP tool is currently advertised as '{}'", call.name),
            })?;

        // `PermissionKey::McpToolCall.tool` persists in `AgentEvent::PermissionDecided`
        // and is reconstructed on `--resume` — it must stay the bare
        // server-reported name (not the namespaced one) so permissions
        // recorded before this namespacing existed keep matching.
        let action = braze_permissions::ActionDescriptor::McpToolCall {
            server: self.server_name.clone(),
            tool: bare_name.clone(),
        };
        self.guard
            .check(&action)
            .await
            .map_err(|err| ToolError::InvocationFailed {
                name: call.name.clone(),
                message: err.to_string(),
            })?;

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

        // Every error surfaced to the model above uses `call.name` (the
        // namespaced name it invoked with), so it can correlate the error
        // with its own tool call — only the wire request to the server
        // itself uses `bare_name`.
        let mut params = CallToolRequestParams::new(bare_name);
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

/// Builds a namespaced tool name in the same style as Claude Code's own
/// MCP client (`mcp__<server>__<tool>`), sanitizing both components so the
/// combined name always matches the `^[a-zA-Z0-9_-]+$`-style pattern
/// model-facing tool-call APIs (Anthropic, Ollama) require. Local built-in
/// tools never carry this prefix, and each server gets its own, so this
/// alone eliminates the local-vs-MCP and server-vs-server name collisions
/// described in `docs/AUDITORIA-2026-07.md` (hallazgo D5).
///
/// This is a pure, forward-only mapping. `resolve_schema`/`invoke` never
/// try to invert it (strip a prefix back off) — see their doc comments for
/// why that would be lossy.
fn advertised_name(server: &str, raw_tool_name: &str) -> String {
    format!("mcp__{}__{}", sanitize(server), sanitize(raw_tool_name))
}

fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Sanitization is lossy: two distinct raw names from the same server can
/// collapse to the same `advertised_name` (e.g. `"foo.bar"` and
/// `"foo:bar"` both become `foo_bar`), silently shadowing one of them.
/// Defensive-only: logs so the collision is diagnosable, never fails the
/// call — an unreachable tool is a better failure mode than a broken
/// server connection.
fn warn_on_intra_provider_collisions(provider_id: &str, server_name: &str, tools: &[Tool]) {
    let mut seen = std::collections::HashSet::new();
    for tool in tools {
        let advertised = advertised_name(server_name, &tool.name);
        if !seen.insert(advertised.clone()) {
            tracing::warn!(
                provider = %provider_id,
                tool = %tool.name,
                advertised_name = %advertised,
                "two distinct MCP tool names from this server sanitize to the same advertised \
                 name — one is now unreachable"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_name_namespaces_with_server_and_tool() {
        assert_eq!(advertised_name("toy", "echo"), "mcp__toy__echo");
    }

    #[test]
    fn advertised_name_sanitizes_invalid_characters_in_both_components() {
        assert_eq!(
            advertised_name("my server", "fs.read"),
            "mcp__my_server__fs_read"
        );
    }

    #[test]
    fn advertised_name_is_a_pure_forward_mapping_not_meant_to_be_inverted() {
        // Two distinct raw names can sanitize to the same advertised name
        // — this is the documented, defensive-only collision case that
        // `warn_on_intra_provider_collisions` logs rather than prevents.
        assert_eq!(
            advertised_name("srv", "foo.bar"),
            advertised_name("srv", "foo:bar")
        );
    }

    #[test]
    fn warn_on_intra_provider_collisions_does_not_panic_on_distinct_names() {
        let tools = vec![
            Tool::new("echo", "", serde_json::Map::new()),
            Tool::new("add", "", serde_json::Map::new()),
        ];
        warn_on_intra_provider_collisions("mcp:toy", "toy", &tools);
    }
}
