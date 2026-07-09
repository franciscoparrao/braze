//! External baseline harness support (E2, docs/AUDITORIA-2026-07-v3.md):
//! without an anchor outside `braze` itself, every `braze-bench` sweep
//! only ever measures `braze` against `braze` — different backends,
//! different ablations, never a *different harness* (e.g. mini-swe-agent,
//! the reference ACI implementation) solving the same tasks. That's the
//! comparison a paper claim like "our harness improves X over baseline Y"
//! actually needs.
//!
//! This module defines the CONTRACT a baseline adapter must satisfy —
//! [`ExternalHarness`], its outcome type, and how that outcome folds into
//! the same [`TaskResult`] shape `runner::run_task` produces, so both
//! rows can sit in the same report/JSON without a separate code path in
//! `report.rs`.
//!
//! **Deliberately not wired to a live adapter.** mini-swe-agent (or any
//! other baseline) isn't installed in this environment, and installing
//! third-party tooling isn't something to do without being asked. Wiring
//! a real one is: implement [`ExternalHarness`] for it (typically
//! shelling out to its CLI inside the sandbox directory, same convention
//! `stop_ollama_model` in `main.rs` already uses for subprocess calls),
//! then add a `--external <name>=<command>` spec form parsed alongside
//! `--backends` in `main.rs`, converting via
//! [`external_outcome_to_task_result`] in place of `metrics::compute_metrics`
//! for that row. Kept `#[allow(dead_code)]` at the module level rather
//! than half-wiring a CLI flag with nothing real behind it.

#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use crate::metrics::{FailureCause, TaskResult};
use crate::task::TaskDef;

/// One task run through an external baseline harness, executed as a
/// subprocess rather than through `braze_engine::Engine` — implement this
/// for a specific tool (mini-swe-agent, ...) to slot it into a sweep
/// alongside braze's own backends.
#[async_trait::async_trait]
pub trait ExternalHarness: Send + Sync {
    /// Name shown in the report's `backend` column, e.g.
    /// `"external:mini-swe-agent"`.
    fn name(&self) -> String;

    /// Runs `task.prompt` against this harness inside `sandbox_dir`
    /// (already populated with `task.setup_files`, same convention
    /// `TaskSandbox` uses for braze's own runs) — implementations are
    /// free to shell out to any subprocess. Must respect `timeout`
    /// itself: the caller does not wrap this in its own
    /// `tokio::time::timeout`, since an external process may need a
    /// graceful-shutdown signal rather than a hard kill on expiry.
    async fn run(
        &self,
        task: &TaskDef,
        sandbox_dir: &Path,
        timeout: Duration,
    ) -> ExternalRunOutcome;
}

/// What an [`ExternalHarness::run`] call produced — deliberately much
/// thinner than braze's own `RunOutcome`/`TaskResult`: an external
/// harness exposes no `AgentEvent`-shaped instrumentation (tool call
/// counts, model rounds, schema validation failures, token usage, ...),
/// so [`external_outcome_to_task_result`] reports those as "not
/// measured" (zeroed) for that row rather than fabricating them.
#[derive(Debug, Clone)]
pub struct ExternalRunOutcome {
    /// Whatever the harness produced as its final answer (its stdout, or
    /// a designated output file's contents) — checked against
    /// `expect_text_contains` the same way `AssistantText` is for
    /// braze's own runs.
    pub final_text: String,
    pub wall_time: Duration,
    /// `Some(message)` if the process failed to run at all (spawn error,
    /// crashed, timed out) — `None` means it completed, and `final_text`
    /// plus the sandbox's filesystem state are meaningful to check
    /// against the task's assertions.
    pub run_error: Option<String>,
}

/// Turns an [`ExternalRunOutcome`] into the same [`TaskResult`] shape
/// braze's own runs produce (mirrors `metrics::compute_metrics`'s
/// `expect_text_contains` logic, applied to `outcome.final_text`), so
/// `report.rs` compares both without a separate table or code path.
///
/// `expect_tool_call`/`expect_no_tool_call` are never evaluated here — a
/// black-box external harness doesn't expose *how* it solved a task
/// through this contract, only its final answer and the sandbox's
/// resulting filesystem state. Reported as not-applicable (`None`) rather
/// than failed, so a `single_tool`/`no_tool` task isn't structurally
/// unwinnable for every external harness regardless of whether it
/// actually solved the task.
pub fn external_outcome_to_task_result(
    backend_name: &str,
    task: &TaskDef,
    repetition: u32,
    outcome: ExternalRunOutcome,
    expected_files_found: Option<bool>,
) -> TaskResult {
    if let Some(run_error) = outcome.run_error {
        return TaskResult {
            backend: backend_name.to_string(),
            task_id: task.id.clone(),
            skill: task.skill.clone(),
            repetition,
            converged: false,
            run_error: Some(run_error),
            failure_cause: Some(FailureCause::ModelBackendError),
            tool_calls_total: 0,
            schema_validation_failures: 0,
            tool_execution_failures: 0,
            permission_denials: 0,
            rounds: 0,
            planned: false,
            expected_tool_called: None,
            expected_text_found: None,
            expected_files_found,
            // No `AgentEvent` log for a black-box external harness, so
            // neither rounds nor tokens are measured — any
            // `expect_max_rounds`/`expect_max_tokens` budget stays
            // `None` (not evaluated) rather than `Some(false)` (blown),
            // same "not reported" contract as the cache fields below.
            expected_rounds_within_budget: None,
            expected_tokens_within_budget: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: None,
            cache_write_tokens: None,
            wall_time_ms: outcome.wall_time.as_millis(),
            passed: false,
        };
    }

    // `expect_tool_call`/`expect_no_tool_call` are mechanism assertions —
    // they ask *how* a task was solved, which a black-box external
    // harness never exposes through this contract. Only the
    // outcome-based assertions (`expect_text_contains`,
    // `expect_file_contains`) transfer; mechanism ones are reported as
    // not applicable (`None`) rather than failed, so a `single_tool` or
    // `no_tool` task doesn't become structurally unwinnable for every
    // external harness regardless of whether it actually solved the task.
    let expected_tool_called = None;
    let expected_text_found = task.expect_text_contains.as_deref().map(|expected| {
        crate::metrics::contains_as_a_bounded_token(
            &outcome.final_text.to_lowercase(),
            &expected.to_lowercase(),
        )
    });

    let assertions_passed =
        expected_text_found.unwrap_or(true) && expected_files_found.unwrap_or(true);

    TaskResult {
        backend: backend_name.to_string(),
        task_id: task.id.clone(),
        skill: task.skill.clone(),
        repetition,
        converged: true,
        run_error: None,
        failure_cause: if assertions_passed {
            None
        } else {
            Some(FailureCause::AssertionText)
        },
        tool_calls_total: 0,
        schema_validation_failures: 0,
        tool_execution_failures: 0,
        permission_denials: 0,
        rounds: 0,
        planned: false,
        expected_tool_called,
        expected_text_found,
        expected_files_found,
        // No round/token counts are measured for a black-box external
        // harness, so any budget assertion stays `None` (not evaluated)
        // — matches the run-error branch above and the `None` semantics
        // `TaskResult::expected_rounds_within_budget` documents.
        expected_rounds_within_budget: None,
        expected_tokens_within_budget: None,
        input_tokens: 0,
        output_tokens: 0,
        // Same as the early-return arm above: a black-box external
        // harness reports no `AgentEvent` log, so no round reported
        // cache tokens. `None`, not `Some(0)`.
        cache_read_tokens: None,
        cache_write_tokens: None,
        wall_time_ms: outcome.wall_time.as_millis(),
        passed: assertions_passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn task(expect_text_contains: Option<&str>, expect_no_tool_call: bool) -> TaskDef {
        TaskDef {
            id: "t".to_string(),
            prompt: "irrelevant".to_string(),
            setup_files: HashMap::new(),
            expect_tool_call: None,
            expect_no_tool_call,
            expect_text_contains: expect_text_contains.map(str::to_string),
            expect_file_contains: HashMap::new(),
            skill: Some("no_tool".to_string()),
            expect_max_rounds: None,
            expect_max_tokens: None,
            expect_max_cost_usd: None,
        }
    }

    #[test]
    fn a_run_error_produces_an_unconverged_result() {
        let outcome = ExternalRunOutcome {
            final_text: String::new(),
            wall_time: Duration::from_millis(50),
            run_error: Some("process exited with signal 9".to_string()),
        };
        let result =
            external_outcome_to_task_result("external:fake", &task(None, false), 0, outcome, None);
        assert!(!result.converged);
        assert!(!result.passed);
        assert_eq!(result.failure_cause, Some(FailureCause::ModelBackendError));
    }

    #[test]
    fn a_successful_run_checks_expect_text_contains_against_final_text() {
        let outcome = ExternalRunOutcome {
            final_text: "La respuesta es 4".to_string(),
            wall_time: Duration::from_millis(500),
            run_error: None,
        };
        let result = external_outcome_to_task_result(
            "external:fake",
            &task(Some("4"), true),
            0,
            outcome,
            None,
        );
        assert!(result.converged);
        assert!(result.passed);
        assert_eq!(result.expected_text_found, Some(true));
    }

    #[test]
    fn a_wrong_answer_fails_the_assertion() {
        let outcome = ExternalRunOutcome {
            final_text: "no lo sé".to_string(),
            wall_time: Duration::from_millis(500),
            run_error: None,
        };
        let result = external_outcome_to_task_result(
            "external:fake",
            &task(Some("4"), true),
            0,
            outcome,
            None,
        );
        assert!(result.converged, "the process itself still ran fine");
        assert!(!result.passed);
        assert_eq!(result.expected_text_found, Some(false));
        assert_eq!(result.failure_cause, Some(FailureCause::AssertionText));
    }

    #[test]
    fn expect_no_tool_call_and_expect_tool_call_are_not_evaluated_for_external_rows() {
        // A task with BOTH mechanism assertions set — a real answer must
        // still pass, since neither is observable through this contract.
        let mut t = task(Some("4"), true);
        t.expect_tool_call = Some("read_file".to_string());
        let outcome = ExternalRunOutcome {
            final_text: "4".to_string(),
            wall_time: Duration::from_millis(200),
            run_error: None,
        };
        let result = external_outcome_to_task_result("external:fake", &t, 0, outcome, None);
        assert!(result.passed);
        assert_eq!(result.expected_tool_called, None);
    }

    #[test]
    fn expect_file_contains_is_forwarded_from_the_sandbox_check() {
        let outcome = ExternalRunOutcome {
            final_text: "listo".to_string(),
            wall_time: Duration::from_millis(500),
            run_error: None,
        };
        let result = external_outcome_to_task_result(
            "external:fake",
            &task(None, false),
            0,
            outcome,
            Some(false),
        );
        assert!(!result.passed);
        assert_eq!(result.expected_files_found, Some(false));
    }
}
