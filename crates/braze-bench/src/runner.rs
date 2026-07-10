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

use crate::backend_spec::{BackendSpec, SamplingSpec};
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

/// Always denies, after persisting the same `PermissionRequested`/
/// `PermissionDecided` pair the real confirmation prompts do (see
/// `braze-cli::TerminalConfirmationPrompt`, `braze-tui::approval`) —
/// without this, a denial never shows up in the session log, so
/// `metrics::compute_metrics`'s `permission_denials` count stays stuck at
/// 0 and the denial gets miscounted as a `tool_execution_failures`
/// instead (the engine already appended `ToolCallStarted` before the
/// tool's own dispatch path rejects it). See N-35,
/// docs/AUDITORIA-2026-07-v2.md.
///
/// Combined with a `WorkdirAllowlist` scoped to a throwaway sandbox
/// directory, this means: safe/reversible actions (reads, writes inside
/// the sandbox, allowlisted shell commands) proceed exactly as they would
/// interactively, while anything the classifier flags `Irreversible` — a
/// hallucinated `dd`/`curl`/`mv`, a write outside the sandbox — is
/// refused before it ever runs for real.
struct DenyAll {
    session: SessionId,
    store: Arc<dyn SessionStore>,
}

#[async_trait]
impl ConfirmationPrompt for DenyAll {
    async fn confirm(&self, action: &ActionDescriptor) -> bool {
        let key = braze_permissions::derive_permission_key(action);

        // Best-effort, same as the real prompts: a session-store hiccup
        // here must not change the (always-deny) decision itself.
        let _ = self
            .store
            .append(
                &self.session,
                &braze_events::AgentEvent::PermissionRequested {
                    action: action.to_string(),
                    reversible: false,
                    key: key.clone(),
                },
            )
            .await;

        let _ = self
            .store
            .append(
                &self.session,
                &braze_events::AgentEvent::PermissionDecided {
                    action: action.to_string(),
                    allowed: false,
                    key,
                },
            )
            .await;

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
    sampling: SamplingSpec,
) -> Result<TaskResult, BenchError> {
    let sandbox = TaskSandbox::new(task)?;

    let session_dir = std::env::temp_dir().join(format!(
        "braze-bench-session-{}-{}",
        std::process::id(),
        SessionId::new()
    ));
    let store: Arc<dyn SessionStore> = Arc::new(FileSessionStore::new(session_dir.clone()));
    let session = SessionId::new();

    // E1 (docs/AUDITORIA-2026-07-v3.md): a `+ablate:` suffix on `spec`
    // overrides whichever of these knobs it names; everything else falls
    // through to `config`'s own value, same as a plain (unablated) spec.
    let ablation = spec.ablation();
    let post_edit_check_enabled =
        !config.disable_post_edit_check && !ablation.disable_post_edit_check;
    let textual_rescue_enabled =
        !config.disable_textual_tool_call_rescue && !ablation.disable_textual_rescue;
    let tactical_window = ablation.tactical_window.unwrap_or(config.tactical_window);
    let tactical_compaction_threshold = ablation
        .tactical_compaction_threshold
        .unwrap_or(config.tactical_compaction_threshold);
    let best_of_n = ablation.best_of_n.unwrap_or(config.best_of_n);

    let allowlist = WorkdirAllowlist::new(sandbox.path());
    let classifier = DefaultClassifier::new(WorkdirAllowlist::new(sandbox.path()));
    let deny_all = DenyAll {
        session,
        store: Arc::clone(&store),
    };
    let guard = PermissionGuard::new(allowlist, Box::new(classifier), Box::new(deny_all));
    // `with_workdir`, not `new`: the bench binary's own process cwd is
    // wherever it happened to be launched from, not this task's sandbox —
    // using `new` (which defaults to the process cwd) would silently
    // decouple the guard's `WorkdirAllowlist` (scoped to the sandbox
    // above) from where the tools' actual I/O lands. See
    // docs/AUDITORIA-2026-07.md hallazgo F1.
    //
    // `with_post_edit_check` here (E1c, docs/AUDITORIA-2026-07-v3.md): this
    // call was previously missing entirely, so every bench run had the
    // guardrail permanently ON regardless of `Config::disable_post_edit_check`
    // — a real `braze` invocation with that flag set measured a different
    // harness than the one the bench reported on.
    let tools_provider = braze_tools_local::LocalToolsProvider::with_workdir(guard, sandbox.path())
        .with_post_edit_check(post_edit_check_enabled)
        .with_edit_strict_mode(ablation.edit_strict_mode);
    let tools = braze_tools_core::ToolRegistry::new(vec![Box::new(tools_provider)]);

    let model = spec.build_agent_model(config, sampling)?;
    // N-36 (docs/AUDITORIA-2026-07-v2.md): the exact same anti-loop system
    // prompt `braze chat`/`braze run` build by default — a bare one-line
    // prompt with no tool-use guidance measured a different (worse)
    // system than the one users actually run. D1
    // (docs/AUDITORIA-2026-07-v3.md): mirrors production's model-family
    // hint. I-4 (docs/AUDITORIA-2026-07-v6.md): the hint is name-based
    // now — every executor passes its model name and
    // `ModelFamily::from_model_name` decides (Generic = no hint), same
    // ungating braze-cli got; an OpenRouter-served GLM/Qwen needs its
    // native-template hint exactly like an Ollama-served one.
    let model_hint = Some(spec.executor_model_name(config));
    // No references (opencode-10): the bench sandbox is hermetic by
    // design — a user's reference dirs leaking into the measured system
    // prompt would make pass rates depend on local config.
    let system_prompt =
        braze_config::default_system_prompt(sandbox.path(), model_hint.as_deref(), &[]);

    // N-36: mirrors `braze-cli::main.rs`'s own Ollama-only context budget
    // — without it, a bench pass rate for an Ollama backend measured a
    // context-management regime production never actually uses. Keyed on
    // the *executor* being Ollama (`ollama_models` also reports a local
    // planner, but the budget protects the executor's context window —
    // production keys it on `default_backend` the same way). Computed
    // here, before `tools`/`system_prompt` move into `Engine::new` below
    // (hallazgo B4, docs/AUDITORIA-2026-07-v3.md: the margin needs the
    // real system prompt length plus the size of every advertised tool
    // stub, not a fixed constant).
    let ollama_budget = if spec.executor_is_ollama() {
        let tool_definitions_bytes =
            braze_tools_core::tool_stub_definition_bytes(&tools.all_stubs_lossy().await);
        Some(braze_config::ollama_context_budget_tokens(
            config.ollama_num_ctx,
            config.max_tokens,
            &system_prompt,
            tool_definitions_bytes,
        ))
    } else {
        None
    };

    // C10 (docs/AUDITORIA-2026-07.md): mirrors braze-cli's wiring, so a
    // bench run measures the same tactical window/threshold/best_of_n
    // behavior a real `braze` invocation with this config would use —
    // modulo whatever `ablation` overrides (E1).
    let mut engine = braze_engine::Engine::new(
        model,
        tools,
        Arc::clone(&store),
        Box::new(SimpleContextCompactor::new(tactical_window)),
        Box::new(braze_events::ChannelTaskNotifier::new()),
        system_prompt,
        config.max_tokens,
    )
    .with_tactical_compaction_threshold(tactical_compaction_threshold)
    .with_best_of_n(best_of_n)
    .with_textual_rescue_enabled(textual_rescue_enabled)
    // E1 + opencode ítem 2 (docs/AUDITORIA-2026-07-v6.md § roadmap
    // Paquete 1): the two levers the paper's ablation matrix couldn't
    // turn off before — the ACI collapse (`+ablate:no-prune`) and
    // tactical compaction (`+ablate:no-compaction`).
    .with_observation_collapse_enabled(!ablation.disable_observation_collapse)
    .with_compaction_enabled(!ablation.disable_compaction)
    // A′.2 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.2):
    // the announced-deadline notes, ablatable so their effect on
    // TurnBudgetExhausted/iteration-cap aborts is measurable.
    .with_harness_notes_enabled(!ablation.disable_harness_notes)
    // B′ (docs/harness-engineering-hooks-skills-2026-07-10.md § Parte
    // II): audit-only — logs the prompt-budget breakdown per request
    // under `RUST_LOG=braze_engine=info`, the same channel the other
    // lever activations already use. Zero effect on results by
    // construction (read-only hook).
    .with_hook(std::sync::Arc::new(braze_engine::PromptBudgetAuditHook))
    // C10: mirrors braze-cli's wiring. `max_turn_iterations`/
    // `planner_max_tokens` were a pre-existing mirror gap (wired in
    // production since opencode ítem 1 but never here — a bench run
    // measured the hardcoded defaults regardless of config);
    // `max_turn_total_tokens` is the v4 P0.2 breaker, new in Paquete 3.
    .with_max_turn_iterations(config.max_turn_iterations as usize)
    .with_planner_max_tokens(config.planner_max_tokens)
    .with_max_turn_total_tokens(config.max_turn_total_tokens);

    if let Some(full_observations) = ablation.tactical_full_observations {
        engine = engine.with_tactical_full_observations(full_observations);
    }

    if let Some(budget) = ollama_budget {
        engine = engine.with_context_budget(budget);
    }

    // PLAN.md § "Split planificador/ejecutor", oleada 4: a spec with a
    // `+plan:` suffix runs the same engine with the planner attached —
    // baseline and planned variant differ in exactly one thing.
    // `+ablate:no-planner` (E1) runs the row WITHOUT attaching it while
    // keeping the `+plan:` display identity, so the pair lines up in the
    // report as the exact same config minus one lever.
    if !ablation.disable_planner
        && let Some(planner) = spec.build_planner(config, sampling)?
    {
        engine = engine.with_planner(planner);
    }

    let started = Instant::now();
    let run_outcome = match tokio::time::timeout(
        timeout,
        engine.run_turn(&session, &task.prompt, &mut braze_events::NoopObserver),
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
                .all(|(relative_path, expected_substrings)| {
                    let Ok(contents) =
                        std::fs::read_to_string(sandbox.path().join(relative_path))
                    else {
                        return false;
                    };
                    // Every expected substring must match as a bounded
                    // token — one miss fails the whole file, matching
                    // the AND semantics the field's doc comment pins.
                    expected_substrings
                        .iter()
                        .all(|needle| crate::metrics::contains_as_a_bounded_token(&contents, needle))
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
        // Paquete 3: `None` when the spec's models aren't priced (or a
        // composite bills at mixed rates) — the row reports no cost
        // estimate rather than a guessed one.
        spec.resolve_pricing(config),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_events::AgentEvent;
    use braze_permissions::ActionDescriptor;

    fn temp_store() -> (Arc<dyn SessionStore>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-runner-test-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        (Arc::new(FileSessionStore::new(dir.clone())), dir)
    }

    /// Regression test for N-35 (docs/AUDITORIA-2026-07-v2.md): `DenyAll`
    /// must persist the same `PermissionRequested`/`PermissionDecided`
    /// pair the real confirmation prompts do — otherwise a bench run's
    /// denials never show up in the session log, and
    /// `metrics::compute_metrics`'s `permission_denials` count (which
    /// scans exactly those events) is stuck at 0 no matter how many
    /// actions actually got refused.
    #[tokio::test]
    async fn deny_all_persists_the_denial_before_returning_false() {
        let (store, dir) = temp_store();
        let session = SessionId::new();
        let deny_all = DenyAll {
            session,
            store: Arc::clone(&store),
        };

        let action = ActionDescriptor::DeleteFile {
            path: std::path::PathBuf::from("/tmp/x"),
        };
        let allowed = deny_all.confirm(&action).await;
        assert!(!allowed);

        let events = store.load(&session).await.expect("load events");
        assert!(matches!(events[0], AgentEvent::PermissionRequested { .. }));
        match &events[1] {
            AgentEvent::PermissionDecided { allowed, .. } => assert!(!allowed),
            other => panic!("expected PermissionDecided, got {other:?}"),
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
