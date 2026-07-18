//! Experimental memory injection for Paper 2.
//!
//! This is deliberately local to `braze-bench`: before promoting
//! `LearnedPlaybook` to a production `braze-memory` API, the paper needs
//! a clean pilot that can inject manual/human playbooks, summaries, and
//! episodic snippets under the same token budget.

use std::path::Path;

use serde::Deserialize;

use crate::error::BenchError;
use crate::task::TaskDef;

const CHARS_PER_TOKEN: usize = 4;
pub const DEFAULT_MEMORY_BUDGET_TOKENS: usize = 500;

#[derive(Debug, Clone)]
pub struct RenderedMemory {
    pub section: String,
    pub tokens_estimate: u32,
}

#[derive(Debug, Deserialize)]
struct LearnedPlaybook {
    title: String,
    task_family: String,
    applies_when: Vec<String>,
    #[serde(default)]
    preconditions: Vec<String>,
    method_steps: Vec<String>,
    verification: Vec<String>,
    avoid: Vec<String>,
    escalate_if: Vec<String>,
}

pub fn resolved_memory_condition(task: &TaskDef) -> Option<String> {
    match (&task.memory_condition, &task.memory_file) {
        (Some(condition), _) => Some(condition.clone()),
        (None, Some(_)) => Some("procedural".to_string()),
        (None, None) => None,
    }
}

pub fn render_task_memory(task: &TaskDef) -> Result<Option<RenderedMemory>, BenchError> {
    let Some(path) = &task.memory_file else {
        return Ok(None);
    };
    let condition = resolved_memory_condition(task).unwrap_or_else(|| "procedural".to_string());
    let raw = std::fs::read_to_string(path)?;
    let budget_tokens = task
        .memory_budget_tokens
        .unwrap_or(DEFAULT_MEMORY_BUDGET_TOKENS);

    let body = match condition.as_str() {
        "procedural" | "human-playbook" | "human_playbook" => {
            render_playbook(path, &raw, &condition)?
        }
        "summary" | "episodic" => render_text_memory(&raw, &condition),
        _ => {
            if raw.trim_start().starts_with('{') {
                render_playbook(path, &raw, &condition)?
            } else {
                render_text_memory(&raw, &condition)
            }
        }
    };

    Ok(
        budget_lines(&body, budget_tokens).map(|section| RenderedMemory {
            tokens_estimate: estimate_tokens(&section),
            section,
        }),
    )
}

fn render_playbook(path: &Path, raw: &str, condition: &str) -> Result<String, BenchError> {
    let playbook: LearnedPlaybook = serde_json::from_str(raw).map_err(|err| {
        BenchError::Startup(format!(
            "invalid LearnedPlaybook JSON in {}: {err}",
            path.display()
        ))
    })?;

    let mut out = String::new();
    push_line(&mut out, &format!("Memory condition: {condition}"));
    push_line(
        &mut out,
        &format!("Procedural playbook: {}", playbook.title),
    );
    push_line(&mut out, &format!("Task family: {}", playbook.task_family));
    push_list(&mut out, "Applies when:", &playbook.applies_when);
    push_list(&mut out, "Preconditions:", &playbook.preconditions);
    push_list(&mut out, "Method:", &playbook.method_steps);
    push_list(&mut out, "Verification:", &playbook.verification);
    push_list(&mut out, "Avoid:", &playbook.avoid);
    push_list(&mut out, "Escalate if:", &playbook.escalate_if);
    Ok(out)
}

fn render_text_memory(raw: &str, condition: &str) -> String {
    let mut out = String::new();
    push_line(&mut out, &format!("Memory condition: {condition}"));
    push_line(&mut out, "Memory payload:");
    for line in raw.lines() {
        push_line(&mut out, line);
    }
    out
}

fn push_list(out: &mut String, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    push_line(out, heading);
    for item in items {
        push_line(out, &format!("- {item}"));
    }
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

fn budget_lines(text: &str, budget_tokens: usize) -> Option<String> {
    let budget_chars = budget_tokens.saturating_mul(CHARS_PER_TOKEN);
    let mut out = String::new();
    let mut used = 0usize;
    for line in text.lines() {
        let len = line.len() + 1;
        if used + len > budget_chars {
            break;
        }
        out.push_str(line);
        out.push('\n');
        used += len;
    }
    let trimmed = out.trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn estimate_tokens(text: &str) -> u32 {
    text.len().div_ceil(CHARS_PER_TOKEN) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn task(memory_file: PathBuf) -> TaskDef {
        TaskDef {
            id: "t".to_string(),
            prompt: "do it".to_string(),
            setup_files: HashMap::new(),
            expect_tool_call: None,
            expect_no_tool_call: false,
            expect_text_contains: None,
            expect_file_contains: HashMap::new(),
            skill: None,
            expect_max_rounds: None,
            expect_max_tokens: None,
            expect_max_cost_usd: None,
            noise_tools: 0,
            synthetic_tools: Vec::new(),
            memory_condition: Some("procedural".to_string()),
            memory_file: Some(memory_file),
            memory_budget_tokens: Some(500),
        }
    }

    fn temp_file(contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-memory-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("playbook.json");
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn renders_a_learned_playbook() {
        let path = temp_file(
            r#"{
              "schema_version": 1,
              "id": "rust-fix",
              "title": "Fix Rust borrow checker errors methodically",
              "lifecycle": "approved",
              "task_family": "rust_compile_repair",
              "applies_when": ["cargo check reports E0502"],
              "failure_signals": ["same compiler error repeats"],
              "method_steps": ["read the whole function", "shorten the borrow scope"],
              "verification": ["cargo check"],
              "avoid": ["blind clone"],
              "escalate_if": ["public API must change"],
              "source": {"origin": "human", "created_at": "2026-07-16"},
              "evidence": {"validated_runs": 0, "failed_runs": 0}
            }"#,
        );
        let rendered = render_task_memory(&task(path.clone())).unwrap().unwrap();
        assert!(rendered.section.contains("Procedural playbook"));
        assert!(rendered.section.contains("shorten the borrow scope"));
        assert!(rendered.tokens_estimate > 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_tiny_budget_drops_whole_lines() {
        let path = temp_file("line one\nline two\nline three\n");
        let mut t = task(path.clone());
        t.memory_condition = Some("summary".to_string());
        t.memory_budget_tokens = Some(8);
        let rendered = render_task_memory(&t).unwrap().unwrap();
        assert!(rendered.section.lines().all(|line| !line.ends_with("th")));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
