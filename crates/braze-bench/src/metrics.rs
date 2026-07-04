//! Turns one task run's persisted `AgentEvent` log into a comparable
//! verdict. Pure and synchronous on purpose — no model, no I/O — so it's
//! testable with hand-built event logs the same way
//! `braze-session::simple_compactor`'s tests are.

use std::collections::HashSet;
use std::time::Duration;

use braze_events::AgentEvent;
use serde::Serialize;

use crate::task::TaskDef;

#[derive(Debug, Clone, Serialize)]
pub struct TaskResult {
    pub backend: String,
    pub task_id: String,
    pub converged: bool,
    pub run_error: Option<String>,
    pub tool_calls_total: u32,
    pub schema_validation_failures: u32,
    pub tool_execution_failures: u32,
    pub permission_denials: u32,
    pub expected_tool_called: Option<bool>,
    pub expected_text_found: Option<bool>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub wall_time_ms: u128,
    pub passed: bool,
}

/// Derives a [`TaskResult`] for one (task, backend) run from its
/// persisted event log plus the `Result` `Engine::run_turn` itself
/// returned (kept separate from the log because a hard model/tool error
/// mid-turn can abort before every expected event lands).
pub fn compute_metrics(
    backend: &str,
    task: &TaskDef,
    events: &[AgentEvent],
    wall_time: Duration,
    run_result: Result<(), String>,
) -> TaskResult {
    let started_ids: HashSet<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallStarted { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();

    let tool_call_names: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AssistantToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    let mut schema_validation_failures = 0u32;
    let mut tool_execution_failures = 0u32;
    for event in events {
        if let AgentEvent::ToolCallCompleted { id, result } = event
            && result.is_error
        {
            if started_ids.contains(id.as_str()) {
                tool_execution_failures += 1;
            } else {
                schema_validation_failures += 1;
            }
        }
    }

    let permission_denials = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::PermissionDecided { allowed: false, .. }))
        .count() as u32;

    let (input_tokens, output_tokens) =
        events
            .iter()
            .fold((0u32, 0u32), |(inp, out), event| match event {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => (inp + input_tokens, out + output_tokens),
                _ => (inp, out),
            });

    let final_text = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AssistantText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let converged = run_result.is_ok();
    let run_error = run_result.err();

    let expected_tool_called = task
        .expect_tool_call
        .as_deref()
        .map(|expected| tool_call_names.contains(&expected));

    let expected_text_found = task
        .expect_text_contains
        .as_deref()
        .map(|expected| final_text.to_lowercase().contains(&expected.to_lowercase()));

    let passed = converged
        && expected_tool_called.unwrap_or(true)
        && (!task.expect_no_tool_call || tool_call_names.is_empty())
        && expected_text_found.unwrap_or(true);

    TaskResult {
        backend: backend.to_string(),
        task_id: task.id.clone(),
        converged,
        run_error,
        tool_calls_total: tool_call_names.len() as u32,
        schema_validation_failures,
        tool_execution_failures,
        permission_denials,
        expected_tool_called,
        expected_text_found,
        input_tokens,
        output_tokens,
        wall_time_ms: wall_time.as_millis(),
        passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::ToolResult;
    use std::collections::HashMap;

    fn task(
        expect_tool_call: Option<&str>,
        expect_no_tool_call: bool,
        expect_text_contains: Option<&str>,
    ) -> TaskDef {
        TaskDef {
            id: "t".to_string(),
            prompt: "irrelevant".to_string(),
            setup_files: HashMap::new(),
            expect_tool_call: expect_tool_call.map(str::to_string),
            expect_no_tool_call,
            expect_text_contains: expect_text_contains.map(str::to_string),
        }
    }

    fn zero() -> Duration {
        Duration::from_millis(0)
    }

    #[test]
    fn a_clean_text_only_turn_with_no_expectations_passes() {
        let events = vec![
            AgentEvent::UserMessage {
                text: "hola".to_string(),
            },
            AgentEvent::AssistantText {
                text: "mundo".to_string(),
            },
        ];
        let result = compute_metrics(
            "ollama:x",
            &task(None, false, None),
            &events,
            zero(),
            Ok(()),
        );
        assert!(result.passed);
        assert!(result.converged);
        assert_eq!(result.tool_calls_total, 0);
    }

    #[test]
    fn expected_tool_call_that_happened_passes() {
        let events = vec![
            AgentEvent::AssistantToolCall {
                id: "1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::ToolCallStarted {
                id: "1".to_string(),
                name: "read_file".to_string(),
                background: true,
            },
            AgentEvent::ToolCallCompleted {
                id: "1".to_string(),
                result: ToolResult {
                    tool_call_id: "1".to_string(),
                    content: "contenido".to_string(),
                    is_error: false,
                },
            },
            AgentEvent::AssistantText {
                text: "listo".to_string(),
            },
        ];
        let result = compute_metrics(
            "ollama:x",
            &task(Some("read_file"), false, None),
            &events,
            zero(),
            Ok(()),
        );
        assert!(result.passed);
        assert_eq!(result.expected_tool_called, Some(true));
        assert_eq!(result.tool_calls_total, 1);
        assert_eq!(result.schema_validation_failures, 0);
        assert_eq!(result.tool_execution_failures, 0);
    }

    #[test]
    fn expected_tool_call_that_never_happened_fails() {
        let events = vec![AgentEvent::AssistantText {
            text: "no hice nada".to_string(),
        }];
        let result = compute_metrics(
            "ollama:x",
            &task(Some("read_file"), false, None),
            &events,
            zero(),
            Ok(()),
        );
        assert!(!result.passed);
        assert_eq!(result.expected_tool_called, Some(false));
    }

    #[test]
    fn schema_rejected_call_is_counted_separately_from_execution_failure() {
        let events = vec![
            // Rejected before dispatch: no ToolCallStarted for this id.
            AgentEvent::AssistantToolCall {
                id: "1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            },
            AgentEvent::ToolCallCompleted {
                id: "1".to_string(),
                result: ToolResult {
                    tool_call_id: "1".to_string(),
                    content: "schema validation failed".to_string(),
                    is_error: true,
                },
            },
            // Dispatched but failed at runtime: has a ToolCallStarted.
            AgentEvent::AssistantToolCall {
                id: "2".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({"text": "hi"}),
            },
            AgentEvent::ToolCallStarted {
                id: "2".to_string(),
                name: "echo".to_string(),
                background: true,
            },
            AgentEvent::ToolCallCompleted {
                id: "2".to_string(),
                result: ToolResult {
                    tool_call_id: "2".to_string(),
                    content: "boom".to_string(),
                    is_error: true,
                },
            },
        ];
        let result = compute_metrics(
            "ollama:x",
            &task(None, false, None),
            &events,
            zero(),
            Ok(()),
        );
        assert_eq!(result.schema_validation_failures, 1);
        assert_eq!(result.tool_execution_failures, 1);
    }

    #[test]
    fn permission_denials_are_counted() {
        let events = vec![
            AgentEvent::PermissionRequested {
                action: "run `dd if=/dev/zero of=/dev/sda`".to_string(),
                reversible: false,
                key: None,
            },
            AgentEvent::PermissionDecided {
                action: "run `dd if=/dev/zero of=/dev/sda`".to_string(),
                allowed: false,
                key: None,
            },
        ];
        let result = compute_metrics(
            "ollama:x",
            &task(None, false, None),
            &events,
            zero(),
            Ok(()),
        );
        assert_eq!(result.permission_denials, 1);
    }

    #[test]
    fn expect_no_tool_call_fails_when_a_tool_was_called() {
        let events = vec![AgentEvent::AssistantToolCall {
            id: "1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({}),
        }];
        let result = compute_metrics("ollama:x", &task(None, true, None), &events, zero(), Ok(()));
        assert!(!result.passed);
    }

    #[test]
    fn expect_no_tool_call_passes_when_no_tool_was_called() {
        let events = vec![AgentEvent::AssistantText {
            text: "4".to_string(),
        }];
        let result = compute_metrics(
            "ollama:x",
            &task(None, true, Some("4")),
            &events,
            zero(),
            Ok(()),
        );
        assert!(result.passed);
    }

    #[test]
    fn expect_text_contains_is_case_insensitive() {
        let events = vec![AgentEvent::AssistantText {
            text: "La respuesta es CUATRO".to_string(),
        }];
        let result = compute_metrics(
            "ollama:x",
            &task(None, false, Some("cuatro")),
            &events,
            zero(),
            Ok(()),
        );
        assert_eq!(result.expected_text_found, Some(true));
        assert!(result.passed);
    }

    #[test]
    fn a_run_error_fails_the_task_regardless_of_other_expectations() {
        let events: Vec<AgentEvent> = vec![];
        let result = compute_metrics(
            "ollama:x",
            &task(None, false, None),
            &events,
            zero(),
            Err("model backend timed out".to_string()),
        );
        assert!(!result.passed);
        assert!(!result.converged);
        assert_eq!(result.run_error.as_deref(), Some("model backend timed out"));
    }

    #[test]
    fn token_usage_is_summed_across_rounds() {
        let events = vec![
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 2,
                stop_reason: Some("end_turn".to_string()),
            },
            AgentEvent::Usage {
                input_tokens: 15,
                output_tokens: 3,
                stop_reason: Some("end_turn".to_string()),
            },
        ];
        let result = compute_metrics(
            "ollama:x",
            &task(None, false, None),
            &events,
            zero(),
            Ok(()),
        );
        assert_eq!(result.input_tokens, 25);
        assert_eq!(result.output_tokens, 5);
    }
}
