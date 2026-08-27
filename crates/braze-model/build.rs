//! Captura, en tiempo de compilación, la versión del crate de bindings
//! `llama-cpp-2` con la que se construyó este binario — la identidad del
//! motor de inferencia in-process del `LocalBackend`.
//!
//! Por qué en build.rs y no en runtime: cada versión de `llama-cpp-2`
//! vendorea un commit concreto de llama.cpp, y ese código queda *linkeado
//! dentro del binario*. No hay nada que consultar en la máquina donde el
//! binario corre — el motor no es un servicio con un endpoint de versión
//! (a diferencia de Ollama, cuya identidad de capa de servicio ya viaja en
//! `RunMetadata::ollama_server_version`). O se embebe al compilar, o se
//! pierde.
//!
//! Esto importa para la procedencia experimental: llama.cpp cambia
//! kernels, cuantización y decodificación entre versiones, así que dos
//! sweeps del LocalBackend construidos contra bindings distintos NO son la
//! misma condición, aunque coincidan modelo, seed y sampling. Sin este
//! campo, la metadata de un sweep `local:` sub-especificaba su propio
//! motor — la misma clase de hueco que el `ollama_server_version` faltante
//! antes de EMSE b2/Issue 3.
//!
//! Se lee del `Cargo.lock` (la versión *resuelta*, no el requisito `"0.1"`
//! del Cargo.toml, que no identifica nada).

use std::path::{Path, PathBuf};

// El parser vive en el crate para que sus tests corran en `cargo test` —
// ver la cabecera de ese archivo.
include!("src/lock_version.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/lock_version.rs");

    let lock = find_cargo_lock();
    if let Some(path) = &lock {
        // Re-ejecuta si alguien bumpea la dependencia: sin esto, el valor
        // embebido sobrevive al upgrade y la metadata miente sobre qué
        // motor se linkeó (justo el fallo que este campo existe para
        // impedir).
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let version = lock
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| locked_version(&text, "llama-cpp-2"));

    // `unknown` en vez de fallar el build: braze-model compila sin el
    // feature `local` en el build normal del workspace, y ahí no hay motor
    // que identificar. El consumidor distingue el caso — ver
    // `braze_model::local_engine_version`.
    println!(
        "cargo:rustc-env=BRAZE_LLAMA_CPP_2_VERSION={}",
        version.as_deref().unwrap_or("unknown")
    );
}

/// Sube desde `CARGO_MANIFEST_DIR` hasta encontrar un `Cargo.lock` — el
/// crate vive en `crates/braze-model/`, el lock en la raíz del workspace,
/// pero se busca en vez de asumir `../..` para no romperse si el layout
/// cambia (o si el crate se compila fuera del workspace).
fn find_cargo_lock() -> Option<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let mut dir: &Path = Path::new(&manifest_dir);
    loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}
