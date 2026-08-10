//! Sandbox de proceso (v9 Paquete 4 — hereda de v8 § "Sandboxing OS",
//! ítem 16). Tres capas in-process, todas Linux-only, todas opt-in bajo
//! `Config::enable_landlock_write_sandbox`:
//!
//! 1. [`apply_write_sandbox`] — **Landlock write-only**: restricción a
//!    nivel de KERNEL de las escrituras del filesystem a una allowlist de
//!    raíces, lecturas libres. Cierra la clase K-2/J-20/J-31 — el gate
//!    léxico de la capa 2 clasifica la *descripción* de una acción, y
//!    symlinks, rutas creativas o un subproceso de `shell_exec` escriben
//!    donde la descripción no alcanza; Landlock restringe el syscall.
//! 2. [`apply_syscall_hardening`] — **seccomp** que deniega
//!    `io_uring_*`/`ptrace`/`process_vm_*` (paquete de seguridad
//!    2026-08-10): las clases de bypass que Landlock write-only no cubre.
//! 3. [`harden_process`] — **prctl + env scrub**: sin core dumps, sin
//!    new-privs, sin `LD_PRELOAD` heredado.
//!
//! ## Lo que este sandbox NO puede hacer (restricción de mecanismo)
//!
//! Landlock es **allowlist-only, sin reglas de deny**, y el permiso más
//! amplio de la jerarquía gana. Por eso **no** son expresables in-process
//! y quedaron deliberadamente fuera del paquete de seguridad:
//! - *Denegar lectura de rutas secretas* (`~/.ssh`, `.env`) manteniendo
//!   lecturas amplias: habría que handle-ar READ y enumerar TODA raíz
//!   legítima de lectura (toolchain, sysroot, workdir…) excluyendo los
//!   secretos — frágil, rompe cargo/rustc. No hay "denegar bajo esta
//!   ruta".
//! - *`.git/hooks` read-only dentro de un workdir escribible*: el
//!   write-allow del workdir cubre todos sus descendientes; no se puede
//!   agregar una regla más restrictiva bajo un padre ya permitido.
//!
//! El fix real de ambos es un mount namespace (bubblewrap, el modelo
//! out-of-process de codex) — trabajo futuro. Mientras tanto, la
//! protección de `.git/`/`.braze/` vive en el clasificador (capa 2), que
//! es la capa correcta para ella.
//!
//! Tampoco se deniega red in-process: braze necesita red para sus propios
//! backends API/Ollama, y el sandbox aplica al proceso entero.
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

/// Endurecimiento de proceso previo al sandbox (paquete de seguridad v9,
/// espejo del `process-hardening` de codex): cierra vectores que ni
/// Landlock ni seccomp cubren y que son baratos.
///
/// - `PR_SET_DUMPABLE=0`: sin core dumps → un crash no vuelca la memoria
///   del proceso (que pudo haber leído credenciales/keys de API) a disco.
/// - `PR_SET_NO_NEW_PRIVS=1`: ningún `execve` posterior gana privilegios
///   vía setuid/setgid — y es además el prerequisito para instalar un
///   filtro seccomp sin CAP_SYS_ADMIN.
/// - Scrub de `LD_PRELOAD`/`LD_AUDIT`/`DYLD_*`: un preload heredado
///   inyecta código en cada subproceso que braze lance (`shell_exec`,
///   `cargo check`); se limpia ANTES de spawnear nada.
///
/// Debe correr en `main()` antes del runtime de tokio y antes de
/// spawnear cualquier subproceso — el scrub de env solo protege a los
/// hijos creados después. Best-effort: un prctl que falla se traza y no
/// aborta (el sandbox de escritura es la protección primaria).
#[cfg(target_os = "linux")]
pub fn harden_process() {
    // SAFETY: prctl con argumentos válidos y constantes; sin efectos
    // sobre memoria del caller.
    unsafe {
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "hardening: PR_SET_DUMPABLE falló (core dumps no deshabilitados)"
            );
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "hardening: PR_SET_NO_NEW_PRIVS falló"
            );
        }
    }
    // SAFETY: corre en main() antes de que exista cualquier otro thread
    // (contrato del module doc), así que la mutación de env no compite.
    unsafe {
        for var in [
            "LD_PRELOAD",
            "LD_AUDIT",
            "DYLD_INSERT_LIBRARIES",
            "DYLD_LIBRARY_PATH",
        ] {
            std::env::remove_var(var);
        }
    }
}

/// Los syscalls que el filtro seccomp deniega — ninguno lo usa braze ni
/// tokio (epoll por default, no io_uring), así que denegarlos con EPERM
/// es seguro y cierra clases reales de bypass del sandbox:
/// - `io_uring_*`: autoridad ambiente que saltea la mediación de
///   syscalls (un io_uring puede hacer I/O sin volver a pasar por el
///   filtro) — el hueco más citado de un sandbox seccomp/Landlock.
/// - `ptrace`/`process_vm_readv`/`process_vm_writev`: leer o inyectar en
///   la memoria de otro proceso (incl. escaparse adjuntándose a un
///   proceso menos restringido).
#[cfg(target_os = "linux")]
const HARDENED_DENIED_SYSCALLS: &[libc::c_long] = &[
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
];

/// Compila el filtro seccomp de endurecimiento: allow-por-default
/// (`mismatch_action = Allow`) con EPERM para los syscalls de
/// [`HARDENED_DENIED_SYSCALLS`] (`match_action`, reglas vacías = matchean
/// siempre). Separado de la aplicación para poder testear que compila sin
/// restringir el proceso de test.
#[cfg(target_os = "linux")]
fn build_syscall_filter() -> Result<seccompiler::BpfProgram, SandboxError> {
    use seccompiler::{SeccompAction, SeccompFilter};

    let rules = HARDENED_DENIED_SYSCALLS
        .iter()
        .map(|&nr| (nr, Vec::new()))
        .collect();
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow, // default: permitir todo lo demás
        SeccompAction::Errno(libc::EPERM as u32), // los listados: EPERM
        std::env::consts::ARCH
            .try_into()
            .map_err(|e| SandboxError(format!("arch no soportada por seccomp: {e:?}")))?,
    )
    .map_err(|e| SandboxError(format!("build del filtro seccomp: {e}")))?;
    filter
        .try_into()
        .map_err(|e| SandboxError(format!("compilación del filtro seccomp: {e}")))
}

/// Instala el filtro seccomp de endurecimiento en todos los threads del
/// proceso (TSYNC). Debe correr antes del runtime de tokio; seccompiler
/// setea `PR_SET_NO_NEW_PRIVS` por sí mismo antes de instalar. Ver
/// [`build_syscall_filter`] por qué estos syscalls son seguros de
/// denegar. Best-effort a nivel del caller: en un kernel sin seccomp
/// esto devuelve `Err` y el caller decide (warning, no abort — el
/// sandbox de escritura sigue siendo la protección primaria).
#[cfg(target_os = "linux")]
pub fn apply_syscall_hardening() -> Result<(), SandboxError> {
    let program = build_syscall_filter()?;
    seccompiler::apply_filter_all_threads(&program)
        .map_err(|e| SandboxError(format!("apply del filtro seccomp: {e}")))
}

/// Stubs no-Linux: seccomp y prctl son de Linux. En otros SO estas dos
/// son no-ops (el caller ya loguea que el sandbox no se enforza).
#[cfg(not(target_os = "linux"))]
pub fn harden_process() {}

#[cfg(not(target_os = "linux"))]
pub fn apply_syscall_hardening() -> Result<(), SandboxError> {
    Ok(())
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

    /// El filtro seccomp de endurecimiento COMPILA a un programa BPF no
    /// vacío. No se APLICA en el test a propósito: `apply_filter_all_threads`
    /// restringiría el binario de test entero (TSYNC) y es irreversible —
    /// la aplicación real se verifica en vivo. Esto cubre que la lista de
    /// syscalls y la arquitectura son válidas para seccompiler.
    #[test]
    fn the_syscall_hardening_filter_compiles_to_a_nonempty_bpf_program() {
        let program = build_syscall_filter().expect("el filtro debe compilar");
        assert!(
            !program.is_empty(),
            "un filtro con syscalls denegados no puede ser vacío"
        );
    }
}
