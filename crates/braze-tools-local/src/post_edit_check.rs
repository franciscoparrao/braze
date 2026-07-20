//! Post-edit validation guardrail (ítem 5 del backlog 2026-07-06,
//! generalizado en v4 P1.6 para cualquier stack vía
//! `braze-config::FormatterConfig`): after `write_file`/`edit_file` lands
//! on a file whose extension matches a configured formatter, run that
//! formatter's command and feed any reported errors back to the model
//! *inside the same tool result*.
//!
//! The evidence is the strongest single number in SWE-agent/ACI (arXiv
//! 2405.15793, Tabla 3): removing the edit guardrail cost -3.0 pp and
//! 51.7% of trajectories contain at least one failed edit — a model
//! that learns about the breakage in the very next observation repairs
//! it; one that doesn't keeps building on a broken tree. Aider ships
//! the same idea as post-edit auto-lint. See docs/SOTA-2026-07.md.
//!
//! Failure posture: the guardrail only ever *adds* feedback to an edit
//! that already succeeded — command missing, timing out, or no
//! `error:` lines emitted all silently skip (trace-level only). It must
//! never turn a good edit into a failed tool call.

use std::path::Path;
use std::time::Duration;

#[cfg(test)]
use std::path::PathBuf;

use braze_config::FormatterConfig;

/// Cap on the feedback appended to the tool result — enough for the
/// first several errors (one line each), not an unbounded dump that would
/// blow up the tactical window the moment an edit breaks a widely-used
/// symbol.
const MAX_FEEDBACK_CHARS: usize = 2_000;

/// Presupuesto de los warnings anexados a un check EXITOSO (incidente
/// roam #11). Un orden de magnitud por debajo de `MAX_FEEDBACK_CHARS`:
/// el objetivo es que el residuo de una edición incompleta sea visible,
/// no volcarle al modelo el lint completo del crate.
const MAX_WARNING_LINES: usize = 3;
const MAX_WARNING_CHARS: usize = 400;

/// Runs the guardrail for `path` (already resolved to an absolute path
/// by the provider), using the configured formatter list (v4 P1.6),
/// and returns the feedback block to append to the tool result, or
/// `None` when there is nothing to say — no formatter matches the file's
/// extension, the matching entry is `disabled`, or the formatter command
/// isn't runnable or timed out.
///
/// Un check EXITOSO ya no devuelve `None` (incidente roam #11): confirma
/// que compila, acota que eso NO es haber corrido tests, y anexa hasta
/// [`MAX_WARNING_LINES`] warnings. `None` queda reservado para "el
/// guardrail no pudo decir nada", que es información distinta de "pasó".
pub(crate) async fn post_edit_feedback(path: &str, formatters: &[FormatterConfig]) -> Option<String> {
    let ext = Path::new(path).extension()?.to_string_lossy().to_lowercase();
    let fmt = formatters.iter().find(|f| {
        !f.disabled
            && f.extensions
                .iter()
                .any(|e| e.trim_start_matches('.').to_lowercase() == ext)
    })?;
    let cwd = Path::new(path).parent()?;
    check_with_formatter(fmt, cwd).await
}

/// The legacy default entry — returned as a fresh `Vec` so it can also
/// serve as the seed for `LocalToolsProvider`'s field default (a `const`
/// with `String`s inside can't be `const`-evaluated under current Rust,
/// but this is just one allocation, paid once per `LocalToolsProvider`
/// construction — negligible vs the rest of engine startup). Delegates
/// to [`braze_config::default_formatters`] rather than hardcoding a
/// second copy of the `cargo check` command — the two definitions had
/// drifted into two separate literals (found duplicated auditing the
/// other-model commit `2923f63`, 2026-07-09), which is exactly the kind
/// of thing that silently goes stale when one gets updated and the
/// other doesn't.
pub(crate) fn default_rust_formatters() -> Vec<FormatterConfig> {
    braze_config::default_formatters()
}

/// Runs `fmt.command` with `cwd = cwd` and turns a non-zero exit + any
/// `error:` lines into a feedback block. Returns `None` when there's
/// nothing to say — binary missing, timeout, clean exit, or non-zero
/// exit with no error-like stderr line.
async fn check_with_formatter(fmt: &FormatterConfig, cwd: &Path) -> Option<String> {
    let program = fmt.command.first()?;
    let args = &fmt.command[1..];
    let output =
        tokio::time::timeout(Duration::from_secs(fmt.timeout_secs), async {
            tokio::process::Command::new(program)
                .args(args)
                .current_dir(cwd)
                .output()
                .await
        })
        .await;

    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            tracing::debug!(
                error = %err,
                command = ?fmt.command,
                "post-edit check skipped: formatter not runnable"
            );
            return None;
        }
        Err(_) => {
            tracing::warn!(
                cwd = %cwd.display(),
                timeout_secs = fmt.timeout_secs,
                "post-edit check skipped: formatter exceeded its timeout"
            );
            return None;
        }
    };

    if output.status.success() {
        // Incidente roam #11 (2026-07-20, tarea 3 del testbed): en éxito
        // el guardrail devolvía `None` — silencio absoluto. El modelo no
        // puede distinguir "el check pasó" de "el check no corrió" ni de
        // "no había formatter", y lee la ausencia de errores como
        // "verificado": tras una edición limpia declaró la tarea
        // terminada sin correr `cargo test`, que el prompt pedía
        // explícitamente. El silencio no era token-free, era ambiguo.
        // Ahora se confirma el éxito Y se acota qué significa.
        //
        // Los warnings se anexan por la misma razón: viven en el stderr
        // de un exit 0, que es justo donde muere el residuo de una
        // eliminación (imports muertos, docstrings huérfanos — ambos
        // observados en la tarea 2). Su presupuesto es deliberadamente
        // mucho menor que `MAX_FEEDBACK_CHARS`: el modo de falla del
        // incidente #7 fue ahogar el canal con texto, y un warning no
        // justifica el mismo espacio que un error.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut warnings = String::new();
        // `contains`, no `starts_with`: el formatter por defecto usa
        // `--message-format=short`, que antepone la ubicación
        // (`src/main.rs:1:5: warning: …`). Misma heurística laxa que el
        // camino de errores de abajo, y por la misma razón.
        for (shown, line) in stderr
            .lines()
            .filter(|line| line.contains("warning:"))
            .enumerate()
        {
            if shown == MAX_WARNING_LINES || warnings.len() + line.len() + 1 > MAX_WARNING_CHARS {
                warnings.push_str("… (more warnings omitted)\n");
                break;
            }
            warnings.push_str(line);
            warnings.push('\n');
        }

        let mut note = format!(
            "\n\n[post-edit check] `{}` passed in {} — the code COMPILES. \
             That is all this confirms: no tests were run. If this task has a \
             verification step, it has not happened yet.",
            program,
            cwd.display()
        );
        if !warnings.is_empty() {
            note.push_str("\nWarnings (not errors, but often the leftovers of an \
                           incomplete edit — unused imports, dead code):\n");
            note.push_str(warnings.trim_end());
        }
        return Some(note);
    }

    // Combine stdout+stderr — many tools (ruff, tsc) put diagnostics on
    // stdout; others (cargo) on stderr. Cap at `MAX_FEEDBACK_CHARS`.
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    // Error-like lines (prefix `error:` or "error[" or containing the
    // word "error" — same heuristic as before the generalization); drop
    // pure warnings/lint-only output that the guardrail isn't for.
    let mut feedback = String::new();
    for line in combined.lines().filter(|line| line.contains("error")) {
        if feedback.len() + line.len() + 1 > MAX_FEEDBACK_CHARS {
            feedback.push_str("… (more errors omitted)\n");
            break;
        }
        feedback.push_str(line);
        feedback.push('\n');
    }
    if feedback.is_empty() {
        // Non-zero exit but no error-prefixed line (e.g. a broken
        // manifest message or a formatter that prints only a different
        // header). Surface a bounded excerpt rather than silence.
        feedback = combined.chars().take(MAX_FEEDBACK_CHARS).collect();
    }

    Some(format!(
        "\n\n[post-edit check] `{}` (exit {}) in {} after this edit \
         (the edit itself was applied). Fix these before moving on:\n{}",
        program,
        output.status.code().unwrap_or(-1),
        cwd.display(),
        feedback.trim_end()
    ))
}

/// Nearest ancestor directory of `path` containing a `Cargo.toml` — kept
/// for tests that still assert the project-resolution logic, even though
/// the runtime path now uses the file's parent directory directly (so
/// cargo walks ancestors itself).
#[cfg(test)]
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
        let dir =
            std::env::temp_dir().join(format!("braze-post-edit-{label}-{}", uuid::Uuid::new_v4()));
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
    async fn a_non_rust_file_produces_no_feedback_with_default_formatters() {
        // `.txt` matches nothing in the default Rust entry.
        assert!(
            post_edit_feedback("/tmp/nota.txt", default_rust_formatters().as_slice())
                .await
                .is_none()
        );
    }

    /// A Rust file with no `Cargo.toml` ancestor still gets checked —
    /// `cargo check` run from the file's parent directory fails with
    /// "error: could not find `Cargo.toml`...", and that error line IS
    /// surfaced as feedback (the pre-generalization code silently
    /// returned `None` for this case instead). Regression test for a
    /// dead assertion found auditing the other-model commit `2923f63`
    /// (2026-07-09): this test used to write the fixture and then assert
    /// nothing at all, despite its name and a comment explaining what
    /// *should* happen — passing unconditionally regardless of whether
    /// the described behavior actually held.
    #[tokio::test]
    async fn a_rust_file_outside_any_cargo_project_still_surfaces_cargos_error() {
        let dir = temp_dir("no-project");
        let path = dir.join("src/suelto.rs");
        std::fs::write(&path, "fn main() {}").expect("write file");

        let feedback =
            post_edit_feedback(&path.to_string_lossy(), default_rust_formatters().as_slice()).await;

        assert!(
            feedback.is_some(),
            "cargo check with no Cargo.toml ancestor exits non-zero with an 'error:' line and \
             must be surfaced to the model, not silently dropped"
        );
        assert!(
            feedback.unwrap().contains("Cargo.toml"),
            "feedback should mention the missing manifest"
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

        let feedback =
            post_edit_feedback(&dir.join("src/main.rs").to_string_lossy(), default_rust_formatters().as_slice())
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

    /// Incidente roam #11 (2026-07-20): un check exitoso confirma que
    /// COMPILA y dice explícitamente que eso no es haber corrido tests.
    /// El contrato anterior era devolver `None` ("token-free happy
    /// path"), y el silencio resultó indistinguible de "el guardrail no
    /// corrió" — el modelo lo leyó como verificación completa y cerró la
    /// tarea sin ejecutar `cargo test`.
    #[tokio::test]
    async fn a_clean_crate_confirms_compilation_and_scopes_it() {
        let dir = temp_dir("clean");
        write_project(&dir, "fn main() {}");

        let feedback = post_edit_feedback(
            &dir.join("src/main.rs").to_string_lossy(),
            default_rust_formatters().as_slice(),
        )
        .await
        .expect("a passing check must confirm it passed, not stay silent");
        assert!(feedback.contains("[post-edit check]"), "got: {feedback}");
        assert!(
            feedback.contains("COMPILES"),
            "the confirmation must say what passed: {feedback}"
        );
        assert!(
            feedback.contains("no tests were run"),
            "the confirmation must scope itself so it is not read as full \
             verification: {feedback}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Los warnings de un exit 0 son el único lugar donde aparece el
    /// residuo de una edición incompleta (imports muertos tras eliminar
    /// una función — observado en la tarea 2 del testbed), y el
    /// guardrail los descartaba junto con el resto del stderr exitoso.
    #[tokio::test]
    async fn a_clean_crate_surfaces_its_warnings() {
        let dir = temp_dir("warn");
        // Compila, pero deja un import muerto: exactamente la forma del
        // residuo observado en producción.
        write_project(&dir, "use std::collections::HashMap;\nfn main() {}");

        let feedback = post_edit_feedback(
            &dir.join("src/main.rs").to_string_lossy(),
            default_rust_formatters().as_slice(),
        )
        .await
        .expect("a passing check with warnings must still speak");
        assert!(
            feedback.contains("Warnings (not errors"),
            "warnings must be labelled as such, never as failures: {feedback}"
        );
        assert!(
            feedback.contains("unused import"),
            "the actual warning must reach the model: {feedback}"
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

    /// Generalization (v4 P1.6): a formatter entry with a custom command
    /// for `.txt` files — the guardrail runs it the same way it runs
    /// `cargo check` for `.rs`.
    #[tokio::test]
    async fn a_custom_formatter_for_a_non_rust_extension_runs_its_command() {
        let dir = std::env::temp_dir().join(format!(
            "braze-post-edit-custom-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("broken.txt"), "this will error").expect("write file");

        // `false` exits non-zero and prints "error: deliberately broken"
        // to stderr — exactly what a real linter like ruff would do.
        let exit_non_zero_with_err = FormatterConfig {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'error: deliberately broken'; exit 1".to_string(),
            ],
            extensions: vec![".txt".to_string()],
            timeout_secs: 10,
            disabled: false,
        };

        let feedback =
            post_edit_feedback(&dir.join("broken.txt").to_string_lossy(), std::slice::from_ref(
                &exit_non_zero_with_err,
            ))
            .await
            .expect("a non-zero exit + error line must produce feedback");
        assert!(feedback.contains("[post-edit check]"));
        assert!(feedback.contains("deliberately broken"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A formatter entry with `disabled: true` is skipped — the
    /// generalization's granular opt-out (per-extension).
    #[tokio::test]
    async fn a_disabled_formatter_entry_is_skipped() {
        let dir = std::env::temp_dir().join(format!(
            "braze-post-edit-disabled-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("x.rs"), "fn main() {}").expect("write file");

        let disabled = FormatterConfig {
            command: vec![
                "cargo".to_string(),
                "check".to_string(),
                "--quiet".to_string(),
            ],
            extensions: vec![".rs".to_string()],
            timeout_secs: 60,
            disabled: true,
        };

        assert!(
            post_edit_feedback(&dir.join("x.rs").to_string_lossy(), std::slice::from_ref(
                &disabled
            ))
            .await
            .is_none(),
            "a disabled formatter entry must produce no feedback"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
