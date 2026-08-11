//! Sandbox out-of-process por-tool con bubblewrap (`bwrap`) — la cuarta
//! capa de seguridad, opcional e independiente del paquete in-process
//! (Landlock/seccomp/prctl de `braze-permissions`). Diseño y alcance
//! MVP: `docs/bwrap-tool-sandbox-design-2026-08-10.md`.
//!
//! Encierra los comandos que emite el MODELO vía `shell_exec` en un
//! mount namespace: el filesystem entra read-only por defecto
//! (`--ro-bind / /`), el workspace se bindea escribible, `.git/` vuelve
//! read-only encima (salvo para comandos git), y los archivos de
//! secretos (`.env`/`.env.*`) se enmascaran con un archivo `chmod 000`
//! de modo que `open()` da EACCES. Esto expresa las DOS cosas que el
//! Landlock write-only in-process no puede (`sandbox.rs:17-34`):
//! read-denial de secretos y `.git` read-only bajo un workdir
//! escribible.
//!
//! **No reemplaza** al paquete in-process — se suma. Se compone DEBAJO
//! del `PermissionGuard`: la decisión de permiso ya se tomó, esto solo
//! encierra la ejecución autorizada.
//!
//! Alcance MVP (ver el design doc para el detalle y lo diferido):
//! `Command::new("bwrap")` directo, sin plumbing de fds — sin
//! `--args fd` (los mounts van en la command line, ARG_MAX holgado para
//! decenas de paths) ni `--seccomp fd` (el anti-`ptrace`/`io_uring` ya
//! lo aporta braze al proceso propio). Si `bwrap` no está disponible, se
//! degrada corriendo SIN encierro con un warning (capa opt-in nueva:
//! romper todo `shell_exec` donde falta bwrap sería peor que el status
//! quo).

use std::path::{Path, PathBuf};

/// Todo lo que el builder de argv necesita para una invocación. Derivado
/// del comando ya autorizado + el workspace del provider.
pub(crate) struct BwrapSpec {
    /// Raíz escribible: el workdir del provider. Se bindea r/w; todo lo
    /// demás del FS queda read-only.
    pub workspace: PathBuf,
    /// ¿El comando es `git`? Entonces `.git/` se bindea escribible en vez
    /// de read-only (un commit/checkout legítimo tiene que poder escribir
    /// su propio `.git`).
    pub git_writable: bool,
    /// Rutas de secretos a enmascarar (absolutas). Cada una se bindea
    /// sobre `mask_file`.
    pub secrets: Vec<PathBuf>,
    /// El archivo `chmod 000` sobre el que se montan los secretos.
    pub mask_file: PathBuf,
    /// ¿Compartir la red del host? Sin esto, `--unshare-all` la aísla.
    pub allow_network: bool,
}

/// Un mount acumulado antes de ordenar. `flag` es el argumento bwrap
/// (`--bind`, `--ro-bind`), `src`/`dest` sus dos operandos.
struct Mount {
    flag: &'static str,
    src: PathBuf,
    dest: PathBuf,
}

/// Construye el argv COMPLETO para `bwrap` (sin el `bwrap` inicial): la
/// cabecera fija, los mounts ordenados por longitud de destino, el
/// separador `--`, y `program` + `args`. Función PURA — misma doctrina
/// testeable que `braze_permissions::build_syscall_filter`.
///
/// El orden por longitud de destino ascendente garantiza que un padre se
/// monte antes que su hijo (si no, el bind del padre ocultaría el del
/// hijo). El sort es estable, así que a igual longitud gana el que se
/// acumuló después — por eso los secretos y el `.git` read-only entran al
/// final: deben ganarle al bind r/w del workspace que los contiene.
pub(crate) fn build_bwrap_argv(spec: &BwrapSpec, program: &str, args: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "--unshare-all".into(),
        "--new-session".into(),
        "--die-with-parent".into(),
    ];
    if spec.allow_network {
        argv.push("--share-net".into());
    }
    // Cabecera del FS: todo read-only, con /dev, /proc y /tmp aislados.
    argv.extend([
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ]);

    let mut mounts: Vec<Mount> = Vec::new();
    // 1. Workspace escribible.
    mounts.push(Mount {
        flag: "--bind",
        src: spec.workspace.clone(),
        dest: spec.workspace.clone(),
    });
    // 2. `.git/` — read-only salvo para comandos git. Entra DESPUÉS del
    //    workspace para ganarle por el desempate del sort estable.
    let git_dir = spec.workspace.join(".git");
    mounts.push(Mount {
        flag: if spec.git_writable {
            "--bind"
        } else {
            "--ro-bind"
        },
        src: git_dir.clone(),
        dest: git_dir,
    });
    // 3. Secretos enmascarados — lo último, ganan a todo lo que los
    //    contiene.
    for secret in &spec.secrets {
        mounts.push(Mount {
            flag: "--bind",
            src: spec.mask_file.clone(),
            dest: secret.clone(),
        });
    }

    // Sort estable por longitud de la ruta destino (padres antes que
    // hijos); a igual longitud, el orden de acumulación de arriba manda.
    mounts.sort_by_key(|m| m.dest.as_os_str().len());

    for m in mounts {
        argv.push(m.flag.into());
        argv.push(m.src.to_string_lossy().into_owned());
        argv.push(m.dest.to_string_lossy().into_owned());
    }

    argv.push("--".into());
    argv.push(program.to_string());
    argv.extend(args.iter().cloned());
    argv
}

/// ¿Está `bwrap` en el `PATH`? Se resuelve una vez al construir el
/// provider; el resultado gobierna la degradación (design doc §
/// "degradar con warning").
pub(crate) fn bwrap_available() -> bool {
    which_bwrap().is_some()
}

/// Ruta absoluta de `bwrap` en el `PATH`, o `None`. Búsqueda manual (sin
/// crate extra) — el mismo patrón que el resto del workspace evita.
fn which_bwrap() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("bwrap");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// ¿Es `command[0]` un invocación de git? Basename, para que
/// `/usr/bin/git` cuente igual que `git`.
pub(crate) fn is_git_command(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .map(|n| n == "git")
        .unwrap_or(false)
}

/// Descubre archivos de secretos (`.env`, `.env.*`) bajo `workspace`,
/// hasta profundidad 3, podando directorios de build/deps ruidosos.
/// Shellea a `find` (como el blueprint) para no reimplementar el walk
/// con poda; degrada a vacío ante cualquier fallo — un secreto no
/// enmascarado es un riesgo, pero abortar el comando entero por un
/// `find` que falló sería peor y el resto del encierro sigue en pie.
pub(crate) async fn discover_secrets(workspace: &Path) -> Vec<PathBuf> {
    let output = tokio::process::Command::new("find")
        .arg(workspace)
        .args([
            "-maxdepth", "3", "-type", "d", "(", "-name", ".git", "-o", "-name", "node_modules",
            "-o", "-name", "target", "-o", "-name", ".venv", "-o", "-name", "__pycache__", "-o",
            "-name", "dist", "-o", "-name", "build", ")", "-prune", "-o", "-type", "f", "(",
            "-name", ".env", "-o", "-name", ".env.*", ")", "-print0",
        ])
        .kill_on_drop(true)
        .output()
        .await;

    let Ok(output) = output else {
        tracing::warn!("find failed while discovering secret files; none will be masked");
        return Vec::new();
    };
    // `-print0` → split por NUL sobre BYTES (nombres no-UTF8 son válidos
    // en Linux; decodificar a String los perdería — gotcha #4 del
    // blueprint).
    use std::os::unix::ffi::OsStrExt;
    output
        .stdout
        .split(|&b| b == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| PathBuf::from(std::ffi::OsStr::from_bytes(chunk)))
        .collect()
}

/// Crea el archivo-máscara `chmod 000` en un directorio temporal propio y
/// devuelve su ruta. El bind r/w de este archivo sobre un secreto hace
/// que `open()` falle con EACCES (por permisos DAC), pero el archivo
/// sigue *existiendo* — un `test -f .env` ve que está, solo no lo puede
/// leer. `None` si no se pudo crear (se degrada a no enmascarar).
pub(crate) fn create_mask_file() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("braze-bwrap-mask-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let mask = dir.join("mask");
    std::fs::write(&mask, b"").ok()?;
    std::fs::set_permissions(&mask, std::fs::Permissions::from_mode(0o000)).ok()?;
    Some(mask)
}

/// Pre-crea los governance files (`.gitignore`, `.git`) vacíos si no
/// existen, para poder congelarlos con `--ro-bind` (que falla si la
/// fuente no existe) — y para que el comando sandboxeado no los pueda
/// *crear* alterando la constitución del repo. Best-effort.
pub(crate) fn ensure_governance_files(workspace: &Path) {
    let gitignore = workspace.join(".gitignore");
    if !gitignore.exists() {
        let _ = std::fs::write(&gitignore, b"");
    }
    let git_dir = workspace.join(".git");
    if !git_dir.exists() {
        let _ = std::fs::create_dir_all(&git_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(ws: &str) -> BwrapSpec {
        BwrapSpec {
            workspace: PathBuf::from(ws),
            git_writable: false,
            secrets: Vec::new(),
            mask_file: PathBuf::from("/tmp/mask"),
            allow_network: false,
        }
    }

    fn positions(argv: &[String], flag: &str, dest: &str) -> Option<usize> {
        argv.windows(3)
            .position(|w| w[0] == flag && w[2] == dest)
    }

    #[test]
    fn header_is_fixed_and_first() {
        let argv = build_bwrap_argv(&spec("/ws"), "ls", &[]);
        assert_eq!(
            &argv[..9],
            &[
                "--unshare-all",
                "--new-session",
                "--die-with-parent",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
            ]
        );
    }

    #[test]
    fn no_network_by_default_and_share_net_when_asked() {
        let argv = build_bwrap_argv(&spec("/ws"), "ls", &[]);
        assert!(!argv.contains(&"--share-net".to_string()));
        let mut s = spec("/ws");
        s.allow_network = true;
        let argv = build_bwrap_argv(&s, "ls", &[]);
        assert!(argv.contains(&"--share-net".to_string()));
    }

    #[test]
    fn workspace_is_writable_bind() {
        let argv = build_bwrap_argv(&spec("/home/u/proj"), "ls", &[]);
        assert!(positions(&argv, "--bind", "/home/u/proj").is_some());
    }

    #[test]
    fn git_is_readonly_by_default_writable_for_git_commands() {
        let argv = build_bwrap_argv(&spec("/ws"), "cat", &[]);
        assert!(positions(&argv, "--ro-bind", "/ws/.git").is_some());
        let mut s = spec("/ws");
        s.git_writable = true;
        let argv = build_bwrap_argv(&s, "git", &["status".into()]);
        assert!(positions(&argv, "--bind", "/ws/.git").is_some());
        assert!(positions(&argv, "--ro-bind", "/ws/.git").is_none());
    }

    #[test]
    fn secrets_masked_and_after_workspace_bind() {
        let mut s = spec("/ws");
        s.secrets = vec![PathBuf::from("/ws/.env")];
        let argv = build_bwrap_argv(&s, "cat", &[".env".into()]);
        // El secreto se monta con el mask file como fuente.
        let i = argv
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == "/tmp/mask" && w[2] == "/ws/.env");
        assert!(i.is_some(), "secreto debe montar el mask file");
    }

    #[test]
    fn parent_mounts_before_children() {
        // Un secreto anidado profundo debe montarse DESPUÉS de la raíz
        // del workspace que lo contiene (orden por longitud de destino).
        let mut s = spec("/ws");
        s.secrets = vec![PathBuf::from("/ws/sub/dir/.env")];
        let argv = build_bwrap_argv(&s, "cat", &[]);
        let ws_i = positions(&argv, "--bind", "/ws").unwrap();
        let secret_i = argv
            .windows(3)
            .position(|w| w[2] == "/ws/sub/dir/.env")
            .unwrap();
        assert!(ws_i < secret_i, "el workspace debe bindarse antes que su secreto anidado");
    }

    #[test]
    fn command_and_args_after_separator() {
        let argv = build_bwrap_argv(&spec("/ws"), "grep", &["-r".into(), "foo".into()]);
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(&argv[sep + 1..], &["grep", "-r", "foo"]);
    }

    #[test]
    fn is_git_command_uses_basename() {
        assert!(is_git_command("git"));
        assert!(is_git_command("/usr/bin/git"));
        assert!(!is_git_command("gitk"));
        assert!(!is_git_command("ls"));
    }

    #[test]
    fn mask_file_is_created_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let Some(mask) = create_mask_file() else {
            return; // entorno sin /tmp escribible — no fallar el test
        };
        let mode = std::fs::metadata(&mask).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o000);
        let _ = std::fs::remove_file(&mask);
    }
}
