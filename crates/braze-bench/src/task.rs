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
    /// A proxy for "did the model attempt the right approach" — doesn't
    /// verify the attempt actually succeeded (a failed or schema-rejected
    /// call still counts). Prefer `expect_file_contains` for tasks whose
    /// point is a filesystem outcome (writes/edits); reserve this for
    /// tasks that only care which tool got reached for.
    #[serde(default)]
    pub expect_tool_call: Option<String>,
    /// If true, the task only passes if NO tool was called at all — e.g.
    /// to check a small model doesn't reach for a tool on a trivial
    /// question it could just answer directly.
    #[serde(default)]
    pub expect_no_tool_call: bool,
    /// If set, the task only passes if the assistant's final text
    /// contains this as a bounded token — case-insensitive, and (E4,
    /// docs/AUDITORIA-2026-07-v3.md) not merely embedded inside a larger
    /// alphanumeric run (`"2"` no longer matches inside `"v2"`, e.g. a
    /// setup file named `informe_final_v2.txt`). See
    /// `metrics::contains_as_a_bounded_token`.
    #[serde(default)]
    pub expect_text_contains: Option<String>,
    /// If non-empty, the task only passes if every named file (path
    /// relative to the sandbox root) exists and contains the given
    /// substring (same bounded-token matching as `expect_text_contains`)
    /// — checked against the sandbox's actual filesystem state after the
    /// run, not just "was some tool called". This is what makes a
    /// write/edit task's pass/fail track the real outcome instead of a
    /// proxy that a failed or no-op call could still satisfy.
    #[serde(default)]
    pub expect_file_contains: HashMap<String, String>,
    /// Optional free-form label (e.g. `"single_tool"`, `"multi_step"`,
    /// `"error_recovery"`) grouping tasks by the kind of capability they
    /// probe, so a report can break results down by skill instead of only
    /// by backend — a flat pass-rate can't show *where* a model's
    /// capability actually ends. Purely descriptive: never affects
    /// pass/fail.
    #[serde(default)]
    pub skill: Option<String>,
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
            skill = "single_tool"

            [tasks.setup_files]
            "notas.txt" = "uno\ndos\ntres\n"

            [tasks.expect_file_contains]
            "notas.txt" = "tres"
        "#;
        let suite: TaskSuiteFile = toml::from_str(toml_src).unwrap();
        assert_eq!(suite.tasks.len(), 1);
        let task = &suite.tasks[0];
        assert_eq!(task.id, "read_file_basic");
        assert_eq!(task.expect_tool_call.as_deref(), Some("read_file"));
        assert_eq!(task.expect_text_contains.as_deref(), Some("3"));
        assert!(!task.expect_no_tool_call);
        assert_eq!(task.skill.as_deref(), Some("single_tool"));
        assert_eq!(
            task.expect_file_contains
                .get("notas.txt")
                .map(String::as_str),
            Some("tres")
        );
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
        assert!(task.expect_file_contains.is_empty());
        assert_eq!(task.skill, None);
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

    /// Regression test for F8: the shipped `default.toml` must actually
    /// parse, and must cover more than the "single_tool" floor — a
    /// gradient with only one difficulty level can't show *where* a small
    /// model's capability ends.
    #[test]
    fn default_suite_parses_and_covers_a_difficulty_gradient() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/default.toml");
        let tasks = load_suite(&path).expect("default.toml must parse");

        assert!(
            tasks.len() >= 18,
            "expected at least 18 tasks, got {}",
            tasks.len()
        );

        // At least one task's pass/fail is verified against the sandbox's
        // real filesystem state, not just "some tool was called" (F4).
        assert!(tasks.iter().any(|t| !t.expect_file_contains.is_empty()));

        // E3 (docs/AUDITORIA-2026-07-v3.md): a skill with n=1 has zero
        // statistical power — a single pass/fail is indistinguishable
        // from sampling noise. Every skill besides the single_tool floor
        // must have at least 3 tasks.
        let mut skill_counts: std::collections::HashMap<&str, usize> = Default::default();
        for skill in tasks.iter().filter_map(|t| t.skill.as_deref()) {
            *skill_counts.entry(skill).or_insert(0) += 1;
        }
        for expected in [
            "single_tool",
            "no_tool",
            "multi_step",
            "error_recovery",
            "distractor_selection",
        ] {
            let count = skill_counts.get(expected).copied().unwrap_or(0);
            assert!(
                count >= 3,
                "expected at least 3 '{expected}' tasks for statistical power, got {count} \
                 (skill counts: {skill_counts:?})"
            );
        }

        // Editing (write_file/edit_file outcomes) was underrepresented
        // relative to read/grep/glob — at least 2 tasks must check the
        // sandbox's real file content after an edit_file-shaped task.
        let editing_tasks = tasks
            .iter()
            .filter(|t| !t.expect_file_contains.is_empty())
            .count();
        assert!(
            editing_tasks >= 2,
            "expected at least 2 tasks verifying real file content, got {editing_tasks}"
        );
    }
}
