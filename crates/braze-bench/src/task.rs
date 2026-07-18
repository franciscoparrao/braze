//! TOML task-suite format: what `braze-bench` runs through the real
//! `Engine::run_turn` against each configured backend.
//!
//! Kept deliberately small (a prompt plus a few optional pass/fail
//! expectations) rather than a general assertion language — see
//! `metrics::compute_metrics` for how these fields turn into a verdict.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    /// relative to the sandbox root) exists and contains *every*
    /// substring in its associated list (each matched as a bounded
    /// token, same rule as `expect_text_contains`) — checked against the
    /// sandbox's actual filesystem state after the run, not just "was
    /// some tool called". This is what makes a write/edit task's
    /// pass/fail track the real outcome instead of a proxy that a
    /// failed or no-op call could still satisfy.
    ///
    /// A list (not a single string) per file lets one task assert
    /// multiple independent invariants on the same file — e.g. a coding
    /// task can require that the new method `build_lead` *was* added
    /// AND that the old `+plan:` support wasn't removed in the process.
    /// TOML doesn't permit duplicate keys in a table, so a one-string-
    /// per-file field can't express that without duplicating the path;
    /// the list form is the natural representation.
    #[serde(default)]
    pub expect_file_contains: HashMap<String, Vec<String>>,
    /// If true, the task only passes if `cargo check` exits 0 in the
    /// sandbox AFTER the run — semantic grading for Rust-fix tasks (v8
    /// K-9, docs/AUDITORIA-2026-07-v8.md). Substring needles alone are
    /// both gameable (a needle inside a comment or dead code passes
    /// without compiling) and undercounting (the 2026-07-16 diagnostic
    /// found 4/4 fixes that compiled and 4/4 grader FAILs); this makes
    /// "the code actually compiles" — what the prompt literally asks
    /// for — part of the verdict. Combined as AND with the needles:
    /// needles measure the *shape* of the fix (the memory-transfer
    /// question), this measures the *outcome*.
    #[serde(default)]
    pub expect_cargo_check: bool,
    /// Optional free-form label (e.g. `"single_tool"`, `"multi_step"`,
    /// `"error_recovery"`) grouping tasks by the kind of capability they
    /// probe, so a report can break results down by skill instead of only
    /// by backend — a flat pass-rate can't show *where* a model's
    /// capability actually ends. Purely descriptive: never affects
    /// pass/fail.
    #[serde(default)]
    pub skill: Option<String>,
    /// If set, the task only passes if the turn converged in **at most**
    /// this many model rounds (one `AgentEvent::Usage` per round — see
    /// `TaskResult::rounds`). A budget assertion: a config that passes
    /// the correctness checks but takes 14 rounds to get there is worse
    /// than one that converges in 3, and a flat pass-rate can't tell them
    /// apart. v4 P0.4 (docs/AUDITORIA-2026-07-v4.md): the bench must be
    /// able to say "better" not just "passes".
    #[serde(default)]
    pub expect_max_rounds: Option<u32>,
    /// If set, the task only passes if the turn's total tokens
    /// (`input_tokens + output_tokens` summed across rounds) stayed at
    /// or below this number. Cache-read/write tokens are reported
    /// separately in `TaskResult` and are *not* counted here — they are a
    /// backend-billing concern (H-18, `+ablate:no-caching` for the A/B),
    /// not a model-efficiency concern. Same budget-assertion rationale
    /// as `expect_max_rounds`.
    #[serde(default)]
    pub expect_max_tokens: Option<u32>,
    /// If set, the task only passes if the turn's estimated cost in USD
    /// (`TaskResult::estimated_cost_usd`, from `Config::model_pricing` —
    /// Paquete 3, docs/AUDITORIA-2026-07-v6.md) stayed at or below this
    /// number. Enforced ONLY when the backend row resolved a pricing
    /// entry: a declared budget on an unpriced model reports
    /// `expected_cost_within_budget: None` ("not evaluated") rather than
    /// passing or failing on a guessed price. Same budget-assertion
    /// rationale as `expect_max_rounds`/`expect_max_tokens`.
    #[serde(default)]
    pub expect_max_cost_usd: Option<f64>,
    /// C′.1 (docs/harness-engineering-hooks-skills-2026-07-10.md § I.3):
    /// número de herramientas SINTÉTICAS de ruido que el runner agrega al
    /// registry para esta tarea (un `NoiseToolsProvider` propio, separado
    /// del provider local) — el fixture del A/B de `search_tools`: con
    /// ruido sobre el umbral de deferral, el brazo default esconde el
    /// catálogo detrás del meta-tool; el brazo
    /// `+ablate:tool-search-threshold=1000000` lo lista entero. `0` (el
    /// default) no agrega nada — las suites existentes no cambian ni de
    /// comportamiento ni de fingerprint.
    #[serde(default)]
    pub noise_tools: usize,
    /// Experimental Paper 2 memory condition. When `memory_file` is set,
    /// this labels what is being injected (`"procedural"`, `"summary"`,
    /// `"episodic"`, `"human-playbook"`, ...). It is serialized into
    /// `TaskResult` so sweeps can compare memory conditions without
    /// parsing task ids. If omitted while `memory_file` is present, the
    /// runner treats it as `"procedural"`.
    #[serde(default)]
    pub memory_condition: Option<String>,
    /// Optional memory/playbook file injected into the system prompt for
    /// this task. Relative paths are resolved against the suite TOML's
    /// directory in [`load_suite`], not against the per-run sandbox.
    #[serde(default)]
    pub memory_file: Option<PathBuf>,
    /// Token budget for the rendered memory section. Defaults in the
    /// renderer to a conservative Paper 2 pilot budget when omitted.
    #[serde(default)]
    pub memory_budget_tokens: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TaskSuiteFile {
    tasks: Vec<TaskDef>,
}

/// Loads and parses a task suite from a TOML file.
pub fn load_suite(path: &Path) -> Result<Vec<TaskDef>, BenchError> {
    let contents = std::fs::read_to_string(path)?;
    let mut suite: TaskSuiteFile = toml::from_str(&contents)?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    for task in &mut suite.tasks {
        if let Some(memory_file) = &mut task.memory_file
            && memory_file.is_relative()
        {
            *memory_file = base_dir.join(&memory_file);
        }
    }
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
            expect_max_rounds = 8
            expect_max_tokens = 4000
            expect_max_cost_usd = 0.05
            expect_cargo_check = true
            memory_condition = "procedural"
            memory_file = "playbooks/rust-fix.json"
            memory_budget_tokens = 500

            [tasks.setup_files]
            "notas.txt" = "uno\ndos\ntres\n"

            [tasks.expect_file_contains]
            "notas.txt" = ["tres"]
        "#;
        let suite: TaskSuiteFile = toml::from_str(toml_src).unwrap();
        assert_eq!(suite.tasks.len(), 1);
        let task = &suite.tasks[0];
        assert_eq!(task.id, "read_file_basic");
        assert_eq!(task.expect_tool_call.as_deref(), Some("read_file"));
        assert_eq!(task.expect_text_contains.as_deref(), Some("3"));
        assert!(!task.expect_no_tool_call);
        assert_eq!(task.skill.as_deref(), Some("single_tool"));
        assert_eq!(task.expect_max_rounds, Some(8));
        assert_eq!(task.expect_max_tokens, Some(4000));
        assert_eq!(task.expect_max_cost_usd, Some(0.05));
        assert!(task.expect_cargo_check);
        assert_eq!(task.memory_condition.as_deref(), Some("procedural"));
        assert_eq!(
            task.memory_file.as_deref(),
            Some(std::path::Path::new("playbooks/rust-fix.json"))
        );
        assert_eq!(task.memory_budget_tokens, Some(500));
        assert_eq!(
            task.expect_file_contains
                .get("notas.txt")
                .map(Vec::as_slice),
            Some(&["tres".to_string()][..])
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
        // v4 P0.4 budget fields default to `None` when absent — same
        // "no budget declared" semantics the metrics tests pin.
        assert_eq!(task.expect_max_rounds, None);
        assert_eq!(task.expect_max_tokens, None);
        assert_eq!(task.expect_max_cost_usd, None);
        assert!(!task.expect_cargo_check);
        assert_eq!(task.memory_condition, None);
        assert_eq!(task.memory_file, None);
        assert_eq!(task.memory_budget_tokens, None);
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
    fn load_suite_resolves_memory_file_relative_to_suite_directory() {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-test-load-suite-memory-{}",
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
                memory_file = "playbooks/p.json"
            "#,
        )
        .unwrap();

        let tasks = load_suite(&path).unwrap();
        assert_eq!(
            tasks[0].memory_file.as_ref(),
            Some(&dir.join("playbooks/p.json"))
        );

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

    /// C′.1: the shipped `tool-search.toml` must parse, every task must
    /// carry enough noise to cross the default deferral threshold (40),
    /// and — the invariant the whole A/B rides on — no task may expect a
    /// NOISE tool: the correct answer always lives in the local tools,
    /// with the noise there purely to distract.
    #[test]
    fn tool_search_suite_parses_and_noise_crosses_the_default_threshold() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/tool-search.toml");
        let tasks = load_suite(&path).expect("tool-search.toml must parse");

        assert!(tasks.len() >= 5, "got {}", tasks.len());
        for task in &tasks {
            assert!(
                task.noise_tools > 40,
                "task {} must cross the default deferral threshold",
                task.id
            );
            if let Some(expected) = &task.expect_tool_call {
                assert!(
                    !expected.starts_with("noise_"),
                    "task {} expects a noise tool — the A/B's premise is that noise is never the answer",
                    task.id
                );
            }
        }
    }

    /// The Gemma diagnostic suite is a small-model fixture, not an ad-hoc
    /// local scratch file: it must keep parsing after unrelated task-suite
    /// changes and must preserve the probes that make its sweep
    /// interpretable (budgets, tool-search noise, no-tool controls, and
    /// real filesystem assertions for writes).
    #[test]
    fn gemma_diagnostic_suite_parses_and_preserves_probe_contracts() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/gemma-diagnostic.toml");
        let tasks = load_suite(&path).expect("gemma-diagnostic.toml must parse");

        assert_eq!(
            tasks.len(),
            12,
            "gemma-diagnostic.toml should stay a compact 12-task diagnostic suite"
        );

        let ids = tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "gemma_no_tool_arithmetic",
                "gemma_simple_read_lines",
                "gemma_distractor_exact_file",
                "gemma_multi_step_sum_write",
                "gemma_error_recovery_near_filename",
                "gemma_schema_discipline_read",
                "gemma_empty_response_after_tool",
                "gemma_plan_prose_stress",
                "gemma_task_list_candidate",
                "gemma_tool_search_noise_read",
                "gemma_spanish_instruction_following",
                "gemma_permission_boundary",
            ]
        );

        for task in &tasks {
            assert!(
                task.expect_max_rounds.is_some(),
                "task '{}' must declare expect_max_rounds",
                task.id
            );
            assert!(
                task.expect_max_tokens.is_some(),
                "task '{}' must declare expect_max_tokens",
                task.id
            );
            assert!(
                task.skill.is_some(),
                "task '{}' must declare a skill label",
                task.id
            );
        }

        assert!(
            tasks.iter().any(|t| t.expect_no_tool_call),
            "suite needs at least one no-tool control"
        );
        assert!(
            tasks.iter().any(|t| !t.expect_file_contains.is_empty()),
            "suite needs real filesystem assertions for write tasks"
        );

        let tool_search = tasks
            .iter()
            .find(|t| t.id == "gemma_tool_search_noise_read")
            .expect("suite must include the tool-search noise probe");
        assert!(
            tool_search.noise_tools > 40,
            "tool-search probe must cross the default deferral threshold"
        );
        assert_eq!(
            tool_search.expect_tool_call.as_deref(),
            Some("read_file"),
            "tool-search probe should still expect the real local read tool"
        );
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

    /// v4 P0.4 (docs/AUDITORIA-2026-07-v4.md § P0.4,
    /// docs/AUDITORIA-2026-07-v5.md § "Paquete 1 — Medición y harness"):
    /// the shipped `self_improvement.toml` suite — which turns the SI-1
    /// and SI-2 self-improvement exercises into permanent bench tasks —
    /// must actually parse, and each of its tasks must declare at least
    /// one budget assertion (`expect_max_rounds`/`expect_max_tokens`).
    /// That budget declaration is the whole point of the suite: the
    /// bench must be able to say a config is "better" not just "passes",
    /// and a coding task with no budget declared can't.
    #[test]
    fn self_improvement_suite_parses_and_declares_budgets() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/self_improvement.toml");
        let tasks = load_suite(&path)
            .expect("self_improvement.toml must parse; check for TOML syntax issues");

        // The suite carries at least the two resolved SI exercises (SI-1
        // warm-up, SI-2 multi-step coding). Future exercises would push
        // this number up, never below.
        assert!(
            tasks.len() >= 2,
            "expected at least 2 tasks in self_improvement.toml, got {}",
            tasks.len()
        );

        // Every task in this suite must be on the `self_improvement`
        // skill — distinguishing it from the synthetic skills of
        // `default.toml` so a report can break results down separately.
        for task in &tasks {
            assert_eq!(
                task.skill.as_deref(),
                Some("self_improvement"),
                "self_improvement.toml task '{}' is missing the 'self_improvement' skill \
                 label (got {:?})",
                task.id,
                task.skill
            );
            // The v4 P0.4 contract for this suite: every task must
            // declare a rounds/token budget so the report can compare
            // configs on efficiency, not just correctness. `cost_usd` is
            // optional (parsed but not enforced — see that field's doc
            // comment), so it's NOT asserted here.
            assert!(
                task.expect_max_rounds.is_some(),
                "self_improvement.toml task '{}' must declare expect_max_rounds",
                task.id
            );
            assert!(
                task.expect_max_tokens.is_some(),
                "self_improvement.toml task '{}' must declare expect_max_tokens",
                task.id
            );
            // Coding tasks need filesystem verification — there's no
            // other honest way to score "did the model actually add
            // `+lead:`" than checking the resulting file content.
            assert!(
                !task.expect_file_contains.is_empty(),
                "self_improvement.toml task '{}' must verify real file content \
                 via expect_file_contains",
                task.id
            );
        }

        // SI-2 specifically must exist and verify the `+lead:` addition
        // (the whole point of the exercise this suite makes permanent).
        // The guard "+plan:" assertion also lives here to catch a model
        // that achieves `+lead:` by deleting the existing `+plan:`
        // support — same regression check SI-2's acceptance criteria
        // demand (docs/self-improvement-exercises.md § SI-2).
        let si_2 = tasks
            .iter()
            .find(|t| t.id == "si_2_lead_suffix")
            .expect("self_improvement.toml must carry the si_2_lead_suffix task");
        let backend_spec_asserts = si_2
            .expect_file_contains
            .get("backend_spec.rs")
            .expect("si_2_lead_suffix must assert against backend_spec.rs");
        assert!(
            backend_spec_asserts.iter().any(|s| s.contains("+lead:")),
            "si_2_lead_suffix must verify '+lead:' landed on backend_spec.rs \
             (asserts: {backend_spec_asserts:?})"
        );
        assert!(
            backend_spec_asserts
                .iter()
                .any(|s| s.contains("build_lead")),
            "si_2_lead_suffix must verify build_lead was added to backend_spec.rs \
             (asserts: {backend_spec_asserts:?})"
        );
        assert!(
            backend_spec_asserts.iter().any(|s| s.contains("+plan:")),
            "si_2_lead_suffix must guard the existing '+plan:' support wasn't removed \
             (asserts: {backend_spec_asserts:?})"
        );
    }

    /// v8 K-10 (docs/AUDITORIA-2026-07-v8.md): la red de regresión que
    /// las revisiones 1-3 del TOML no tenían. Cada needle de la suite
    /// memory-distillation se fija contra un fixture del fix CANÓNICO,
    /// graduado con la misma función real (`contains_as_a_bounded_token`,
    /// no una reimplementación standalone como la que validó la revisión
    /// 3). Además, cada tarea debe tener al menos un needle que NO
    /// matchee el setup buggy — un needle ya presente en el bug (el caso
    /// `let mut owned_items = self.items;` que encontró la auditoría)
    /// tiene cero poder discriminante por sí solo.
    #[test]
    fn memory_distillation_needles_match_canonical_fixes_and_discriminate() {
        const FIX_BORROW: &str = "pub struct Store {\n    items: Vec<String>,\n}\n\nimpl Store {\n    pub fn new(items: Vec<String>) -> Self {\n        Self { items }\n    }\n\n    pub fn push_and_first(&mut self, value: String) -> Option<String> {\n        let first = self.items.first().cloned();\n        self.items.push(value);\n        first\n    }\n}\n";
        const FIX_LOOP: &str = "pub struct WordBank {\n    words: Vec<String>,\n    suffix: String,\n}\n\nimpl WordBank {\n    pub fn new(words: Vec<String>, suffix: String) -> Self {\n        Self { words, suffix }\n    }\n\n    pub fn expand_short_words(&mut self, max_len: usize) {\n        let mut new_words = Vec::new();\n        for word in &self.words {\n            if word.len() <= max_len {\n                let mut expanded = word.clone();\n                expanded.push_str(&self.suffix);\n                new_words.push(expanded);\n            }\n        }\n        self.words.extend(new_words);\n    }\n}\n";
        const FIX_MOVE: &str = "pub struct Batch {\n    items: Vec<String>,\n}\n\nimpl Batch {\n    pub fn new(items: Vec<String>) -> Self {\n        Self { items }\n    }\n\n    pub fn total_chars(&self) -> usize {\n        self.items.iter().map(|s| s.len()).sum()\n    }\n\n    pub fn consume_and_count(self) -> (usize, usize) {\n        let total = self.total_chars();\n        let mut owned_items = self.items;\n        owned_items.sort();\n        (total, owned_items.len())\n    }\n}\n";
        const FIX_HOLDOUT: &str = "pub fn triple(n: u32) -> u32 {\n    n * 3\n}\n\npub fn double(n: u32) -> u32 {\n    n * 2\n}\n";

        let canonical_fix_by_skill: HashMap<&str, &str> = HashMap::from([
            ("memory_transfer_b", FIX_BORROW),
            ("memory_transfer_b_loop", FIX_LOOP),
            ("memory_transfer_b_move", FIX_MOVE),
            ("memory_holdout_h", FIX_HOLDOUT),
        ]);

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("suites/memory-distillation.toml");
        let tasks = load_suite(&path).expect("memory-distillation.toml must parse");

        for task in &tasks {
            let skill = task.skill.as_deref().unwrap_or_default();
            let fix = canonical_fix_by_skill.get(skill).unwrap_or_else(|| {
                panic!("task '{}' has skill '{skill}' without a canonical fixture — add one here", task.id)
            });
            let needles = task
                .expect_file_contains
                .get("src/lib.rs")
                .unwrap_or_else(|| panic!("task '{}' must assert on src/lib.rs", task.id));
            let buggy = task
                .setup_files
                .get("src/lib.rs")
                .unwrap_or_else(|| panic!("task '{}' must set up src/lib.rs", task.id));

            for needle in needles {
                assert!(
                    crate::metrics::contains_as_a_bounded_token(fix, needle),
                    "task '{}': needle {needle:?} does not match its canonical fix as a \
                     bounded token — the exact failure class of revision 3",
                    task.id
                );
            }
            assert!(
                needles
                    .iter()
                    .any(|n| !crate::metrics::contains_as_a_bounded_token(buggy, n)),
                "task '{}': every needle already matches the BUGGY setup — the assertion \
                 set has zero discriminant power (v8 K-10)",
                task.id
            );
            // v8 K-9: el grading semántico es parte del contrato de la
            // suite — los needles miden la forma del fix, cargo check
            // mide el resultado que el prompt literalmente pide.
            assert!(
                task.expect_cargo_check,
                "task '{}' must declare expect_cargo_check = true",
                task.id
            );
        }
    }

    #[test]
    fn memory_distillation_suite_parses_and_resolves_playbook_paths() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("suites/memory-distillation.toml");
        let tasks = load_suite(&path)
            .expect("memory-distillation.toml must parse; check for TOML syntax issues");

        assert_eq!(tasks.len(), 7);
        assert!(
            tasks.iter().any(|task| task.memory_file.is_none()),
            "pilot suite needs a no-memory baseline task"
        );

        let memory_tasks: Vec<_> = tasks
            .iter()
            .filter(|task| task.memory_file.is_some())
            .collect();
        assert_eq!(memory_tasks.len(), 4);
        for task in memory_tasks {
            assert_eq!(task.memory_condition.as_deref(), Some("human-playbook"));
            assert_eq!(task.memory_budget_tokens, Some(500));
            let memory_file = task.memory_file.as_ref().expect("checked above");
            assert!(
                memory_file.exists(),
                "memory file for task '{}' must exist after suite-relative resolution: {}",
                task.id,
                memory_file.display()
            );
        }
    }
}
