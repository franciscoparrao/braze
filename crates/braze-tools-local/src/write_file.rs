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
    /// Add `content` at the end of the file (creating it if missing)
    /// instead of replacing it. Lets a model build a long artifact — notes
    /// per section of a document it is reading, a review written criterion
    /// by criterion — without re-sending everything it already wrote, and
    /// without the shrink guard (an append never discards anything).
    #[serde(default)]
    pub append: bool,
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
pub async fn write_file(args: WriteFileArgs, gate: bool) -> Result<String, String> {
    let path = PathBuf::from(&args.path);
    let len = args.content.len();

    // Gate sintáctico pre-aplicación (survey 2026-08-10): crear o
    // sobrescribir un `.rs` con sintaxis rota se rechaza SIN escribir,
    // salvo que el archivo previo ya estuviera roto. Solo se lee el
    // original para `.rs` (el gate es no-op fuera de Rust). Ver
    // `crate::syntactic_gate`.
    if gate && crate::syntactic_gate::is_rust(&path) {
        let original = tokio::fs::read_to_string(&path).await.ok();
        // An append is judged by the file it produces, not the fragment.
        let resulting = match (&original, args.append) {
            (Some(original), true) => format!("{original}{}", args.content),
            _ => args.content.clone(),
        };
        crate::syntactic_gate::check_rust_edit(&path, original.as_deref(), &resulting, "content")?;
    }

    if args.append {
        return append_to_file(&path, &args.content).await;
    }

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
    create_parent_dirs(&path).await?;
    tokio::fs::write(&path, args.content.as_bytes())
        .await
        .map_err(|err| format!("failed to write '{}': {err}", path.display()))?;

    Ok(format!("wrote {len} bytes to {}", path.display()))
}

async fn create_parent_dirs(path: &std::path::Path) -> Result<(), String> {
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
    Ok(())
}

/// `write_file` with `append: true`: creates the file (and its parents)
/// if missing, otherwise adds `content` at its end. The summary reports
/// the resulting size so the model can tell its progress was kept.
async fn append_to_file(path: &std::path::Path, content: &str) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;

    create_parent_dirs(path).await?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|err| format!("failed to open '{}' for append: {err}", path.display()))?;
    file.write_all(content.as_bytes())
        .await
        .map_err(|err| format!("failed to append to '{}': {err}", path.display()))?;
    file.flush()
        .await
        .map_err(|err| format!("failed to append to '{}': {err}", path.display()))?;
    let total = tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(format!(
        "appended {} bytes to {} (now {total} bytes)",
        content.len(),
        path.display()
    ))
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

        let result = write_file(
            WriteFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                content: "pub fn hola() {}\n".to_string(),
                allow_shrink: false,
                append: false,
            },
            false,
        )
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

        let result = write_file(
            WriteFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                content: "payload".to_string(),
                allow_shrink: false,
                append: false,
            },
            false,
        )
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

        let result = write_file(
            WriteFileArgs {
                path: target.to_string_lossy().into_owned(),
                content: "payload".to_string(),
                allow_shrink: false,
                append: false,
            },
            false,
        )
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

        let result = write_file(
            WriteFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                content: "short".to_string(),
                allow_shrink: false,
                append: false,
            },
            false,
        )
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

        let result = write_file(
            WriteFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                content: "short".to_string(),
                allow_shrink: true,
                append: false,
            },
            false,
        )
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

        let result = write_file(
            WriteFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                content: "updated content".to_string(),
                allow_shrink: false,
                append: false,
            },
            false,
        )
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

        let result = write_file(
            WriteFileArgs {
                path: file_path.to_string_lossy().into_owned(),
                content: "hello".to_string(),
                allow_shrink: false,
                append: false,
            },
            false,
        )
        .await;

        assert!(result.is_ok(), "got: {result:?}");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    fn append_args(path: &std::path::Path, content: &str) -> WriteFileArgs {
        WriteFileArgs {
            path: path.to_string_lossy().into_owned(),
            content: content.to_string(),
            allow_shrink: false,
            append: true,
        }
    }

    /// `append: true` crea el archivo (y sus padres) si falta y después
    /// agrega al final, sin tocar lo anterior.
    #[tokio::test]
    async fn append_creates_then_extends_without_discarding() {
        let dir = unique_temp_dir("write-file-append");
        let file_path = dir.join("notas").join("review-notes.md");

        let first = write_file(append_args(&file_path, "## L1-150\n- a\n"), false).await;
        assert!(first.is_ok(), "got: {first:?}");
        let second = write_file(append_args(&file_path, "## L151-300\n- b\n"), false).await;
        let summary = second.expect("second append");
        assert!(summary.contains("appended 16 bytes"), "{summary}");
        assert!(summary.contains("now 30 bytes"), "{summary}");

        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "## L1-150\n- a\n## L151-300\n- b\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Un append chico sobre un archivo grande NO es un shrink: nunca
    /// descarta nada, así que el guard no debe rechazarlo.
    #[tokio::test]
    async fn small_append_to_a_large_file_is_not_refused_as_a_shrink() {
        let dir = unique_temp_dir("write-file-append-large");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file_path = dir.join("big.md");
        let big = "x".repeat(SHRINK_PREFLIGHT_THRESHOLD_BYTES * 4);
        tokio::fs::write(&file_path, &big).await.unwrap();

        let result = write_file(append_args(&file_path, "y"), false).await;

        assert!(result.is_ok(), "got: {result:?}");
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content.len(), big.len() + 1);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// El gate sintáctico juzga el archivo resultante: agregar una función
    /// completa a un `.rs` sano pasa; agregar media función se rechaza
    /// sin escribir.
    #[tokio::test]
    async fn rust_append_is_gated_on_the_resulting_file() {
        let dir = unique_temp_dir("write-file-append-rs");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file_path = dir.join("lib.rs");
        tokio::fs::write(&file_path, "pub fn a() {}\n").await.unwrap();

        let ok = write_file(append_args(&file_path, "pub fn b() {}\n"), true).await;
        assert!(ok.is_ok(), "got: {ok:?}");

        let broken = write_file(append_args(&file_path, "pub fn c() {\n"), true).await;
        assert!(broken.is_err(), "got: {broken:?}");
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "pub fn a() {}\npub fn b() {}\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
