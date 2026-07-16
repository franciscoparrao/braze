//! Opt-in preservation of a run's sandbox + session transcript, for
//! hand-grading a sample against the automated pass/fail assertion — see
//! `docs/emse-review-2026-07-13-checklist.md` Issue 4 (EMSE review, Persona
//! A: "no independent validation of the automated grader"). Off by default:
//! [`run_task`](crate::runner::run_task) unconditionally deletes both the
//! sandbox and the session directory after computing metrics (`sandbox.rs`'s
//! `Drop`, `runner.rs`'s `remove_dir_all`) — this module gives a sweep a way
//! to opt out of that deletion for a subset of runs without changing default
//! behavior for the other 99% of sweeps that don't need transcripts kept.
//!
//! Enabled via the `BRAZE_BENCH_KEEP_SESSIONS` env var (any value other than
//! empty or `"0"`); the destination is a stable, identifiable directory
//! outside `std::env::temp_dir()` (which the OS periodically sweeps), named
//! by backend + task id + repetition so a sampled transcript is traceable
//! back to the exact `TaskResult` row it produced.

use std::io;
use std::path::{Path, PathBuf};

/// Directory a preserved run's artifacts land in, relative to the current
/// working directory (a sweep is always invoked from the repo root — see
/// `docs/sweep-*.md`'s repro commands).
pub const DEFAULT_PRESERVE_ROOT: &str = "braze-bench-preserved-sessions";

/// Reads `BRAZE_BENCH_KEEP_SESSIONS` once. Any value other than unset or
/// `"0"` enables preservation — matches the boolean-env-var convention
/// already used for `BRAZE_OLLAMA_BASE_URL`-style overrides elsewhere in the
/// workspace (`braze-config::overrides`).
pub fn keep_sessions_enabled() -> bool {
    std::env::var("BRAZE_BENCH_KEEP_SESSIONS").is_ok_and(|v| v != "0")
}

/// Replaces path-hostile characters (`:`, `+`, `/`, whitespace) from a
/// backend display name or task id with `_`, so it's safe as a single path
/// component on every OS this project targets (Linux only today, but no
/// reason to paint into a corner).
fn sanitize_path_component(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Builds the destination directory for one (backend, task, repetition)
/// run's preserved artifacts, under `root`. Does not create it — callers
/// create `sandbox`/`session` subdirectories as needed via
/// [`copy_dir_recursive`].
pub fn preserved_run_dir(
    root: &Path,
    backend_display: &str,
    task_id: &str,
    repetition: u32,
) -> PathBuf {
    root.join(sanitize_path_component(backend_display))
        .join(sanitize_path_component(task_id))
        .join(format!("rep{repetition}"))
}

/// Recursively copies `src`'s contents into `dst` (creating `dst` and any
/// intermediate directories as needed). `std::fs` has no built-in recursive
/// copy; this is deliberately minimal rather than pulling in a dependency
/// for a helper that's ~15 lines and used in exactly one place.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &dst_path)?;
        }
        // Symlinks (shouldn't occur in a freshly-seeded sandbox or a
        // FileSessionStore's JSONL dir) are silently skipped rather than
        // followed or erroring — preservation is best-effort diagnostics,
        // not a backup guarantee.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_backend_specs_with_colons_and_plus_signs() {
        let got = sanitize_path_component("ollama:llama3.2:1b+lead:ollama:gemma4:e4b");
        assert_eq!(got, "ollama_llama3.2_1b_lead_ollama_gemma4_e4b");
    }

    #[test]
    fn preserved_run_dir_nests_by_backend_task_and_repetition() {
        let got = preserved_run_dir(Path::new("root"), "ollama:qwen2.5:3b", "grep_basic", 2);
        assert_eq!(got, PathBuf::from("root/ollama_qwen2.5_3b/grep_basic/rep2"));
    }

    #[test]
    fn copy_dir_recursive_copies_nested_files() {
        let tmp = std::env::temp_dir().join(format!(
            "braze-bench-preserve-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("a.txt"), "top-level").unwrap();
        std::fs::write(src.join("nested/b.txt"), "nested").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("a.txt")).unwrap(),
            "top-level"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("nested/b.txt")).unwrap(),
            "nested"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
