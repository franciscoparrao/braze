//! TOML task-suite format: what `braze-bench` runs through the real
//! `Engine::run_turn` against each configured backend.
//!
//! Kept deliberately small (a prompt plus a few optional pass/fail
//! expectations) rather than a general assertion language — see
//! `metrics::compute_metrics` for how these fields turn into a verdict.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::BenchError;

/// One task in a suite file.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskDef {
    pub id: String,
    pub prompt: String,
    /// Files to write into the task's isolated sandbox directory before
    /// the prompt is sent, keyed by path relative to the sandbox root —
    /// lets tasks like "read notas.txt" have deterministic content
    /// instead of depending on whatever happens to be in the real repo.
    #[serde(default)]
    pub setup_files: HashMap<String, String>,
    /// If set, the task only passes if this tool was called at least once.
    #[serde(default)]
    pub expect_tool_call: Option<String>,
    /// If true, the task only passes if NO tool was called at all — e.g.
    /// to check a small model doesn't reach for a tool on a trivial
    /// question it could just answer directly.
    #[serde(default)]
    pub expect_no_tool_call: bool,
    /// If set, the task only passes if the assistant's final text
    /// contains this substring (case-insensitive).
    #[serde(default)]
    pub expect_text_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskSuiteFile {
    tasks: Vec<TaskDef>,
}

/// Loads and parses a task suite from a TOML file.
pub fn load_suite(path: &Path) -> Result<Vec<TaskDef>, BenchError> {
    let contents = std::fs::read_to_string(path)?;
    let suite: TaskSuiteFile = toml::from_str(&contents)?;
    Ok(suite.tasks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_task_with_every_field_set() {
        let toml_src = r#"
            [[tasks]]
            id = "read_file_basic"
            prompt = "Lee notas.txt y dime cuántas líneas tiene."
            expect_tool_call = "read_file"
            expect_text_contains = "3"

            [tasks.setup_files]
            "notas.txt" = "uno\ndos\ntres\n"
        "#;
        let suite: TaskSuiteFile = toml::from_str(toml_src).unwrap();
        assert_eq!(suite.tasks.len(), 1);
        let task = &suite.tasks[0];
        assert_eq!(task.id, "read_file_basic");
        assert_eq!(task.expect_tool_call.as_deref(), Some("read_file"));
        assert_eq!(task.expect_text_contains.as_deref(), Some("3"));
        assert!(!task.expect_no_tool_call);
        assert_eq!(
            task.setup_files.get("notas.txt").map(String::as_str),
            Some("uno\ndos\ntres\n")
        );
    }

    #[test]
    fn optional_fields_default_when_absent() {
        let toml_src = r#"
            [[tasks]]
            id = "minimal"
            prompt = "hola"
        "#;
        let suite: TaskSuiteFile = toml::from_str(toml_src).unwrap();
        let task = &suite.tasks[0];
        assert!(task.setup_files.is_empty());
        assert_eq!(task.expect_tool_call, None);
        assert!(!task.expect_no_tool_call);
        assert_eq!(task.expect_text_contains, None);
    }

    #[test]
    fn parses_multiple_tasks_in_order() {
        let toml_src = r#"
            [[tasks]]
            id = "a"
            prompt = "primero"

            [[tasks]]
            id = "b"
            prompt = "segundo"
        "#;
        let suite: TaskSuiteFile = toml::from_str(toml_src).unwrap();
        assert_eq!(
            suite
                .tasks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn load_suite_reads_and_parses_a_real_file() {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-test-load-suite-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("suite.toml");
        std::fs::write(
            &path,
            r#"
                [[tasks]]
                id = "only"
                prompt = "hola"
            "#,
        )
        .unwrap();

        let tasks = load_suite(&path).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "only");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_suite_reports_invalid_toml_as_bench_error() {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-test-invalid-toml-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("suite.toml");
        std::fs::write(&path, "this is not valid toml [[[").unwrap();

        let result = load_suite(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
