//! Carga del context file estándar `AGENTS.md` (interop v8/v9 — el
//! formato emergió como estándar entre harnesses; arXiv 2602.14690 mide
//! que los context files son la palanca de contexto dominante en la
//! práctica). braze lo lee del working directory y lo inyecta como
//! sección del system prompt, igual que hacen los demás coding agents
//! con su archivo equivalente.
//!
//! Solo `AGENTS.md` en la raíz del cwd, a propósito: sin jerarquías de
//! directorios ni archivos por-usuario en esta primera pasada — el valor
//! del estándar es que UN archivo versionado en el repo funcione en
//! cualquier harness, y esa es la parte interoperable. El contenido es
//! del proyecto (mismo nivel de confianza que la config del repo);
//! opt-out con `disable_agents_md`.

use std::path::Path;

/// Tope de bytes del contenido inyectado — misma cifra que el tope
/// default de output por tool result (`tool_output_max_bytes`, 8000):
/// suficiente para un context file real, y un archivo enorme no puede
/// desplazar al presupuesto de contexto del turno. El truncado se anota
/// para que el modelo sepa que hay más.
const AGENTS_MD_MAX_BYTES: usize = 8_000;

/// Lee `AGENTS.md` del directorio dado. `None` si no existe, no se puede
/// leer (un context file ilegible no debe abortar el arranque — se traza
/// y se sigue) o está vacío tras trim.
pub fn load_agents_md(cwd: &Path) -> Option<String> {
    let path = cwd.join("AGENTS.md");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "AGENTS.md ilegible; se ignora");
            return None;
        }
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= AGENTS_MD_MAX_BYTES {
        return Some(trimmed.to_string());
    }
    // Truncado en el borde de char más cercano bajo el tope.
    let cut = (0..=AGENTS_MD_MAX_BYTES)
        .rev()
        .find(|i| trimmed.is_char_boundary(*i))
        .unwrap_or(0);
    tracing::warn!(
        path = %path.display(),
        bytes = trimmed.len(),
        max = AGENTS_MD_MAX_BYTES,
        "AGENTS.md excede el tope; se inyecta truncado"
    );
    Some(format!(
        "{}\n\n[AGENTS.md truncado a {AGENTS_MD_MAX_BYTES} bytes de {} — leer el archivo \
         completo con read_file si hace falta]",
        &trimmed[..cut],
        trimmed.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("braze-agents-md-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_present_agents_md_is_loaded_trimmed() {
        let dir = temp_dir("present");
        std::fs::write(dir.join("AGENTS.md"), "\n# Reglas\n- usar rustfmt\n\n").unwrap();
        assert_eq!(
            load_agents_md(&dir).as_deref(),
            Some("# Reglas\n- usar rustfmt")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_empty_agents_md_is_none() {
        let dir = temp_dir("missing");
        assert_eq!(load_agents_md(&dir), None);
        std::fs::write(dir.join("AGENTS.md"), "  \n\t\n").unwrap();
        assert_eq!(load_agents_md(&dir), None, "vacío tras trim = ausente");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_agents_md_is_truncated_with_a_note() {
        let dir = temp_dir("oversized");
        // Multibyte a propósito: el corte tiene que caer en borde de char.
        let big = "á".repeat(AGENTS_MD_MAX_BYTES); // 2 bytes por char
        std::fs::write(dir.join("AGENTS.md"), &big).unwrap();
        let loaded = load_agents_md(&dir).expect("se inyecta truncado, no se descarta");
        assert!(loaded.contains("[AGENTS.md truncado"));
        assert!(loaded.len() < big.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
