# Cola de ejercicios de auto-mejora — braze

Tareas puntuales para correr con `braze chat --tui --supervised` sobre el propio código de `braze`, en un `git worktree` aislado — ver `docs/self-improvement-guide.html` para el flujo completo. Cada ejercicio trae su origen (de qué hallazgo o ítem de backlog sale), el prompt exacto para copiar y pegar, y los criterios de aceptación que el humano revisa antes de mergear.

No todos los hallazgos de `docs/usability-log-*.md` se convierten en un ejercicio limpio — algunos (U-2, U-3, U-4) son defectos de contenido/comportamiento del *modelo*, no bugs del harness: no hay una línea de código en `braze` que "arregle" que un modelo chico alucine `$(date)` sin resolver. Quedan anotados abajo como no-ejercicios, con la razón.

## Ejercicios

### SI-1 — Ampliar la lista seguro de comandos de shell (origen: U-5)

**Dificultad**: fácil. **Archivos**: `crates/braze-permissions/src/classifier.rs`.

`DefaultClassifier::is_safe_shell_command` no incluye `lscpu`/`lsmem`/`lshw` (ver hallazgo U-5, `docs/usability-log-2026-07-07.md`) — comandos de solo lectura/introspección de hardware, sin argumentos de ruta, que hoy quedan clasificados `Irreversible` y piden confirmación individual pese a no tener superficie de ataque.

**Prompt (copiar y pegar)**:
```
Lee crates/braze-permissions/src/classifier.rs y entiende cómo is_safe_shell_command
clasifica comandos como seguros (ls, pwd, wc, whoami, date, which, true, false).
Agrega lscpu, lsmem y lshw a esa misma lista de comandos siempre-seguros (no llevan
argumentos de ruta que validar, a diferencia de cat/head/tail/grep). Agrega tests
de regresión siguiendo el patrón de los tests existentes de esa función. Corre
cargo test -p braze-permissions al final y confirma que todo pasa.
```

**Criterios de aceptación**:
- Solo toca `classifier.rs` (y su módulo de tests).
- `lshw` con flags que sí mutan (si acaso existieran) no debería colarse — verificar que el diff solo agrega los 3 nombres a la rama de comandos sin argumentos, no a la rama `cat`/`head`/`tail` (que sí valida rutas).
- `cargo test -p braze-permissions` pasa con los tests nuevos incluidos.
- `cargo clippy -p braze-permissions --all-targets -- -D warnings` limpio.

**Estado**: **Resuelto** 2026-07-07. `openrouter:deepseek/deepseek-v4-flash` falló en 4 intentos sobre la misma sesión (loop de no-convergencia, y dos alucinaciones consecutivas incluso citando "texto literal" con procedencia falsa — U-6, U-7, U-8). Un quinto intento, cambiando de modelo a mitad de la misma sesión a `openrouter:anthropic/claude-sonnet-5` vía `/model`, sí produjo un diff correcto — verificado de forma independiente (no confiando en el resumen del modelo, que seguía siendo inexacto — U-9): toca solo `classifier.rs`, agrega `lscpu\|lsmem\|lshw` a la rama correcta, `cargo test -p braze-permissions` 61/61, `cargo clippy -p braze-permissions --all-targets -- -D warnings` limpio. Ver `docs/usability-log-2026-07-07-si1.md` (hallazgos U-6 a U-9). Diff en `../braze-self-improve-si1` (rama `self-improve/si-1`), pendiente de push/PR.

---

### SI-2 — Sintaxis de spec `+lead:` en `braze-bench` (origen: backlog, A/B del `EscalatingBackend`)

**Dificultad**: media. **Archivos**: `crates/braze-bench/src/backend_spec.rs`, `crates/braze-bench/src/runner.rs`.

`braze chat --lead <backend>` ya existe en producción, pero `braze-bench` no tiene forma de medir el A/B (baseline vs. con lead) en un mismo sweep — a diferencia de `+plan:`, que sí tiene su propia sintaxis de spec.

**Prompt (copiar y pegar)**:
```
Lee crates/braze-bench/src/backend_spec.rs completo, en particular cómo BackendSpec::parse
maneja el sufijo "+plan:<spec>" (executor + planner) y cómo runner.rs llama
spec.build_planner(...) y engine.with_planner(...). Agrega un sufijo análogo
"+lead:<spec>" con la misma gramática (mismo split en el primer ':', mismo manejo
de errores para specs vacíos o duplicados), un método BackendSpec::build_lead
equivalente a build_planner, y wireálo en runner.rs con
braze_model::EscalatingBackend::new(lead, executor) antes de construir el Engine
—mismo patrón que ya usa braze-cli/src/main.rs para --lead. display_name debe
reflejar el sufijo +lead: igual que ya hace con +plan:. Agrega tests siguiendo el
patrón de los tests existentes de +plan: en el mismo archivo. Corre
cargo test -p braze-bench al final.
```

**Criterios de aceptación**:
- `+plan:` y `+lead:` deben poder combinarse en el mismo spec sin interferirse (mismo patrón que ya prueban los tests de `+ablate:` combinado con `+plan:`).
- `display_name` muestra ambos sufijos cuando ambos están presentes.
- Tests nuevos que cubran: parseo solo, combinado con `+plan:`, error en spec vacío/duplicado.
- `cargo test -p braze-bench` y `cargo clippy -p braze-bench --all-targets -- -D warnings` limpios.
- Este es el prerequisito que falta para correr el A/B real del `EscalatingBackend` mencionado en PLAN.md — no corre el A/B en sí, solo lo habilita.

---

### SI-3 — Iteración pre-registrada del planner (origen: backlog, PLAN.md § "Split planificador/ejecutor")

**Dificultad**: difícil / abierta a criterio. **Archivos**: `crates/braze-engine/src/engine.rs` (`attempt_planning_round` y alrededores).

Ítem "opcional" ya anotado en PLAN.md: descartar planes de un solo paso (un plan que no dice más que "haz X" no aporta nada sobre no tener plan) y/o cambiar el rol con el que se renderiza el plan en el historial (hoy como texto del *assistant* — ver `history.rs`). El resultado esperado no está decidido de antemano — si ninguna de las dos variantes mueve la aguja en `multi_step`/`error_recovery` del bench, la conclusión legítima es remover la característica, no forzar una mejora que no existe.

**Prompt (copiar y pegar)**:
```
Lee PLAN.md, busca la sección "Split planificador/ejecutor" y el ítem pendiente
sobre iteración pre-registrada del planner. Lee crates/braze-engine/src/engine.rs
(attempt_planning_round) y cómo PlanCreated se renderiza en history.rs. Implementa
UNA variante primero: descartar (no persistir PlanCreated) cuando el plan generado
tiene una sola línea o menos de N palabras (elegí un umbral razonable y explicalo
en un comentario). Agrega un test de regresión que confirme que un plan de una
sola línea se descarta y uno de varias líneas se persiste igual que antes. No
toques el rol de renderizado todavía — eso es una segunda iteración separada.
```

**Criterios de aceptación**:
- Cambio acotado a "descartar planes triviales", no un rediseño del planner.
- El umbral elegido está justificado en un comentario (no un número mágico sin explicación).
- Un plan "normal" (varias líneas / con pasos) sigue persistiéndose exactamente igual que hoy.
- Este ejercicio es el más abierto de los tres — si el diff que produce braze no te convence, es un candidato perfecto para **denegar en el momento** vía `--supervised`, no solo para revisar al final.

## No-ejercicios (hallazgos que no se resuelven con código)

- **U-2** (`$(date)` sin resolver): es el modelo alucinando sintaxis de shell dentro del contenido que escribe, no un bug de `braze`. Lo más cercano a un "fix" sería reforzar el system prompt por defecto (`braze-config::default_system_prompt`) con una línea explícita tipo "nunca emitas sintaxis de shell sin resolver (`$(...)`, backticks) en el contenido de un archivo — si necesitás la fecha/hora actual, usá `shell_exec`". Si se quiere probar esa vía, es un ejercicio de prompt-engineering, no de lógica — candidato a un SI-4 si se decide intentarlo.
- **U-3** (posible disco duplicado): error de interpretación de datos por parte del modelo (confundir `/dev/ng0n1` con un segundo disco), no hay lógica de `braze` que lo cause ni lo prevenga.
- **U-4** (omite núcleos/hilos): el modelo decidió qué incluir en el resumen; no hay una tool o instrucción de `braze` que fuerce cierto contenido en el reporte final.
