//! Runs one task against one backend, end to end, through the real
//! `braze_engine::Engine` — same composition `braze-cli` does at
//! startup, minus MCP servers (determinism: no dependency on what
//! happens to be configured/reachable) and with permission confirmation
//! replaced by [`DenyAll`] (see module doc on why, and PLAN.md's
//! "Hallazgo de diseño no anticipado").

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use braze_config::Config;
use braze_permissions::{
    ActionDescriptor, ConfirmationPrompt, DefaultClassifier, PermissionGuard, WorkdirAllowlist,
};
use braze_session::{FileSessionStore, SessionStore, SimpleContextCompactor};
use braze_types::SessionId;

use crate::backend_spec::BackendSpec;
use crate::error::BenchError;
use crate::metrics::{TaskResult, compute_metrics};
use crate::sandbox::TaskSandbox;
use crate::task::TaskDef;

const SYSTEM_PROMPT: &str = "You are braze, an experimental agentic CLI assistant.";

/// Always denies. Combined with a `WorkdirAllowlist` scoped to a
/// throwaway sandbox directory, this means: safe/reversible actions
/// (reads, writes inside the sandbox, allowlisted shell commands) proceed
/// exactly as they would interactively, while anything the classifier
/// flags `Irreversible` — a hallucinated `dd`/`curl`/`mv`, a write
/// outside the sandbox — is refused before it ever runs for real. See
/// `braze_tools_local::test_support::AlwaysDeny` for the identical
/// `#[cfg(test)]`-only shape this mirrors.
struct DenyAll;

#[async_trait]
impl ConfirmationPrompt for DenyAll {
    async fn confirm(&self, _action: &ActionDescriptor) -> bool {
        false
    }
}

/// Runs `task` against the backend `spec` builds, and returns the
/// resulting metrics. Never propagates a model/engine error up to the
/// caller — a failed run still produces a `TaskResult` (with
/// `converged: false` and `run_error: Some(..)`) so one bad task doesn't
/// abort the whole suite.
pub async fn run_task(
    spec: &BackendSpec,
    config: &Config,
    task: &TaskDef,
) -> Result<TaskResult, BenchError> {
    let sandbox = TaskSandbox::new(task)?;

    let allowlist = WorkdirAllowlist::new(sandbox.path());
    let classifier = DefaultClassifier::new(WorkdirAllowlist::new(sandbox.path()));
    let guard = PermissionGuard::new(allowlist, Box::new(classifier), Box::new(DenyAll));
    let tools_provider = braze_tools_local::LocalToolsProvider::new(guard);
    let tools = braze_tools_core::ToolRegistry::new(vec![Box::new(tools_provider)]);

    let session_dir = std::env::temp_dir().join(format!(
        "braze-bench-session-{}-{}",
        std::process::id(),
        SessionId::new()
    ));
    let store: Arc<dyn SessionStore> = Arc::new(FileSessionStore::new(session_dir.clone()));
    let session = SessionId::new();

    let model = spec.build(config)?;
    let engine = braze_engine::Engine::new(
        model,
        tools,
        Arc::clone(&store),
        Box::new(SimpleContextCompactor::default()),
        Box::new(braze_events::ChannelTaskNotifier::new()),
        SYSTEM_PROMPT.to_string(),
        config.max_tokens,
    );

    let started = Instant::now();
    let run_result = engine
        .run_turn(&session, &task.prompt, &mut |_text| {})
        .await
        .map_err(|err| err.to_string());
    let wall_time = started.elapsed();

    let events = match store.load(&session).await {
        Ok(events) => events,
        // A session that never got a single event persisted (e.g. the
        // model call failed before anything was appended) — treat as
        // an empty log rather than a harness-level failure.
        Err(braze_session::SessionError::NotFound(_)) => Vec::new(),
        Err(err) => return Err(err.into()),
    };

    let _ = tokio::fs::remove_dir_all(&session_dir).await;

    Ok(compute_metrics(
        &spec.display_name(config),
        task,
        &events,
        wall_time,
        run_result,
    ))
}
