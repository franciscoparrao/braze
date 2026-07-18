//! A minimal, from-scratch lead+executor tool-calling loop — deliberately
//! NOT `braze_engine::Engine` — implementing [`ExternalHarness`] so it slots
//! into the same sweep/report pipeline as `braze`'s own rows (`external.rs`).
//! Built for the EMSE review's Issue 1 (`docs/emse-review-2026-07-13-checklist.md`):
//! every comparison in the paper was `braze` against itself; this is the
//! first arm that isn't. See `docs/external-harness-baseline-design.md` for
//! the pre-registered hypothesis and criterion this exists to test.
//!
//! # What this is
//!
//! The same lead/executor *composition pattern* `braze`'s own `+lead:`
//! spec uses (a stronger model opens the first `lead_turns` rounds, then a
//! smaller executor takes over) — reimplemented from scratch here, NOT by
//! reusing [`braze_model::escalation::EscalatingBackend`], so this arm's
//! implementation is genuinely independent of the routing decorator the
//! rest of the paper's `+lead:` results depend on. Real tool execution
//! (`braze_tools_local::LocalToolsProvider`, same six tools, same
//! `WorkdirAllowlist`-scoped sandbox safety boundary and `BenchPrompt`-
//! equivalent permission posture every other `braze-bench` row uses —
//! see `runner.rs`'s `BenchPrompt` doc for why deny-except-cargo-verify
//! is the benchmark's convention, not a leniency difference this arm
//! should introduce) so a pass/fail difference is attributable to the
//! turn loop's design, not to different tools behaving differently.
//!
//! # What this deliberately does NOT have
//!
//! Every lever in `Table~\ref{tab:levers}` of the paper except the lead
//! itself: no per-family textual tool-call rescue (a malformed call's raw
//! error goes straight back to the model, no repair hints), no observation
//! collapse/tactical compaction (full history every round), no tool
//! deferral (every tool's full schema is sent from round one), no
//! post-edit compiler-feedback guardrail, no best-of-$n$ voting, no
//! harness notes, no task list/planner, no project memory. Also: a short,
//! generic system prompt ([`BARE_SYSTEM_PROMPT`], four sentences) rather
//! than `braze_config::prompt::default_system_prompt` (463 lines of
//! braze-specific guidance) — the prompt is exactly as much of "the
//! harness's engineering" as the rescue ladder is, and reusing it here
//! would confound the ablation this arm exists to run.

use std::path::Path;
use std::time::{Duration, Instant};

use braze_permissions::{
    ActionDescriptor, ConfirmationPrompt, DefaultClassifier, PermissionGuard, WorkdirAllowlist,
};
use braze_tools_core::ToolProvider;
use braze_tools_local::LocalToolsProvider;
use braze_types::{ContentBlock, Message, Role, ToolCall, ToolStub};

use braze_model::{CompletionEvent, CompletionRequest, ModelBackend, ModelError};

use crate::external::{ExternalHarness, ExternalRunOutcome};
use crate::task::TaskDef;

/// Matches `EscalatingBackend`'s `DEFAULT_LEAD_TURNS` (Goose's
/// `GOOSE_LEAD_TURNS`) — the same proactive-opening width `braze`'s own
/// `+lead:` spec uses by default, so a difference in outcome isn't
/// attributable to a different lead/executor split point.
const LEAD_TURNS: usize = 3;

/// Matches `braze_engine::Engine`'s `MAX_TURN_ITERATIONS` — same round
/// budget as every other arm in this paper's sweeps.
const MAX_ROUNDS: usize = 20;

/// Deliberately short and generic — NOT `braze_config::prompt::default_system_prompt`.
/// See this module's doc comment.
const BARE_SYSTEM_PROMPT: &str = "You are an assistant with access to tools to read, write, and \
edit files, search file contents, and run shell commands. Use them as needed to complete the \
user's task, then reply with your final answer as plain text.";

/// A from-scratch lead+executor tool-calling loop. See the module doc
/// comment for what this is and isn't.
pub struct BareLeadExecutor {
    lead: Box<dyn ModelBackend>,
    executor: Box<dyn ModelBackend>,
    display_name: String,
}

impl BareLeadExecutor {
    pub fn new(
        lead: Box<dyn ModelBackend>,
        executor: Box<dyn ModelBackend>,
        display_name: impl Into<String>,
    ) -> Self {
        Self {
            lead,
            executor,
            display_name: display_name.into(),
        }
    }

    fn backend_for_round(&self, round: usize) -> &dyn ModelBackend {
        if round < LEAD_TURNS {
            self.lead.as_ref()
        } else {
            self.executor.as_ref()
        }
    }
}

/// Mirrors `runner.rs`'s `BenchPrompt` verdict — deny everything
/// irreversible except the `is_benchable_cargo` carve-out — minus the
/// `SessionStore` event persistence (this loop keeps no `AgentEvent` log
/// at all; `ExternalRunOutcome` reports only final text and wall time,
/// per `external.rs`'s contract). Matching the engine rows' verdict (not
/// e.g. an always-allow prompt, nor a stricter one) keeps the permission
/// posture identical across every `braze-bench` row — a baseline that
/// gets `cargo check` denied while the engine rows have it approved
/// would measure the prompt policy, not the harness. See this module's
/// doc comment.
struct BarePrompt;

#[async_trait::async_trait]
impl ConfirmationPrompt for BarePrompt {
    async fn confirm(&self, action: &ActionDescriptor) -> bool {
        crate::runner::is_benchable_cargo(action)
    }
}

/// Fetches every local tool's full schema up front (no deferral — see
/// module doc comment) and returns them as fully-resolved [`ToolStub`]s.
async fn eager_tool_stubs(provider: &LocalToolsProvider) -> Vec<ToolStub> {
    let stubs = provider.list_stubs().await.unwrap_or_default();
    let mut resolved = Vec::with_capacity(stubs.len());
    for mut stub in stubs {
        if let Ok(Some(schema)) = provider.resolve_schema(&stub.name).await {
            stub.input_schema = Some(schema.input_schema);
        }
        resolved.push(stub);
    }
    resolved
}

/// Drains `stream` into its text and tool-call requests. Per
/// `ModelBackend::complete`'s invariant, the stream ends in `Done` or an
/// `Err` — a stream that ends with neither (silently drops) is treated as
/// an empty completion rather than panicking, since a single malformed
/// round shouldn't crash the whole sweep.
async fn drain_completion(
    mut stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<CompletionEvent, ModelError>> + Send>,
    >,
) -> Result<(String, Vec<ToolCall>), ModelError> {
    use futures::StreamExt;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    while let Some(event) = stream.next().await {
        match event? {
            CompletionEvent::TextDelta(delta) => text.push_str(&delta),
            CompletionEvent::ToolCallRequested {
                id,
                name,
                arguments,
            } => tool_calls.push(ToolCall {
                id,
                name,
                arguments,
            }),
            CompletionEvent::Usage { .. } | CompletionEvent::Done => {}
        }
    }
    Ok((text, tool_calls))
}

#[async_trait::async_trait]
impl ExternalHarness for BareLeadExecutor {
    fn name(&self) -> String {
        format!("external:bare-lead({})", self.display_name)
    }

    async fn run(
        &self,
        task: &TaskDef,
        sandbox_dir: &Path,
        timeout: Duration,
    ) -> ExternalRunOutcome {
        let started = Instant::now();
        let run = tokio::time::timeout(timeout, self.run_inner(task, sandbox_dir)).await;
        let wall_time = started.elapsed();
        match run {
            Ok(Ok(final_text)) => ExternalRunOutcome {
                final_text,
                wall_time,
                run_error: None,
            },
            Ok(Err(err)) => ExternalRunOutcome {
                final_text: String::new(),
                wall_time,
                run_error: Some(err),
            },
            Err(_elapsed) => ExternalRunOutcome {
                final_text: String::new(),
                wall_time,
                run_error: Some("timed out".to_string()),
            },
        }
    }
}

impl BareLeadExecutor {
    async fn run_inner(&self, task: &TaskDef, sandbox_dir: &Path) -> Result<String, String> {
        let allowlist = WorkdirAllowlist::new(sandbox_dir);
        let classifier = DefaultClassifier::new(allowlist);
        let guard = PermissionGuard::new(
            WorkdirAllowlist::new(sandbox_dir),
            Box::new(classifier),
            Box::new(BarePrompt),
        );
        // No post-edit compiler-feedback guardrail — that's one of the
        // levers this arm exists to omit (see module doc comment).
        let tools =
            LocalToolsProvider::with_workdir(guard, sandbox_dir).with_post_edit_check(false);
        let tool_stubs = eager_tool_stubs(&tools).await;

        let mut messages = vec![Message::text(Role::User, task.prompt.clone())];

        for round in 0..MAX_ROUNDS {
            let backend = self.backend_for_round(round);
            let stream = backend
                .complete(CompletionRequest {
                    messages: messages.clone(),
                    tool_stubs: tool_stubs.clone(),
                    system_prompt: BARE_SYSTEM_PROMPT.to_string(),
                    max_tokens: 4096,
                })
                .await
                .map_err(|err| format!("model backend error: {err}"))?;
            let (text, tool_calls) = drain_completion(stream)
                .await
                .map_err(|err| format!("model stream error: {err}"))?;

            if tool_calls.is_empty() {
                // No tool call requested this round — the text is the
                // final answer. Matches `Engine`'s own "no tool calls this
                // round means the turn is done" convention.
                return Ok(text);
            }

            let mut assistant_content: Vec<ContentBlock> = Vec::new();
            if !text.is_empty() {
                assistant_content.push(ContentBlock::Text { text: text.clone() });
            }
            for call in &tool_calls {
                assistant_content.push(ContentBlock::ToolUse {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.arguments.clone(),
                });
            }
            messages.push(Message {
                role: Role::Assistant,
                content: assistant_content,
            });

            let mut result_content: Vec<ContentBlock> = Vec::new();
            for call in &tool_calls {
                // A raw error, no rescue-ladder repair hints — see module
                // doc comment on what this arm deliberately omits.
                let result = match tools.invoke(call).await {
                    Ok(result) => result,
                    Err(err) => braze_types::ToolResult {
                        tool_call_id: call.id.clone(),
                        content: err.to_string(),
                        is_error: true,
                    },
                };
                result_content.push(ContentBlock::ToolResult {
                    tool_use_id: result.tool_call_id,
                    content: result.content,
                    is_error: result.is_error,
                });
            }
            messages.push(Message {
                role: Role::User,
                content: result_content,
            });
        }

        // Iteration cap exhausted without a text-only final round — no
        // harness-note nudge (that's a lever too, see module doc comment).
        // Whatever text the last round produced (possibly empty) is the
        // final answer, same as it would be if the caller simply stopped
        // reading here.
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::pin::Pin;

    use futures::stream;

    fn task(prompt: &str, expect_text_contains: Option<&str>) -> TaskDef {
        TaskDef {
            id: "t".to_string(),
            prompt: prompt.to_string(),
            setup_files: HashMap::new(),
            expect_tool_call: None,
            expect_no_tool_call: false,
            expect_text_contains: expect_text_contains.map(str::to_string),
            expect_file_contains: HashMap::new(),
            skill: Some("no_tool".to_string()),
            expect_max_rounds: None,
            expect_max_tokens: None,
            expect_max_cost_usd: None,
            noise_tools: 0,
            synthetic_tools: Vec::new(),
            memory_condition: None,
            memory_file: None,
            memory_budget_tokens: None,
        }
    }

    /// A canned backend that answers with fixed text and no tool calls —
    /// enough to exercise the loop's happy path without a real model.
    struct CannedBackend {
        name: String,
        reply: String,
    }

    #[async_trait::async_trait]
    impl ModelBackend for CannedBackend {
        fn name(&self) -> &str {
            &self.name
        }

        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            Pin<Box<dyn futures::Stream<Item = Result<CompletionEvent, ModelError>> + Send>>,
            ModelError,
        > {
            let events = vec![
                Ok(CompletionEvent::TextDelta(self.reply.clone())),
                Ok(CompletionEvent::Done),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn a_lead_only_answer_short_circuits_before_any_executor_round() {
        let lead = Box::new(CannedBackend {
            name: "lead".to_string(),
            reply: "4".to_string(),
        });
        let executor = Box::new(CannedBackend {
            name: "executor".to_string(),
            reply: "should never be reached".to_string(),
        });
        let harness = BareLeadExecutor::new(lead, executor, "test");
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-bare-lead-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let outcome = harness
            .run(
                &task("what is 2+2?", Some("4")),
                &dir,
                Duration::from_secs(5),
            )
            .await;

        assert_eq!(outcome.run_error, None);
        assert_eq!(outcome.final_text, "4");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn name_reports_the_bare_lead_prefix_and_display_name() {
        let lead = Box::new(CannedBackend {
            name: "lead".to_string(),
            reply: String::new(),
        });
        let executor = Box::new(CannedBackend {
            name: "executor".to_string(),
            reply: String::new(),
        });
        let harness =
            BareLeadExecutor::new(lead, executor, "ollama:llama3.2:1b+lead:ollama:gemma4:e4b");
        assert_eq!(
            harness.name(),
            "external:bare-lead(ollama:llama3.2:1b+lead:ollama:gemma4:e4b)"
        );
    }
}
