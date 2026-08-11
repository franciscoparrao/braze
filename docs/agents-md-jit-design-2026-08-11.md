# Diseño: carga JIT de AGENTS.md por subdirectorio

Fecha: 2026-08-11
Origen: survey de referencia (`docs/reference-agents-survey-2026-08-10.md`
§ gemini-cli, `memoryDiscovery.ts loadJitSubdirectoryMemory`). Es la
continuación anunciada del TODO del module doc de
`crates/braze-config/src/context_file.rs:8-13` ("solo AGENTS.md en la
raíz del cwd... sin jerarquías en esta primera pasada").

## El hueco

braze carga hoy UN solo `AGENTS.md` — el de la raíz del cwd — al
arranque, baked en el system prompt. Un monorepo con instrucciones
por-subsistema (`crates/foo/AGENTS.md`, `frontend/AGENTS.md`) las pierde:
o las metes todas en el raíz (infla el prompt, justo lo que un modelo
chico no tiene presupuesto para tragar) o el modelo no las ve nunca.

La carga JIT resuelve las dos: cuando un tool toca un archivo en un
subdirectorio, se descubre el `AGENTS.md` más cercano subiendo por el
árbol y se inyecta **solo entonces**. El prompt base queda chico; el
contexto por-subsistema aparece exactamente cuando el modelo trabaja en
ese subsistema.

## Mecanismo

- **Techo del walk-up**: `braze_memory::resolve_project_root(cwd)` (ya
  existe — sube buscando `.git`, cae a cwd si no hay repo). El engine lo
  recibe por un builder `with_project_root(root)`; su presencia (`Some`)
  ES el gate de la feature, igual que `planner`/`skill_registry` se
  gatean por `Option`. La CLI lo setea salvo `disable_agents_md`.
- **Disparo**: en `dispatch.rs:739-766`, donde el engine YA extrae el
  `path` de cada `read_file`/`edit_file`/`write_file` (para la
  contabilidad de relectura). Se resuelve el path a absoluto, se camina
  desde su directorio hacia arriba hasta el techo, y se toma el primer
  `AGENTS.md` que no esté ya cargado. Descubierto en la ronda N →
  inyectado en el request de la ronda N+1 (el system prompt se
  reconstruye por request).
- **Dedup + anti-raíz**: `loaded_agents_md: Mutex<HashSet<PathBuf>>`
  (paths canónicos), **sembrado con `cwd/AGENTS.md`** para que el raíz
  —ya en el system prompt— nunca se re-inyecte. Session-scoped: una vez
  descubierto sigue vigente el resto de la sesión (NO se resetea por
  turno, a diferencia de los harness notes; mismo ámbito que los skills).
- **Inyección**: addendum al system prompt, modelado sobre skills
  (`system_prompt_with_skills`). Un AGENTS.md de subdir es "instrucción
  del proyecto" — misma naturaleza que el raíz que ya vive en el prompt.
- **Tope**: reutiliza el cap de 8000 bytes por archivo de
  `context_file.rs`. Además un tope de CUENTA por sesión
  (`AGENTS_MD_JIT_MAX_FILES = 8`): una sesión que barre medio repo no
  puede inflar el prompt sin límite — el descubrimiento #9 en adelante se
  salta con un evento. (El prompt chico es el punto de la feature;
  romperlo la anularía.)
- **Auditoría + `--resume`**: evento nuevo `AgentEvent::AgentsMdLoaded
  { path }` (audit-only, cuerpo request-scoped como `SkillLoaded`) y un
  `rehydrate_agents_md_from_log` espejo del de skills — al resumir, se
  re-siembra el set y se recargan los bodies desde disco. Un archivo que
  desapareció degrada a warn, no aborta.

## Confianza (importa, es contenido que entra al prompt)

Un `AGENTS.md` de subdir tiene el MISMO nivel de confianza que el raíz:
es contenido versionado del repo, del proyecto, no del modelo ni de un
tercero. El walk-up está acotado por el techo (`resolve_project_root`),
así que nunca sube por encima del proyecto a un `AGENTS.md` de `$HOME` o
del sistema — solo archivos DENTRO del árbol del repo, bajo el mismo
paraguas de confianza que el `.git` que define ese árbol. El path que
dispara el walk ya pasó el `PermissionGuard` (el tool se ejecutó), así
que no es una ruta que el modelo no tuviera derecho a tocar.

## Alcance MVP y lo diferido

ENTRA: descubrimiento por `read_file`/`edit_file`/`write_file`, walk-up
acotado, dedup, addendum, evento + rehidratación, doble tope.

DIFERIDO:
- **`@import` con guardas de ciclo/profundidad** (el nice-to-have del
  survey): un AGENTS.md que incluye otro. Fuera del MVP — el estándar
  interoperable es el archivo plano.
- **Bench**: solo-CLI por ahora. El runner pasa `agents_md=None`
  deliberadamente (sandbox hermético) y no cablea `project_root`. Llevar
  el JIT al bench pide tres cables (`with_project_root(sandbox)`, un flag
  `+ablate:agents-md-jit`, y tareas que siembren `subdir/AGENTS.md` vía
  `setup_files`) — se agrega con el molde de `explore`/`editor` cuando su
  A/B lo amerite.
- **Descubrimiento por `shell_exec` cwd**: el cwd del shell es el
  workdir, ya cubierto por el raíz. No aporta.

## Verificación

- **Unit**: `find_nearest_agents_md(from, ceiling)` — encuentra el más
  cercano, se detiene en el techo, ignora por encima; el seed del raíz
  hace que un touch en el raíz no re-descubra nada.
- **Integración** (engine): una sesión scripteada con un
  `subdir/AGENTS.md` plantado; un `read_file subdir/x.rs` en la ronda 1
  hace que el request de la ronda 2 contenga el addendum del subdir, y
  que se persista `AgentsMdLoaded`. Un segundo touch del mismo subdir NO
  re-inyecta (dedup). El raíz nunca aparece dos veces. Más el no-op sin
  lever. (2 tests, verdes.)
- **En vivo VERIFICADO (2026-08-11)** con el binario real
  (`braze run`, openrouter:deepseek-v4-flash): un proyecto con
  `crates/foo/AGENTS.md` conteniendo "termina tu respuesta con
  XYZZY-FOO-42"; un `read_file crates/foo/lib.rs` disparó el
  descubrimiento (log: "AGENTS.md de subdirectorio cargado JIT
  path=…/crates/foo/AGENTS.md") y **la respuesta del modelo terminó con
  XYZZY-FOO-42** — el código vivía SOLO en el AGENTS.md del subdir, así
  que el modelo no solo lo recibió sino que obedeció una instrucción que
  ninguna otra fuente contenía. Prueba end-to-end del valor de la
  feature, no solo del mecanismo.
