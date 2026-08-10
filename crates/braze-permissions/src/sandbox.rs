//! Sandbox Landlock write-only (v9 Paquete 4 — hereda de v8 § "Sandboxing
//! OS", ítem 16): restricción a nivel de KERNEL de los accesos de
//! escritura del filesystem a una allowlist de raíces, con las lecturas
//! libres. Cierra de raíz la clase K-2/J-20/J-31 — el gate léxico de la
//! capa 2 clasifica la *descripción* de una acción, y symlinks, rutas
//! creativas o un subproceso de `shell_exec` pueden escribir donde la
//! descripción no alcanza; Landlock restringe el syscall mismo y se
//! hereda por todos los procesos hijos.
//!
//! Alcance deliberado de esta primera pasada:
//!
//! - **Solo escrituras** (`AccessFs::from_write`): denegar lecturas
//!   rompería la mitad de las herramientas de diagnóstico del modelo
//!   para ganar poco — la clase de daño documentada es la escritura.
//! - **Opt-in** (`Config::enable_landlock_write_sandbox`): la allowlist
//!   de raíces necesita validarse contra flujos reales (cargo escribe en
//!   `~/.cargo`, los shells redirigen a `/dev/null`) antes de promoverla
//!   a default — misma doctrina que toda palanca del proyecto.
//! - **Best-effort por ABI** (`CompatLevel::BestEffort`): en un kernel
//!   viejo el ruleset degrada en vez de fallar; el caller decide qué
//!   hacer con un status parcial. En quien lo pidió explícitamente, un
//!   fallo DURO de aplicación es fatal (fail-closed) — correr sin el
//!   sandbox que el usuario cree tener sería mentirle.
//!
//! El sandbox aplica al THREAD que llama y se hereda por los threads y
//! procesos creados DESPUÉS — por eso `braze-cli` lo aplica en `main()`
//! síncrono, antes de construir el runtime de tokio: aplicarlo dentro
//! del async main dejaría sin restringir a los worker threads que el
//! runtime ya arrancó.

use std::path::PathBuf;

/// Qué tan enforzado quedó el sandbox tras aplicarlo — refleja el
/// `RulesetStatus` de Landlock sin exponer el crate en la API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteSandboxStatus {
    /// Todos los accesos pedidos quedaron restringidos.
    FullyEnforced,
    /// El kernel soporta una ABI anterior: parte de los accesos quedó
    /// restringida (p.ej. sin `TRUNCATE`, ABI < 3). Mejor que nada, y el
    /// caller lo loguea para que el operador sepa qué tiene.
    PartiallyEnforced,
    /// El kernel no soporta Landlock (o el SO no es Linux): nada quedó
    /// restringido.
    NotEnforced,
}

/// Fallo duro al construir o aplicar el ruleset — distinto de "el kernel
/// no soporta", que es [`WriteSandboxStatus::NotEnforced`].
#[derive(Debug, thiserror::Error)]
#[error("landlock write sandbox failed: {0}")]
pub struct SandboxError(String);

/// Aplica el sandbox write-only con las raíces dadas como únicos
/// destinos de escritura permitidos. Raíces inexistentes se saltan con
/// warning (una reference dir configurada pero ausente no debe abortar
/// el arranque). Ver el module doc por el contrato de threads.
#[cfg(target_os = "linux")]
pub fn apply_write_sandbox(write_roots: &[PathBuf]) -> Result<WriteSandboxStatus, SandboxError> {
    use landlock::{
        ABI, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus,
    };

    // ABI v3 = hasta `TRUNCATE` (kernel 6.2+); BestEffort degrada a lo
    // que el kernel corriente soporte en vez de fallar.
    let abi = ABI::V3;
    let write_access = AccessFs::from_write(abi);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(write_access)
        .map_err(|e| SandboxError(e.to_string()))?
        .create()
        .map_err(|e| SandboxError(e.to_string()))?;

    for root in write_roots {
        match PathFd::new(root) {
            Ok(fd) => {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(fd, write_access))
                    .map_err(|e| SandboxError(e.to_string()))?;
            }
            Err(err) => {
                tracing::warn!(
                    root = %root.display(),
                    error = %err,
                    "landlock: raíz de escritura inexistente/inaccesible — se salta"
                );
            }
        }
    }

    let status = ruleset
        .restrict_self()
        .map_err(|e| SandboxError(e.to_string()))?;
    Ok(match status.ruleset {
        RulesetStatus::FullyEnforced => WriteSandboxStatus::FullyEnforced,
        RulesetStatus::PartiallyEnforced => WriteSandboxStatus::PartiallyEnforced,
        RulesetStatus::NotEnforced => WriteSandboxStatus::NotEnforced,
    })
}

/// Stub no-Linux: Landlock es un LSM de Linux; en otros SO el sandbox
/// simplemente no existe y se reporta como tal — el caller loguea y (si
/// el usuario lo pidió explícitamente) decide si eso es aceptable.
#[cfg(not(target_os = "linux"))]
pub fn apply_write_sandbox(_write_roots: &[PathBuf]) -> Result<WriteSandboxStatus, SandboxError> {
    Ok(WriteSandboxStatus::NotEnforced)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// El contrato entero en un test: dentro de la raíz permitida se
    /// escribe; fuera, el KERNEL deniega — no la capa léxica. Corre en
    /// su propio thread (libtest: un thread por test), así la
    /// restricción muere con el test y no contamina a los demás.
    #[test]
    fn writes_inside_the_root_pass_and_outside_are_denied_by_the_kernel() {
        let allowed =
            std::env::temp_dir().join(format!("braze-landlock-allowed-{}", std::process::id()));
        let denied =
            std::env::temp_dir().join(format!("braze-landlock-denied-{}", std::process::id()));
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&denied).unwrap();

        let status =
            apply_write_sandbox(std::slice::from_ref(&allowed)).expect("aplicar el sandbox");
        if status == WriteSandboxStatus::NotEnforced {
            // Kernel sin Landlock: no hay nada que verificar acá. El
            // caller de producción loguea este caso; el test no puede
            // fingir un kernel distinto.
            eprintln!("landlock no soportado por este kernel; test vacío");
            return;
        }

        std::fs::write(allowed.join("adentro.txt"), "ok")
            .expect("escribir DENTRO de la raíz permitida debe pasar");

        let err = std::fs::write(denied.join("afuera.txt"), "no")
            .expect_err("escribir FUERA debe denegarla el kernel");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "la denegación es EACCES del kernel, got: {err}"
        );

        // Limpieza solo de lo permitido — el archivo denegado no existe.
        let _ = std::fs::remove_dir_all(&allowed);
        // remove_dir_all(&denied) fallaría: borrar también es escritura.
    }
}
