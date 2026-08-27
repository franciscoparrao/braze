// Parseo de la versión resuelta de una dependencia desde `Cargo.lock`.
//
// Comentarios `//` y no `//!`: este archivo lo comparten DOS
// compilaciones —`build.rs` lo trae por `include!` (necesita la función
// antes de que exista el crate) y el crate lo compila bajo `cfg(test)`
// para que sus tests corran de verdad en `cargo test`— y un doc-comment
// de módulo es inválido inyectado a media altura de otro archivo.
//
// El `include!` existe porque un `mod tests` dentro de `build.rs` no lo
// ejecuta nadie: cargo no compila los tests de un build script. Sería
// cobertura decorativa.
//
// Parseo textual deliberado: el formato de `Cargo.lock` es regular y
// estable, y una build-dependency sobre `toml` solo para esto no se paga
// sola.

/// Extrae la versión resuelta de `name` de un `Cargo.lock`.
///
/// El formato es una secuencia de bloques `[[package]]` con `name` y
/// `version` en líneas propias; se toma el primer `version` que sigue al
/// `name` buscado. Devuelve `None` si el paquete no está en el lock (p.ej.
/// un lock generado sin las dependencias opcionales).
pub fn locked_version(lock: &str, name: &str) -> Option<String> {
    let needle = format!("name = \"{name}\"");
    // Match por línea COMPLETA, no por substring: `llama-cpp-sys-2`
    // contiene a `llama-cpp-2` como subcadena, así que un `contains`
    // resolvería el wrapper contra el bloque del `-sys`.
    let mut lines = lock.lines().skip_while(|line| line.trim() != needle);
    // Consume la línea del `name` para que el `version` encontrado sea el
    // del MISMO bloque.
    lines.next()?;
    for line in lines {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("version = \"") {
            return rest.strip_suffix('"').map(str::to_string);
        }
        // Un bloque nuevo antes del `version` = paquete sin versión
        // declarada. No se inventa un valor.
        if line == "[[package]]" {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::locked_version;

    const LOCK: &str = r#"
[[package]]
name = "llama-cpp-sys-2"
version = "0.1.151"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "llama-cpp-2"
version = "0.1.152"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde"
version = "1.0.200"
"#;

    #[test]
    fn extracts_the_resolved_version() {
        assert_eq!(
            locked_version(LOCK, "llama-cpp-2"),
            Some("0.1.152".to_string())
        );
    }

    /// El `-sys` comparte subcadena con el wrapper y aparece ANTES en el
    /// lock: si el match no fuera por línea completa, `llama-cpp-2`
    /// devolvería 0.1.151 (la del `-sys`) en vez de 0.1.152.
    #[test]
    fn does_not_confuse_the_sys_crate_with_the_wrapper() {
        assert_eq!(
            locked_version(LOCK, "llama-cpp-2"),
            Some("0.1.152".to_string())
        );
        assert_eq!(
            locked_version(LOCK, "llama-cpp-sys-2"),
            Some("0.1.151".to_string())
        );
    }

    #[test]
    fn absent_package_is_none() {
        assert_eq!(locked_version(LOCK, "no-existe"), None);
    }

    /// El fixture prueba el parser; esto prueba que el parser sirve para
    /// lo que existe. Si el formato de `Cargo.lock` cambiara, o si
    /// `llama-cpp-2` desapareciera del lock, el build script embebería
    /// `unknown` en silencio y la metadata de cada sweep `local:` diría
    /// tener procedencia sin tenerla. Es un fallo mudo — por eso se
    /// verifica contra el archivo real.
    #[test]
    fn resolves_llama_cpp_2_against_the_real_workspace_lock() {
        let lock_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock");
        let Ok(lock) = std::fs::read_to_string(lock_path) else {
            return; // compilado fuera del workspace — no hay nada que verificar
        };
        let version = locked_version(&lock, "llama-cpp-2")
            .expect("`llama-cpp-2` debe estar en el lock: es la dep del feature `local`");
        assert!(
            version.starts_with("0.1."),
            "versión inesperada, revisar el contrato de `local_engine_version`: {version}"
        );
    }

    #[test]
    fn package_without_version_is_none_not_the_next_packages_version() {
        let lock = r#"
[[package]]
name = "sin-version"

[[package]]
name = "otro"
version = "9.9.9"
"#;
        assert_eq!(locked_version(lock, "sin-version"), None);
    }
}
