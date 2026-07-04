use braze_types::{ToolCall, ToolResult, ToolStub};

use crate::error::ToolError;
use crate::provider::{ToolProvider, ToolSchema};

/// Fans a lookup out across every connected [`ToolProvider`] (local
/// built-ins, one or more MCP servers). This struct — not the
/// [`ToolProvider`] trait itself — is the "search mechanism" behind
/// deferred tool loading: [`ToolRegistry::all_stubs`] is cheap and always
/// in context; [`ToolRegistry::resolve`] is called only once, right before
/// a specific tool is dispatched.
///
/// Signature frozen in PLAN.md Fase 1; body implemented in Fase 3 (Nivel 1,
/// alongside `ToolProvider`).
pub struct ToolRegistry {
    providers: Vec<Box<dyn ToolProvider>>,
}

impl ToolRegistry {
    pub fn new(providers: Vec<Box<dyn ToolProvider>>) -> Self {
        Self { providers }
    }

    /// Cheap: fans `list_stubs` out across every provider.
    ///
    /// Providers are independent I/O sources (local built-ins, MCP
    /// connections over stdio/network), so this fans the calls out
    /// concurrently via `futures::future::join_all` instead of awaiting
    /// them one at a time — a slow MCP server shouldn't serialize behind
    /// the others just to build the flat stub list. `join_all` still waits
    /// for every future to settle before this function inspects the
    /// results (it doesn't cancel siblings the moment one provider fails),
    /// but for the MVP's handful of providers that's an acceptable trade
    /// against hand-rolling early cancellation with `FuturesUnordered`.
    /// Once everything has settled, results are walked back in provider
    /// order and the first error found is propagated immediately — no
    /// partial-success tolerance, per the MVP's fail-fast requirement.
    pub async fn all_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
        let futures = self.providers.iter().map(|p| p.list_stubs());
        let results = futures::future::join_all(futures).await;

        let mut stubs = Vec::new();
        for result in results {
            stubs.extend(result?);
        }
        Ok(stubs)
    }

    /// Resolves the full schema for `name` — the deferred-loading step,
    /// called right before dispatch.
    ///
    /// Walks providers in registration order and returns the first
    /// `Some(schema)`, matching the ordering that
    /// [`ToolProvider::resolve_schema`] documents: "the registry then
    /// tries the next provider". This is the point where a schema that was
    /// previously only a name+summary [`ToolStub`] gets fetched in full —
    /// logged so `RUST_LOG=braze=debug` can confirm tools stay
    /// stub-only in context until the model actually asks to call one.
    pub async fn resolve(&self, name: &str) -> Result<ToolSchema, ToolError> {
        for provider in &self.providers {
            if let Some(schema) = provider.resolve_schema(name).await? {
                tracing::debug!(
                    tool = name,
                    provider = provider.provider_id(),
                    "resolved full tool schema on demand"
                );
                return Ok(schema);
            }
        }
        Err(ToolError::NotFound(name.to_string()))
    }

    /// Dispatches `call` to whichever provider owns `call.name`.
    ///
    /// Same in-order search as [`ToolRegistry::resolve`], but here to
    /// locate the owning provider rather than to return a schema.
    pub async fn dispatch(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
        for provider in &self.providers {
            // Re-checking ownership via `resolve_schema` immediately before
            // `invoke` is a somewhat redundant lookup — in practice
            // `call.name` is always a name this provider already advertised
            // via `list_stubs` (see `ToolProvider::invoke`'s doc-comment).
            // It's kept anyway so `ToolRegistry` can find *which* provider
            // owns `call.name` without maintaining its own name->provider
            // index. Accepted as an MVP trade-off: one extra async call per
            // dispatch in exchange for not giving `ToolRegistry` any cached
            // state that would need to stay in sync with the providers it
            // wraps.
            if provider.resolve_schema(&call.name).await?.is_some() {
                return provider.invoke(call).await;
            }
        }
        Err(ToolError::NotFound(call.name.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Hand-rolled [`ToolProvider`] mock: owns a fixed set of
    /// `(name, summary)` tools and can optionally be made to fail
    /// `list_stubs` to exercise the fail-fast path of `all_stubs`.
    struct MockProvider {
        id: &'static str,
        tools: Vec<&'static str>,
        fail_list_stubs: bool,
    }

    impl MockProvider {
        fn new(id: &'static str, tools: Vec<&'static str>) -> Self {
            Self {
                id,
                tools,
                fail_list_stubs: false,
            }
        }

        fn failing(id: &'static str) -> Self {
            Self {
                id,
                tools: Vec::new(),
                fail_list_stubs: true,
            }
        }
    }

    #[async_trait]
    impl ToolProvider for MockProvider {
        fn provider_id(&self) -> &str {
            self.id
        }

        async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError> {
            if self.fail_list_stubs {
                return Err(ToolError::ProviderUnavailable(self.id.to_string()));
            }
            Ok(self
                .tools
                .iter()
                .map(|name| ToolStub {
                    name: name.to_string(),
                    summary: format!("{name} summary"),
                    source: self.id.to_string(),
                })
                .collect())
        }

        async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError> {
            if self.tools.contains(&name) {
                Ok(Some(ToolSchema {
                    name: name.to_string(),
                    description: format!("{name} description"),
                    input_schema: serde_json::json!({}),
                }))
            } else {
                Ok(None)
            }
        }

        async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError> {
            if !self.tools.contains(&call.name.as_str()) {
                return Err(ToolError::NotFound(call.name.clone()));
            }
            Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("invoked by {}", self.id),
                is_error: false,
            })
        }
    }

    fn tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: "call-1".to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn all_stubs_concatenates_across_providers() {
        let registry = ToolRegistry::new(vec![
            Box::new(MockProvider::new("local", vec!["read_file", "write_file"])),
            Box::new(MockProvider::new("mcp:filesystem", vec!["list_dir"])),
        ]);

        let stubs = registry
            .all_stubs()
            .await
            .expect("all_stubs should succeed");
        let names: Vec<&str> = stubs.iter().map(|s| s.name.as_str()).collect();

        assert_eq!(names, vec!["read_file", "write_file", "list_dir"]);
    }

    #[tokio::test]
    async fn all_stubs_propagates_first_provider_error() {
        let registry = ToolRegistry::new(vec![
            Box::new(MockProvider::new("local", vec!["read_file"])),
            Box::new(MockProvider::failing("mcp:broken")),
        ]);

        let err = registry
            .all_stubs()
            .await
            .expect_err("a failing provider must fail the whole call");

        assert!(matches!(err, ToolError::ProviderUnavailable(id) if id == "mcp:broken"));
    }

    #[tokio::test]
    async fn resolve_tries_providers_in_order_and_returns_first_match() {
        // Second provider owns a tool the first one doesn't — this checks
        // that `resolve` doesn't stop after the first provider comes back
        // empty and actually walks the whole list in order.
        let registry = ToolRegistry::new(vec![
            Box::new(MockProvider::new("local", vec!["read_file"])),
            Box::new(MockProvider::new("mcp:filesystem", vec!["list_dir"])),
        ]);

        let schema = registry
            .resolve("list_dir")
            .await
            .expect("list_dir is owned by the second provider");

        assert_eq!(schema.name, "list_dir");
    }

    #[tokio::test]
    async fn resolve_returns_not_found_when_no_provider_owns_the_tool() {
        let registry = ToolRegistry::new(vec![
            Box::new(MockProvider::new("local", vec!["read_file"])),
            Box::new(MockProvider::new("mcp:filesystem", vec!["list_dir"])),
        ]);

        let err = registry
            .resolve("does_not_exist")
            .await
            .expect_err("no provider owns this tool");

        assert!(matches!(err, ToolError::NotFound(name) if name == "does_not_exist"));
    }

    #[tokio::test]
    async fn dispatch_routes_to_the_owning_provider() {
        let registry = ToolRegistry::new(vec![
            Box::new(MockProvider::new("local", vec!["read_file"])),
            Box::new(MockProvider::new("mcp:filesystem", vec!["list_dir"])),
        ]);

        let result = registry
            .dispatch(&tool_call("list_dir"))
            .await
            .expect("list_dir is owned by the second provider");

        assert_eq!(result.content, "invoked by mcp:filesystem");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn dispatch_returns_not_found_when_no_provider_owns_the_tool() {
        let registry = ToolRegistry::new(vec![
            Box::new(MockProvider::new("local", vec!["read_file"])),
            Box::new(MockProvider::new("mcp:filesystem", vec!["list_dir"])),
        ]);

        let err = registry
            .dispatch(&tool_call("does_not_exist"))
            .await
            .expect_err("no provider owns this tool");

        assert!(matches!(err, ToolError::NotFound(name) if name == "does_not_exist"));
    }
}
