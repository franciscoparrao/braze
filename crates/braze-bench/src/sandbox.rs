//! Isolated per-task-run working directory: never the real repo — see
//! PLAN.md's "Hallazgo de diseño no anticipado" for why the harness never
//! points a `WorkdirAllowlist` at a real directory a model could damage.
//! Also gives every backend byte-identical starting conditions for the
//! same task, which is what makes the comparison fair.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::task::TaskDef;

/// A temp directory created for exactly one (task, backend) run, seeded
/// with `TaskDef::setup_files`. Removed on drop, best-effort.
pub struct TaskSandbox {
    dir: PathBuf,
}

impl TaskSandbox {
    /// Creates a fresh, uniquely-named temp directory and writes `task`'s
    /// `setup_files` into it before returning.
    pub fn new(task: &TaskDef) -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-sandbox-{}-{}-{n}",
            std::process::id(),
            task.id
        ));
        std::fs::create_dir_all(&dir)?;

        for (relative_path, contents) in &task.setup_files {
            // N-38 (docs/AUDITORIA-2026-07-v2.md): `Path::join` does no
            // sanitization — a `setup_files` key like `"../../x"` walks
            // out of `dir`, and an absolute-looking key (`"/etc/passwd"`
            // on Unix) replaces `dir` entirely, letting a malicious/buggy
            // suite TOML write a file outside the sandbox before the
            // task even runs.
            let rel = Path::new(relative_path);
            if rel.is_absolute()
                || rel
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "task {:?}'s setup_files key {relative_path:?} would escape the \
                         sandbox (absolute path or '..' component) — refusing to write \
                         outside {dir:?}",
                        task.id
                    ),
                ));
            }

            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, contents)?;
        }

        Ok(Self { dir })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TaskSandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn task_with_files(files: &[(&str, &str)]) -> TaskDef {
        TaskDef {
            id: "sandbox-test".to_string(),
            prompt: "irrelevant".to_string(),
            setup_files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            expect_tool_call: None,
            accept_tool_calls: Vec::new(),
            expect_no_tool_call: false,
            expect_text_contains: None,
            expect_file_contains: HashMap::new(),
            expect_cargo_check: false,
            sandbox_commands: Vec::new(),
            skill: None,
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

    #[test]
    fn writes_setup_files_into_a_fresh_directory() {
        let task = task_with_files(&[("notas.txt", "uno\ndos\n")]);
        let sandbox = TaskSandbox::new(&task).unwrap();

        let content = std::fs::read_to_string(sandbox.path().join("notas.txt")).unwrap();
        assert_eq!(content, "uno\ndos\n");
    }

    #[test]
    fn two_sandboxes_for_the_same_task_do_not_collide() {
        let task = task_with_files(&[("a.txt", "x")]);
        let first = TaskSandbox::new(&task).unwrap();
        let second = TaskSandbox::new(&task).unwrap();
        assert_ne!(first.path(), second.path());
    }

    #[test]
    fn directory_is_removed_on_drop() {
        let task = task_with_files(&[("a.txt", "x")]);
        let path = {
            let sandbox = TaskSandbox::new(&task).unwrap();
            sandbox.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    /// Regression test for N-38 (docs/AUDITORIA-2026-07-v2.md): a
    /// `setup_files` key with a `..` component must be rejected instead
    /// of writing outside the sandbox.
    #[test]
    fn rejects_a_setup_file_path_that_escapes_the_sandbox_via_parent_dir() {
        let task = task_with_files(&[("../escaped.txt", "x")]);
        let result = TaskSandbox::new(&task);
        assert!(
            result.is_err(),
            "expected a '..' setup_files key to be rejected"
        );
    }

    /// Regression test for N-38: an absolute-looking key must also be
    /// rejected — `Path::join` with an absolute path silently replaces
    /// the base directory entirely instead of nesting under it.
    #[test]
    fn rejects_an_absolute_setup_file_path() {
        let task = task_with_files(&[("/tmp/escaped-braze-bench-test.txt", "x")]);
        let result = TaskSandbox::new(&task);
        assert!(
            result.is_err(),
            "expected an absolute setup_files key to be rejected"
        );
    }
}
