//! Embebe el commit de git que construyó este binario.
//!
//! Cierra el caveat que `main.rs` dejó anotado como mejora futura al
//! capturar la identidad del sweep: `git rev-parse HEAD` en runtime
//! describe el *directorio desde el que se lanzó el bench*, que no tiene
//! por qué ser el commit que compiló el binario. Dos formas de mentir,
//! ambas observadas:
//!
//! 1. El binario se compiló en un commit anterior al HEAD del arranque
//!    (se recompiló tarde, o no se recompiló) — el sweep queda atribuido
//!    a código que no corrió.
//! 2. El binario se copió a la máquina de benchmark sin el árbol de
//!    fuentes. En Nitro, `~/braze` no es un checkout git, así que la
//!    captura en runtime devolvía `None` y TODOS los sweeps del nodo
//!    quedaron sin procedencia de harness (verificado en los JSON del A/B
//!    de weight-quant: `braze_git_commit: null`).
//!
//! El commit de build-time no tiene ninguno de los dos problemas: viaja
//! dentro del ejecutable.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Re-ejecuta cuando cambia el commit: `HEAD` cubre los checkouts de
    // rama, y el ref apuntado cubre los commits nuevos sobre la misma
    // rama. Sin esto, cargo cachea el build script y el commit embebido
    // envejece en silencio.
    if let Some(git_dir) = git_dir() {
        println!("cargo:rerun-if-changed={}/HEAD", git_dir.trim_end());
        // `logs/HEAD` se toca en commit, checkout, merge y rebase — es el
        // disparador más barato que cubre "el HEAD se movió" sin tener
        // que resolver a qué ref apunta.
        println!("cargo:rerun-if-changed={}/logs/HEAD", git_dir.trim_end());
    }

    let commit = run_git(&["rev-parse", "HEAD"]);

    // Un árbol sucio significa que el binario incluye cambios que ningún
    // commit describe: la procedencia es el commit MÁS lo no commiteado, y
    // callarlo haría pasar por reproducible algo que no lo es.
    let dirty = run_git(&["status", "--porcelain"])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(false);

    let value = match (commit, dirty) {
        (Some(c), true) => format!("{c}-dirty"),
        (Some(c), false) => c,
        // Cadena vacía = no se pudo determinar (build desde un tarball,
        // git ausente). El consumidor la traduce a `None` en vez de
        // registrar un valor inventado.
        (None, _) => String::new(),
    };
    println!("cargo:rustc-env=BRAZE_BUILD_GIT_COMMIT={value}");
}

fn git_dir() -> Option<String> {
    run_git(&["rev-parse", "--absolute-git-dir"])
}

/// `git` best-effort — `None` ante cualquier fallo. Nunca revienta el
/// build: la procedencia es un diagnóstico, no un requisito de compilación
/// (misma postura que `metadata::current_git_commit`).
fn run_git(args: &[&str]) -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let output = Command::new("git")
        // Corre desde el manifest, no desde el cwd de cargo: es el único
        // directorio que se sabe dentro del árbol de este crate.
        .current_dir(&manifest_dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}
