//! Gate sintáctico pre-aplicación (survey de referencia 2026-08-10,
//! hallazgo Tier-1 de SWE-agent `tools/windowed_edit_linting`): antes de
//! que una edición aterrice en disco, se verifica que no INTRODUZCA un
//! error de sintaxis en un archivo Rust. Si lo haría, la edición se
//! RECHAZA sin escribir — el archivo queda siempre válido, que es un
//! invariante mucho más limpio para un modelo chico que razonar sobre un
//! intermedio corrupto.
//!
//! Complementa —no reemplaza— al post-edit `cargo check` de
//! `post_edit_check.rs`: este gate es un parse instantáneo con `syn`
//! (sin spawnear cargo) que ataca la clase SINTÁCTICA *antes* de
//! escribir; el `cargo check` sigue corriendo *después* de una edición
//! exitosa para la clase de TIPOS/semántica. Barato arriba, caro abajo.
//!
//! Regla de atribución (el pre/post-diff de SWE-agent, aquí binario
//! porque el parse es todo-o-nada): solo se bloquea si el error es
//! NUEVO. Si el archivo original ya no parseaba, no se bloquea nada —
//! el modelo puede estar justamente arreglándolo, y un `syn` que no
//! entiende cierta sintaxis del archivo (macro exótica, sintaxis
//! nightly) no debe volverse un falso positivo que traba toda edición a
//! ese archivo. Un archivo nuevo (sin original) sí se protege: crear un
//! `.rs` roto es la misma clase de corrupción.

use std::path::Path;

/// ¿Es un archivo Rust por extensión? El gate solo aplica a `.rs` —
/// mismo criterio por-extensión que el post-edit check.
pub(crate) fn is_rust(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
}

/// `Ok(())` = la edición puede aplicarse. `Err(mensaje)` = la edición
/// introduciría un error de sintaxis y NO debe escribirse; el mensaje es
/// accionable y trae el nudge anti-loop. `original` es `None` para un
/// archivo nuevo. `edit_noun` nombra qué corregir según la tool
/// (`"new_string"` / `"content"`).
pub(crate) fn check_rust_edit(
    path: &Path,
    original: Option<&str>,
    proposed: &str,
    edit_noun: &str,
) -> Result<(), String> {
    if !is_rust(path) {
        return Ok(());
    }
    let err = match syn::parse_file(proposed) {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    // El propuesto no parsea. Atribuir solo si el original SÍ parseaba
    // (o no existía): si ya venía roto, no se puede culpar a esta edición
    // y bloquearla impediría un arreglo legítimo.
    if original.is_some_and(|o| syn::parse_file(o).is_err()) {
        return Ok(());
    }
    Err(format!(
        "the edit was NOT applied and '{}' is unchanged: it would introduce a Rust syntax \
         error ({err}). Fix your {edit_noun} and send a corrected edit — do NOT re-send the \
         same one. (This is a fast parse check that runs before the edit lands; type/semantic \
         errors are still reported separately by the post-edit check after a successful edit.)",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_non_rust_file_is_never_gated() {
        assert!(
            check_rust_edit(Path::new("notas.txt"), Some("ok"), "{{{ broken", "content").is_ok()
        );
    }

    #[test]
    fn valid_rust_passes() {
        assert!(
            check_rust_edit(
                Path::new("lib.rs"),
                Some("fn a() {}"),
                "fn a() {}\nfn b() {}",
                "new_string"
            )
            .is_ok()
        );
    }

    #[test]
    fn an_edit_that_breaks_previously_valid_rust_is_rejected() {
        let err = check_rust_edit(
            Path::new("lib.rs"),
            Some("fn a() {}"),
            "fn a() { ", // llave sin cerrar
            "new_string",
        )
        .expect_err("debe rechazar");
        assert!(err.contains("NOT applied"), "got: {err}");
        assert!(err.contains("syntax error"), "got: {err}");
    }

    #[test]
    fn an_already_broken_file_is_not_blocked() {
        // Original NO parsea → no se atribuye a esta edición (el modelo
        // puede estar arreglándolo, o syn no entiende el archivo).
        assert!(
            check_rust_edit(
                Path::new("lib.rs"),
                Some("fn a( {"),
                "fn a( { still broken",
                "new_string"
            )
            .is_ok()
        );
    }

    #[test]
    fn a_new_rust_file_with_broken_content_is_rejected() {
        let err = check_rust_edit(Path::new("nuevo.rs"), None, "fn x( {", "content")
            .expect_err("crear un .rs roto se rechaza");
        assert!(err.contains("NOT applied"), "got: {err}");
    }

    #[test]
    fn a_new_rust_file_with_valid_content_passes() {
        assert!(check_rust_edit(Path::new("nuevo.rs"), None, "pub fn x() {}", "content").is_ok());
    }
}
