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

/// Encuentra el `AGENTS.md` más cercano subiendo desde `from` (un
/// directorio) hasta `ceiling` INCLUSIVE, sin pasarse: el primer
/// directorio de la cadena `from → … → ceiling` que contenga un
/// `AGENTS.md` legible. `None` si ninguno lo tiene, o si `from` no está
/// bajo `ceiling` (el walk se corta al llegar al techo y jamás sube por
/// encima — garantía de confianza: nunca alcanza un `AGENTS.md` de
/// `$HOME` o del sistema). Devuelve la ruta del archivo, sin leerlo —
/// el caller decide con [`load_agents_md_from`] (dedup por path primero).
///
/// Las rutas se comparan canonicalizadas para que el techo se detecte
/// aunque `from` traiga `..`/symlinks; si la canonicalización falla
/// (ruta inexistente), se usa la ruta lexical y el walk se corta igual
/// por el conteo de ancestros.
pub fn find_nearest_agents_md(from: &Path, ceiling: &Path) -> Option<std::path::PathBuf> {
    let ceiling = std::fs::canonicalize(ceiling).unwrap_or_else(|_| ceiling.to_path_buf());
    let mut dir = std::fs::canonicalize(from).unwrap_or_else(|_| from.to_path_buf());
    loop {
        let candidate = dir.join("AGENTS.md");
        if candidate.is_file() {
            return Some(candidate);
        }
        if dir == ceiling {
            // Llegamos al techo sin encontrar nada: no se sube más.
            return None;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => return None,
        }
        // Salvaguarda: si `from` no era descendiente de `ceiling`, el
        // bucle igual termina al agotar los ancestros (parent = None).
    }
}

/// Lee y prepara (trim + cap) el `AGENTS.md` en `dir` — el mismo
/// tratamiento que [`load_agents_md`] da al raíz, factorizado para que la
/// carga JIT por subdirectorio lo reutilice sobre cualquier directorio.
pub fn load_agents_md_from(dir: &Path) -> Option<String> {
    load_agents_md(dir)
}

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

    #[test]
    fn nearest_agents_md_is_found_walking_up_to_the_ceiling() {
        let root = temp_dir("jit-root");
        let sub = root.join("crates/foo/src");
        std::fs::create_dir_all(&sub).unwrap();
        // AGENTS.md intermedio en crates/foo.
        std::fs::write(root.join("crates/foo/AGENTS.md"), "# foo rules").unwrap();
        // Un touch en crates/foo/src encuentra el de crates/foo.
        let found = find_nearest_agents_md(&sub, &root).expect("debe encontrar el intermedio");
        assert_eq!(found, std::fs::canonicalize(root.join("crates/foo/AGENTS.md")).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn walk_up_stops_at_the_ceiling_and_never_goes_above() {
        let root = temp_dir("jit-ceiling");
        let sub = root.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        // AGENTS.md solo POR ENCIMA del techo (en el padre de root).
        std::fs::write(root.join("AGENTS.md"), "# root").unwrap();
        let above = root.parent().unwrap().join("AGENTS.md");
        let _ = std::fs::write(&above, "# fuera del proyecto");
        // Con techo = root, un touch en a/b encuentra el root, no el de arriba.
        let found = find_nearest_agents_md(&sub, &root).expect("encuentra el root");
        assert_eq!(found, std::fs::canonicalize(root.join("AGENTS.md")).unwrap());
        // Sin ningún AGENTS.md dentro del árbol: None, jamás el de arriba.
        std::fs::remove_file(root.join("AGENTS.md")).unwrap();
        assert_eq!(
            find_nearest_agents_md(&sub, &root),
            None,
            "no debe escapar el techo hacia el AGENTS.md de afuera"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_agents_md_anywhere_is_none() {
        let root = temp_dir("jit-none");
        let sub = root.join("x/y");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(find_nearest_agents_md(&sub, &root), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
