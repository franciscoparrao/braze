//! Runs one task against one backend, end to end, through the real
//! `braze_engine::Engine` — same composition `braze-cli` does at
//! startup, minus MCP servers (determinism: no dependency on what
//! happens to be configured/reachable) and with permission confirmation
//! replaced by [`DenyAll`] (see module doc on why, and PLAN.md's
//! "Hallazgo de diseño no anticipado").

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use braze_config::Config;
use braze_permissions::{
    ActionDescriptor, ConfirmationPrompt, DefaultClassifier, PermissionGuard, WorkdirAllowlist,
};
use braze_session::{FileSessionStore, SessionStore, SimpleContextCompactor};
use braze_types::SessionId;

use crate::backend_spec::BackendSpec;
use crate::error::BenchError;
use crate::metrics::{RunOutcome, TaskResult, compute_metrics};
use crate::sandbox::TaskSandbox;
use crate::task::TaskDef;

/// Wall-clock budget for a single task attempt. A model stuck in a
/// non-convergence loop on CPU-only Ollama has been observed taking
/// upwards of 20 minutes to exhaust `MAX_TURN_ITERATIONS` on its own —
/// without an independent timeout here, one such task can stall an entire
/// sweep instead of being recorded as the (diagnostically useful) failure
/// it is. See docs/AUDITORIA-2026-07.md hallazgo F2.
pub const DEFAULT_TASK_TIMEOUT: Duration = Duration::from_secs(180);

/// Builds the system prompt for one task run, including the sandbox path
/// so the model knows where relative paths resolve to — without this, a
/// model has no way to know its working directory isn't wherever it might
/// otherwise assume.
fn system_prompt(sandbox_path: &std::path::Path) -> String {
    format!(
        "You are braze, an experimental agentic CLI assistant. Working directory: {}.",
        sandbox_path.display()
    )
}

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
    repetition: u32,
    timeout: Duration,
) -> Result<TaskResult, BenchError> {
    let sandbox = TaskSandbox::new(task)?;

    let allowlist = WorkdirAllowlist::new(sandbox.path());
    let classifier = DefaultClassifier::new(WorkdirAllowlist::new(sandbox.path()));
    let guard = PermissionGuard::new(allowlist, Box::new(classifier), Box::new(DenyAll));
    // `with_workdir`, not `new`: the bench binary's own process cwd is
    // wherever it happened to be launched from, not this task's sandbox —
    // using `new` (which defaults to the process cwd) would silently
    // decouple the guard's `WorkdirAllowlist` (scoped to the sandbox
    // above) from where the tools' actual I/O lands. See
    // docs/AUDITORIA-2026-07.md hallazgo F1.
    let tools_provider = braze_tools_local::LocalToolsProvider::with_workdir(guard, sandbox.path());
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
        system_prompt(sandbox.path()),
        config.max_tokens,
    );

    let started = Instant::now();
    let run_outcome = match tokio::time::timeout(
        timeout,
        engine.run_turn(&session, &task.prompt, &mut |_text| {}),
    )
    .await
    {
        Ok(Ok(())) => RunOutcome::Converged,
        Ok(Err(err)) => RunOutcome::Failed(err),
        // The elapsed-timer future itself carries no useful information
        // (just "it didn't finish in time") — the interesting bit is
        // already captured by `RunOutcome::TimedOut`.
        Err(_elapsed) => RunOutcome::TimedOut,
    };
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

    // Checked before the sandbox drops at the end of this function (its
    // `Drop` removes the directory) — this is what makes a write/edit
    // task's pass/fail track the real filesystem outcome instead of only
    // "was some tool called" (see docs/AUDITORIA-2026-07.md hallazgo F4).
    let expected_files_found = if task.expect_file_contains.is_empty() {
        None
    } else {
        Some(
            task.expect_file_contains
                .iter()
                .all(|(relative_path, expected_substring)| {
                    std::fs::read_to_string(sandbox.path().join(relative_path))
                        .map(|contents| contents.contains(expected_substring.as_str()))
                        .unwrap_or(false)
                }),
        )
    };

    Ok(compute_metrics(
        &spec.display_name(config),
        task,
        repetition,
        &events,
        wall_time,
        run_outcome,
        expected_files_found,
    ))
}
