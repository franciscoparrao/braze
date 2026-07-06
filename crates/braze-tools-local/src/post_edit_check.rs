//! Post-edit validation guardrail (ítem 5 del backlog 2026-07-06):
//! after `write_file`/`edit_file` lands on a Rust source file inside a
//! Cargo project, run `cargo check` and feed any compile errors back to
//! the model *inside the same tool result*.
//!
//! The evidence is the strongest single number in SWE-agent/ACI (arXiv
//! 2405.15793, Tabla 3): removing the edit guardrail cost -3.0 pp and
//! 51.7% of trajectories contain at least one failed edit — a model
//! that learns about the breakage in the very next observation repairs
//! it; one that doesn't keeps building on a broken tree. Aider ships
//! the same idea as post-edit auto-lint. See docs/SOTA-2026-07.md.
//!
//! Failure posture: the guardrail only ever *adds* feedback to an edit
//! that already succeeded — `cargo` missing, timing out, or the file
//! not being part of a Cargo project all silently skip (trace-level
//! only). It must never turn a good edit into a failed tool call.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Upper bound for one `cargo check` run. A warm check on a mid-size
/// workspace is single-digit seconds; a cold one on a huge tree can be
/// minutes — past this, the guardrail silently skips rather than stall
/// the agent loop (the edit itself already succeeded).
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// Cap on the feedback appended to the tool result — enough for the
/// first several errors (`--message-format=short`, one line each), not
/// an unbounded dump that would blow up the tactical window the moment
/// an edit breaks a widely-used symbol.
const MAX_FEEDBACK_CHARS: usize = 2_000;

/// Runs the guardrail for `path` (already resolved to an absolute path
/// by the provider) and returns the feedback block to append to the
/// tool result, or `None` when there is nothing to say — not a Rust
/// file, no enclosing Cargo project, `cargo` unavailable/timed out, or
/// the check passed. Only compile *failure* produces feedback: a clean
/// check appends nothing, so the guardrail is token-free on the happy
/// path.
pub(crate) async fn post_edit_feedback(path: &str) -> Option<String> {
    if Path::new(path).extension().is_none_or(|ext| ext != "rs") {
        return None;
    }
    let project_dir = find_cargo_project(Path::new(path))?;

    let command = tokio::process::Command::new("cargo")
        .args(["check", "--quiet", "--message-format=short"])
        .current_dir(&project_dir)
        .output();
    let output = match tokio::time::timeout(CHECK_TIMEOUT, command).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "post-edit check skipped: cargo not runnable");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                project = %project_dir.display(),
                "post-edit check skipped: cargo check exceeded its timeout"
            );
            return None;
        }
    };

    if output.status.success() {
        return None;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    // `--message-format=short` emits one `path:line:col: error[..]: msg`
    // line per diagnostic; keep error lines (and cargo's own terminal
    // "error: could not compile ..." summary), drop warnings — the
    // guardrail is about breakage, not style.
    let mut feedback = String::new();
    for line in stderr.lines().filter(|line| line.contains("error")) {
        if feedback.len() + line.len() + 1 > MAX_FEEDBACK_CHARS {
            feedback.push_str("… (more errors omitted)\n");
            break;
        }
        feedback.push_str(line);
        feedback.push('\n');
    }
    if feedback.is_empty() {
        // Non-zero exit but nothing matching "error" (e.g. a broken
        // Cargo.toml manifest message) — still worth surfacing a capped
        // excerpt rather than staying silent about a failing check.
        feedback = stderr.chars().take(MAX_FEEDBACK_CHARS).collect();
    }

    Some(format!(
        "\n\n[post-edit check] `cargo check` fails in {} after this edit \
         (the edit itself was applied). Fix these before moving on:\n{}",
        project_dir.display(),
        feedback.trim_end()
    ))
}

/// Nearest ancestor directory of `path` containing a `Cargo.toml` — the
/// crate (or workspace member) the edited file belongs to. Checking the
/// *nearest* manifest keeps the run scoped to that member instead of a
/// whole workspace.
fn find_cargo_project(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1) // the file itself
        .find(|dir| dir.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "braze-post-edit-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("src")).expect("create temp project dirs");
        dir
    }

    fn write_project(dir: &Path, main_body: &str) {
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"guardrail-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write Cargo.toml");
        std::fs::write(dir.join("src/main.rs"), main_body).expect("write main.rs");
    }

    #[tokio::test]
    async fn a_non_rust_file_produces_no_feedback() {
        assert!(post_edit_feedback("/tmp/nota.txt").await.is_none());
    }

    #[tokio::test]
    async fn a_rust_file_outside_any_cargo_project_produces_no_feedback() {
        let dir = temp_dir("no-project");
        let path = dir.join("src/suelto.rs");
        std::fs::write(&path, "fn main() {}").expect("write file");

        assert!(
            post_edit_feedback(&path.to_string_lossy()).await.is_none(),
            "no Cargo.toml ancestor must mean silent skip"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real `cargo check` against a deliberately broken fixture crate —
    /// the guardrail's whole value is the actual compiler diagnostics,
    /// so this test pays the real (small: trivial crate, warm toolchain)
    /// cost instead of mocking the interesting part away.
    #[tokio::test]
    async fn a_compile_error_comes_back_as_feedback() {
        let dir = temp_dir("broken");
        write_project(&dir, "fn main() { let x: u32 = \"no\"; }");

        let feedback = post_edit_feedback(&dir.join("src/main.rs").to_string_lossy())
            .await
            .expect("a broken crate must produce feedback");
        assert!(feedback.contains("[post-edit check]"), "got: {feedback}");
        assert!(feedback.contains("error"), "got: {feedback}");
        assert!(
            feedback.contains("the edit itself was applied"),
            "the feedback must not read as if the edit failed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_clean_crate_produces_no_feedback() {
        let dir = temp_dir("clean");
        write_project(&dir, "fn main() {}");

        assert!(
            post_edit_feedback(&dir.join("src/main.rs").to_string_lossy())
                .await
                .is_none(),
            "a passing check must append nothing (token-free happy path)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_cargo_project_picks_the_nearest_manifest() {
        let dir = temp_dir("nested");
        std::fs::write(dir.join("Cargo.toml"), "[workspace]\n").expect("outer manifest");
        let inner = dir.join("src"); // reuse the created subdir as "member"
        std::fs::write(inner.join("Cargo.toml"), "[package]\n").expect("inner manifest");
        std::fs::create_dir_all(inner.join("src")).expect("inner src");
        let file = inner.join("src/lib.rs");
        std::fs::write(&file, "").expect("write lib.rs");

        assert_eq!(find_cargo_project(&file), Some(inner));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
