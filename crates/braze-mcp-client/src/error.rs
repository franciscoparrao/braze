use std::time::Duration;

use thiserror::Error;

/// Everything that can go wrong while spawning/talking to the MCP
/// subprocess, before it gets flattened into
/// [`braze_tools_core::ToolError`] at the `ToolProvider` boundary.
///
/// `ToolProvider`'s error type is fixed by the frozen contract
/// (`braze-tools-core/src/provider.rs`) and only has three variants, none
/// of which distinguish "subprocess failed to spawn" from "server sent a
/// malformed handshake" from "request timed out". This type keeps that
/// detail alive long enough to build one good error message; every variant
/// maps to [`braze_tools_core::ToolError::ProviderUnavailable`] via
/// [`McpClientError::into_tool_error`], per the crate's restriction: a dead
/// subprocess or broken connection must never panic or hang, only ever
/// surface as `ProviderUnavailable`.
#[derive(Debug, Error)]
pub(crate) enum McpClientError {
    #[error("failed to spawn MCP server subprocess: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("MCP client handshake failed: {0}")]
    Initialize(#[source] Box<rmcp::service::ClientInitializeError>),

    #[error("MCP request failed: {0}")]
    Request(#[source] rmcp::ServiceError),

    #[error("MCP operation timed out after {0:?}")]
    Timeout(Duration),

    /// K-16: el server está en el cooldown de la negative-cache tras un
    /// timeout — la llamada se rechaza INSTANTÁNEO en vez de re-pagar
    /// otro `REQUEST_TIMEOUT` completo contra un server colgado. El
    /// mensaje dice cuándo se re-probará, para que el modelo (o el
    /// operador leyendo el log) sepa que no es permanente.
    #[error(
        "MCP server is in a failure cooldown (a request timed out {since:?} ago); \
         failing fast instead of waiting out another timeout — it will be re-probed \
         in {retry_in:?}"
    )]
    NegativeCache { since: Duration, retry_in: Duration },
}

impl McpClientError {
    /// Flattens into the frozen `ToolError` shape, tagging the message with
    /// this provider's id so a multi-server `ToolRegistry` failure log line
    /// is still attributable to the right subprocess.
    pub(crate) fn into_tool_error(self, provider_id: &str) -> braze_tools_core::ToolError {
        braze_tools_core::ToolError::ProviderUnavailable(format!("{provider_id}: {self}"))
    }
}
