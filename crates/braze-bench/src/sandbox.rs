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
            let path = dir.join(relative_path);
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
            expect_no_tool_call: false,
            expect_text_contains: None,
            expect_file_contains: HashMap::new(),
            skill: None,
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
}
