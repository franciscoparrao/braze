# Diseño: sandbox out-of-process por-tool con bubblewrap

Fecha: 2026-08-10
Origen: survey de referencia (`docs/reference-agents-survey-2026-08-10.md`
§ gemini-cli), que convirtió el "trabajo futuro" difuso del module doc de
`crates/braze-permissions/src/sandbox.rs:31` en un blueprint concreto.
Blueprint operativo extraído del clon de gemini-cli
(`packages/core/src/sandbox/linux/`).

## El hueco que llena

El paquete de seguridad in-process (Landlock write-only + seccomp +
prctl, todo bajo `enable_landlock_write_sandbox`) declara textualmente
sus dos límites de mecanismo (`sandbox.rs:17-34`):

1. **No puede denegar LECTURA de secretos** (`.env`, `~/.ssh`) —
   Landlock es allowlist-only sin reglas de deny; el permiso más amplio
   de la jerarquía gana.
2. **No puede hacer `.git/` read-only dentro de un workdir escribible** —
   el write-allow del workdir cubre todos sus descendientes.

Un mount namespace (bubblewrap) expresa las dos: monta el FS
read-only por defecto y bindea selectivamente lo escribible, así el
default es deny y cada excepción es explícita. Esto NO reemplaza al
paquete in-process — se suma como cuarta capa, opcional e independiente,
que encierra los comandos del MODELO (`shell_exec`) sin tocar la
protección global-al-proceso.

## Alcance MVP (deliberadamente recortado)

Lo que ENTRA:

- **Solo `shell_exec`** — los comandos que emite el modelo. El punto de
  inyección es `shell_exec::shell_exec` (`shell_exec.rs:80`): construye
  el argv `bwrap [config] -- <program> <args>` y delega en el `run`
  existente, heredando timeout, `kill_on_drop` y captura de output. Con
  `--die-with-parent`, el kill de tokio al proceso bwrap mata el árbol.
- **FS read-only por defecto** (`--ro-bind / /`) + workspace escribible
  (`--bind` del `WorkdirAllowlist`) + `/dev`, `/proc`, `/tmp` aislados.
- **Enmascarado de secretos**: `.env`/`.env.*` descubiertos con `find`
  (maxdepth 3, prune de `.git`/`node_modules`/`target`/`__pycache__`) y
  montados sobre un **mask file** `chmod 000` — `open()` da EACCES. (Se
  usa mask file, no `--ro-bind /dev/null`, para que el archivo siga
  *existiendo* — un tool que hace `test -f .env` ve que está, pero no lo
  puede leer; menos señal de que hay un sandbox.)
- **`.git/` read-only** (`--ro-bind` sobre el bind r/w del workspace,
  montado DESPUÉS por el orden de longitud → gana). Governance files
  (`.gitignore`, `.git`) pre-creados vacíos si no existen, para poder
  congelarlos.
- **Sin red por defecto** (`--unshare-all`), con override
  `bwrap_allow_network`.
- **Detección + degradación explícita**: `which bwrap` al construir el
  provider. Si falta y el sandbox se pidió: warning y se corre SIN
  encierro (NO fail-closed en MVP — a diferencia de Landlock, este es
  una capa nueva opt-in y romper todo `shell_exec` en una máquina sin
  bwrap sería peor que el status quo; el fail-closed se reconsidera si
  se promueve a default). El warning es una sola vez, no por comando.

Lo que se DIFIERE (con razón, anotado para no re-descubrirlo):

- **seccomp propio del bwrap** (`--seccomp fd`): el BPF anti-ptrace de 7
  instrucciones. braze YA aplica seccomp anti-`ptrace`/`io_uring` al
  proceso propio bajo el paquete de seguridad; el FS es el 90% del valor
  que Landlock no da. Diferido junto con todo el **plumbing de fds** —
  sin `--seccomp fd` ni `--args fd`, el wrapper es `Command::new("bwrap")`
  directo, sin `sh -c`/`pre_exec`/`dup2`. Límite aceptado: los mounts
  van en la command line (ARG_MAX ~2MB); con workspace + governance +
  unas pocas decenas de secretos son ~20-50 args, holgado. Si la
  política llegara a cientos de paths, migrar a `--args fd` (gotcha #6
  del blueprint: `dup2` en `pre_exec` para 8/9).
- **Política por-comando configurable** (el TOML de gemini-cli): MVP usa
  una política fija derivada del `WorkdirAllowlist` + reglas
  `.git`/`.braze` del clasificador. El `.git` escribible solo para
  comandos `git` (flag `isGitCommand`) SÍ entra — es barato y correcto.
- **Relajar el gate bajo sandbox** (auto-aprobar `Irreversible` porque
  el encierro los hace seguros, estilo grok-build): cambio de contrato
  en `Reversibility`, no un parche. MVP solo AGREGA encierro, nunca
  relaja el prompt. Anotado como la evolución natural.
- **post_edit_check / verification gate / MCP subprocess**: son
  infraestructura de braze (necesitan `~/.cargo`, `target/`, red para
  deps), no comandos del modelo. Fuera del bwrap por-tool. Si algún día
  entran, es con una política mucho más laxa y distinta.

## Construcción del argv (orden fijo, del blueprint)

```
bwrap
  --unshare-all --new-session --die-with-parent
  [--share-net]                        # solo si bwrap_allow_network
  --ro-bind / /
  --dev /dev --proc /proc --tmpfs /tmp
  <mounts ordenados por longitud de destino ascendente>
  -- <program> <args...>
```

Los `<mounts>` se acumulan en este orden (el sort estable desempata por
inserción, así los que entran al final ganan a igual longitud):

1. workspace: `--bind <ws> <ws>` (r/w).
2. governance: `--ro-bind <ws>/.gitignore …`, `--ro-bind <ws>/.git …`
   (salvo `.git` para comandos git, que va `--bind`).
3. secretos: `--bind <maskfile> <ruta-secreto>` (mask `chmod 000`).

`--ro-bind` (sin `-try`) para governance y secretos — su fuente existe
(la pre-creamos / la encontró `find`), y un `--ro-bind` fallido aborta
bwrap entero. El workspace usa `--bind` directo (ya validado que existe
por el allowlist).

## Composición con lo existente

- **Debajo del PermissionGuard**: `invoke_shell_exec` (`provider.rs:260`)
  ya hace `guard.check()` ANTES de spawnear. El bwrap se interpone
  después, en el spawn — no cambia ninguna decisión de permiso, solo
  encierra la ejecución que el guard ya autorizó.
- **Junto al paquete in-process**: si ambos flags están on, el proceso
  braze corre bajo Landlock/seccomp Y sus `shell_exec` además bajo
  bwrap. bwrap unprivileged requiere `PR_SET_NO_NEW_PRIVS=1`, que
  `harden_process()` ya setea — compatible. El seccomp heredado no
  deniega `clone`/`unshare`/`pivot_root` (lo que bwrap usa), verificado
  contra `HARDENED_DENIED_SYSCALLS`.

## Config (el knob nuevo, 5 sitios + el bug de paso)

`enable_bwrap_tool_sandbox: bool` (default `false`), por el patrón de 5
sitios: `Config` (campo+doc), `Default`, `ConfigOverrides`, `from_env`
(`BRAZE_ENABLE_BWRAP_TOOL_SANDBOX`), `apply_overrides`, **y**
`KNOWN_OVERRIDE_KEYS` en `file.rs`. De paso se agregan a esa lista
`enable_landlock_write_sandbox` y `disable_agents_md`, que hoy FALTAN
(un config file que los use dispara "unrecognized key; ignored" aunque
el valor se aplique — bug latente encontrado en el mapeo). Más
`bwrap_allow_network: bool` (default `false`) por los mismos sitios.

## Verificación

- **Unit**: el builder de argv es función pura (`build_bwrap_argv(spec)
  -> Vec<String>`), como `build_syscall_filter` hoy. Tests: orden de la
  cabecera, workspace r/w, `.git` ro salvo git, secreto → mask bind,
  orden por longitud (padre antes que hijo), sin red por defecto.
- **En vivo** (la convención del proyecto): con bwrap real, un comando
  que (a) intenta LEER un `.env` plantado en el workspace → EACCES; (b)
  intenta ESCRIBIR fuera del workspace (`/etc`, `$HOME`) → read-only FS;
  (c) escribe DENTRO del workspace → funciona. Las tres aserciones sobre
  el binario real, no mocks.

## Riesgos anotados

- **bwrap unprivileged necesita user namespaces habilitados**
  (`kernel.unprivileged_userns_clone=1` / `max_user_namespaces>0`).
  Algunas distros endurecidas los desactivan → bwrap falla al arrancar.
  Se detecta en la verificación en vivo; el degradado con warning cubre
  el caso (el comando corre sin encierro, no se cuelga).
- **Mask file `chmod 000` no protege si el proceso corre como root
  dentro del userns** (gotcha #1). braze no mapea a root; el userns por
  defecto de `--unshare-all` mantiene el uid real. Aceptado; si se
  quisiera blindar, `--ro-bind /dev/null` para secretos también.
- **Symlink colgante** en el walk de secretos: `find -type f` no lo
  sigue, no entra — más seguro que el `statSync` de gemini-cli (gotcha
  #3).
- **Nombres de archivo no-UTF8**: `find -print0` + split por NUL sobre
  bytes, no sobre String (gotcha #4).
