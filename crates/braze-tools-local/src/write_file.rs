//! `write_file` tool: creates or overwrites a file with the given content.
//! Guarded — `LocalToolsProvider::invoke` checks
//! `ActionDescriptor::WriteFile` before calling [`write_file`].

use std::path::PathBuf;

use serde::Deserialize;

/// Arguments as they arrive in `ToolCall.arguments`:
/// `{"path": "notes.txt", "content": "...", "allow_shrink": true}`.
#[derive(Debug, Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
    /// P0.3 (docs/AUDITORIA-2026-07-v4.md): explicit opt-in required to
    /// overwrite an existing file with content much smaller than what's
    /// there — absent/false, such a write is refused *before* touching
    /// disk. `#[serde(default)]` so the common create/append-size case
    /// needs no extra field.
    #[serde(default)]
    pub allow_shrink: bool,
}

/// An overwrite counts as a destructive shrink when the previous file was
/// at least this many bytes larger than the new content — small enough to
/// catch "wrote back a paginated read_file page instead of the whole
/// file" (docs/AUDITORIA-2026-07-v3.md, hallazgo A2), large enough to
/// stay quiet on ordinary edits that legitimately shrink a file a bit.
///
/// P0.3 (docs/AUDITORIA-2026-07-v4.md) upgraded this from a post-write
/// warning to a preflight refusal: for small models the most dangerous
/// failure mode isn't a wrong summary, it's a total overwrite with an
/// incomplete version — a warning after the write is a diagnosis of
/// damage already done (observed live: destructive rewrite of
/// `backend_spec.rs` during SI-2, docs/usability-log-2026-07-07-si2.md).
const SHRINK_PREFLIGHT_THRESHOLD_BYTES: usize = 500;

/// `Ok(summary)` on success. `Err(message)` is a recoverable tool-level
/// failure (parent directory doesn't exist, or a destructive shrink
/// without `allow_shrink: true`) — see `provider.rs::wrap`.
pub async fn write_file(args: WriteFileArgs) -> Result<String, String> {
    let path = PathBuf::from(&args.path);
    let len = args.content.len();

    // Read the previous size *before* writing — this is the only point
    // where "what was there before" is still observable. A model that
    // only saw a truncated/paginated `read_file` page and obeyed the
    // steering to "write the complete file" can silently discard
    // everything past what it saw.
    let previous_size = tokio::fs::metadata(&path)
        .await
        .ok()
        .map(|m| m.len() as usize);

    if let Some(previous_size) = previous_size
        && previous_size > len
        && previous_size - len >= SHRINK_PREFLIGHT_THRESHOLD_BYTES
        && !args.allow_shrink
    {
        return Err(format!(
            // La redacción importa: la versión anterior decía "retry this
            // exact write_file call with allow_shrink: true", y contra roam
            // (2026-07-26) gpt-oss:20b hizo exactamente eso — reintentó la
            // llamada EXACTA, sin agregar el campo, y quedó atrapado en el
            // guard de repetición hasta abandonar el turno. La instrucción
            // ahora es imperativa, nombra el campo como algo que hay que
            // AGREGAR, y avisa que repetir igual vuelve a fallar.
            "refused: '{}' is {previous_size} bytes but this write would replace it with only \
             {len} bytes. Nothing was written. If you only saw a truncated or paginated \
             read_file page rather than the complete file, you would have discarded the rest. \
             Either use edit_file for a targeted change, or — if you really do mean to replace \
             the whole file with something this much smaller — ADD the field \
             \"allow_shrink\": true to this call's arguments and send it again. Sending the \
             same arguments unchanged will be refused again.",
            path.display()
        ));
    }

    // Incidente roam (2026-07-19): al andamiar un workspace nuevo, el
    // modelo escribe `roam-core/Cargo.toml` antes de que `roam-core/`
    // exista — el fallo costaba una ronda extra de mkdir (y en aquel
    // turno, la ronda que murió). Crear los padres es seguro por
    // construcción: el PermissionGuard ya aprobó ESTA ruta (los padres
    // son prefijos de una ruta permitida dentro del allowlist), y es la
    // semántica de la tool Write de los harnesses de referencia.
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            format!(
                "failed to create parent directory '{}': {err}",
                parent.display()
            )
        })?;
    }
    tokio::fs::write(&path, args.content.as_bytes())
        .await
        .map_err(|err| format!("failed to write '{}': {err}", path.display()))?;

    Ok(format!("wrote {len} bytes to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_dir;

    /// Regression test for the roam incident (2026-07-19): writing to
    /// a path whose parent directory doesn't exist yet must create the
    /// parents (workspace scaffolding writes `roam-core/Cargo.toml`
    /// before any `mkdir`), not fail the call.
    #[tokio::test]
    async fn creates_missing_parent_directories() {
        let dir = unique_temp_dir("write-file-parents");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("nuevo-crate").join("src").join("lib.rs");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "pub fn hola() {}\n".to_string(),
            allow_shrink: false,
        })
        .await;

        assert!(result.is_ok(), "got: {result:?}");
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back written file");
        assert_eq!(contents, "pub fn hola() {}\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn creates_file_with_given_content() {
        let dir = unique_temp_dir("write-file-happy");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("out.txt");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "payload".to_string(),
            allow_shrink: false,
        })
        .await;

        assert!(result.is_ok());
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back written file");
        assert_eq!(contents, "payload");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn write_failure_is_a_recoverable_error() {
        // The target path IS a directory -> the write fails. (A missing
        // parent no longer fails since the roam fix: parents are
        // created like `mkdir -p`.)
        let dir = unique_temp_dir("write-file-target-is-dir");
        let target = dir.join("soy-un-directorio");
        tokio::fs::create_dir_all(&target)
            .await
            .expect("create target dir");

        let result = write_file(WriteFileArgs {
            path: target.to_string_lossy().into_owned(),
            content: "payload".to_string(),
            allow_shrink: false,
        })
        .await;

        assert!(result.is_err());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // --- shrink preflight (P0.3, docs/AUDITORIA-2026-07-v4.md; formerly
    // a post-write warning, hallazgo A2 de v3) ---

    #[tokio::test]
    async fn overwriting_a_much_larger_file_is_refused_before_writing() {
        let dir = unique_temp_dir("write-file-shrink-refused");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("out.txt");
        let original = "x".repeat(SHRINK_PREFLIGHT_THRESHOLD_BYTES * 4);
        tokio::fs::write(&file_path, &original)
            .await
            .expect("write fixture");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "short".to_string(),
            allow_shrink: false,
        })
        .await;

        let err = result.expect_err("a destructive shrink must be refused");
        assert!(err.contains("allow_shrink"), "got: {err}");
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(
            contents, original,
            "the refusal must happen BEFORE touching disk — P0.3's acceptance criterion"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn allow_shrink_lets_an_intentional_shrink_through() {
        let dir = unique_temp_dir("write-file-shrink-allowed");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("out.txt");
        tokio::fs::write(&file_path, "x".repeat(SHRINK_PREFLIGHT_THRESHOLD_BYTES * 4))
            .await
            .expect("write fixture");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "short".to_string(),
            allow_shrink: true,
        })
        .await
        .expect("an explicitly allowed shrink must succeed");

        assert!(result.contains("wrote 5 bytes"), "got: {result}");
        let contents = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read back");
        assert_eq!(contents, "short");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn overwriting_with_a_similar_size_needs_no_allow_shrink() {
        let dir = unique_temp_dir("write-file-no-shrink-preflight");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("out.txt");
        tokio::fs::write(&file_path, "original content")
            .await
            .expect("write fixture");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "updated content".to_string(),
            allow_shrink: false,
        })
        .await;

        assert!(result.is_ok(), "got: {result:?}");
    }

    #[tokio::test]
    async fn creating_a_new_file_needs_no_allow_shrink() {
        let dir = unique_temp_dir("write-file-new-no-preflight");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let file_path = dir.join("brand-new.txt");

        let result = write_file(WriteFileArgs {
            path: file_path.to_string_lossy().into_owned(),
            content: "hello".to_string(),
            allow_shrink: false,
        })
        .await;

        assert!(result.is_ok(), "got: {result:?}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
