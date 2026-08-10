# El subagente `editor` (SWE-Edit) — diseño

Fecha: 2026-08-10
Ítem: v9 Paquete 4 #17 (`docs/AUDITORIA-2026-07-v9.md`), estilo SWE-Edit
(arXiv 2604.26102). Único variante de multi-agente con evidencia; **NO**
orquestación genérica.
Método: dos Plan agents en paralelo (mínimo vs completitud), como el
Grupo 2 de seguridad. Este doc es la síntesis.

## La idea, y por qué el trabajo nuevo es solo la mitad "Editor"

braze **ya tiene la mitad Viewer**: el subagente `explore`
(`exploration.rs`) es un mini-loop aislado read-only que devuelve solo la
conclusión, manteniendo el contenido de los archivos fuera del contexto
del padre. Lo nuevo es el **Editor**: el padre delega una edición
autocontenida sobre UN archivo a un hijo aislado que corre el loop
read→edit→verify y devuelve solo un resumen de estado, manteniendo el
churn (ediciones fallidas, contenido del archivo, salida del `cargo
check`) fuera del contexto del padre.

## La costura, verificada en código

- **El guard viaja dentro del provider.** `LocalToolsProvider::invoke`
  corre `guard.check()` en los brazos de `edit_file`/`write_file`. Un
  hijo que despacha vía `self.tools.dispatch(call)` —igual que
  `run_exploration`— hereda el guard, la resolución de workdir y el
  sandbox Landlock (mismo proceso). **Cero código nuevo.**
- **El post-edit check también viaja dentro del provider**: anexa el
  bloque `[post-edit check] ... COMPILES` / `(exit N)` al resultado de
  la edición. El hijo lo ve en su propio tool result e itera; ese churn
  se queda en el hijo. **Cero código nuevo, y ES la ganancia.**
- **El interlock L-10 vive en el engine**, no en el provider
  (`TurnDispatchState::edit_failures_by_path`, enforced en
  `dispatch_tool_calls`). Un hijo que despacha directo lo **evita** —
  ésta es la única lógica nueva de peso.

## Decisiones (síntesis de los dos diseños)

### Tool y loop
- `editor(path, instruction)`, ambos requeridos. `path` singular y
  explícito: (a) el hijo siembra su primer `read_file` sin buscar; (b)
  acota a un archivo (un `paths[]` sería el primer paso a refactor
  repo-wide = orquestación, excluido); (c) el evento de auditoría y el
  bookkeeping del padre necesitan el target sin parsear prosa.
- Módulo `editor.rs` espejo de `exploration.rs`. Cap
  `MAX_EDITOR_CHILD_ROUNDS = 6` (read→edit→ver verdict→un fix→re-check→
  un fix más; entre el 4 del diseño mínimo y el 8 del de completitud).
- Tools del hijo: `read_file`, `edit_file`, `write_file`. NO `grep`/
  `glob` (el path se da), NO `shell_exec` (verify = post-edit check, que
  ya viaja gratis; darle shell lo vuelve un worker genérico), NO
  `editor`/`explore` (profundidad 1 por construcción).

### La pieza nueva: interlock L-10 propio del hijo
El hijo mantiene su propio `edit_failures_by_path` (fresco por
delegación) reusando la constante `EDIT_FAILURE_WRITE_INTERLOCK_THRESHOLD`
y el mensaje de bloqueo del padre. NO se comparte el `TurnDispatchState`
del padre: es `&mut` inalcanzable desde `run_editor`, mezclaría los
fallos del padre con los del hijo, y el aislamiento es justo el punto.
Cierra la clase de daño de L-10 (el modelo que no puede reproducir un
carácter cae a reescribir el archivo entero y lo corrompe) **dentro** del
loop aislado, donde sería aún menos observable.

### `EditorOutcome` estructurado (la diferencia clave con `ExplorationOutcome`)
explore solo necesitaba `content`/`is_error` porque nunca muta. El editor
muta fuera del dispatch del padre, así que el padre necesita saber el
estado del workspace SIN releer (releer derrota el aislamiento):

```
struct EditorOutcome {
    content: String,              // resumen corto para el modelo padre
    is_error: bool,
    landed: bool,                 // ¿alguna edición tuvo éxito? — GROUND TRUTH del dispatch
    compiles: CompileStatus,      // Pass | Fail | Unknown, derivado del [post-edit check]
    child_rounds, child_input_tokens, child_output_tokens,
}
```

- `landed` se deriva de los resultados reales del dispatch (no del
  auto-reporte del hijo): true si algún `edit_file`/`write_file`
  devolvió `is_error=false`. Es lo que maneja el bookkeeping del padre.
- `compiles` se deriva del marker `[post-edit check]` en el resultado de
  la última edición exitosa: `COMPILES` → Pass, `(exit ` → Fail, sin
  marker → Unknown. Verdict, no churn.
- Regla dura: **nunca `is_error=false` con un "listo" vago.** Si
  `landed && compiles==Fail`, o si el hijo no converge con `landed`, el
  outcome es `is_error=true` y el content dice "el archivo quedó
  modificado pero no compila / a medias — relee antes de seguir". Un
  resumen limpio sobre un árbol roto es el envenenamiento del próximo
  turno que las notas de `turn.rs` ya documentan.

### Bookkeeping del padre tras `run_editor` (lo que explore NO hace)
En el brazo de interceptación, después de que el hijo vuelve:
1. `turn_attempted_edit = true` siempre que se delegó.
2. `turn_did_edit = true` **sii** `outcome.landed`.
3. `seen_calls.clear()` **sii** `outcome.landed` (regla F6: una mutación
   exitosa invalida la caché de repetición).

Sin esto, la lógica de salvage de ronda vacía y las notas de
convergencia de `turn.rs` (que ramifican en esos flags) malinterpretan un
turno cuya única mutación pasó dentro del hijo.

### Auditoría, config, bench (espejo de explore)
- `AgentEvent::EditorDelegated { path, instruction, landed, compiles,
  child_rounds, child_tokens }` — extiende la forma de
  `ExplorationDelegated` con los dos campos de ground-truth que el A/B
  necesita. Audit-only, nunca renderizado al modelo.
- Doble entrada como explore: `Usage { stop_reason: "editor_child" }`
  agregado (para que toda contabilidad de tokens lo cuente gratis) + el
  evento. Sin doble conteo: `turn_total_tokens` solo suma el usage de
  rondas del padre, no los eventos `Usage` del dispatch — igual que
  explore; el cap real del costo del hijo es `MAX_EDITOR_CHILD_ROUNDS`.
- `Config::enable_editor` (default false) + override + `+ablate:editor`
  en braze-bench, junto a `enable_exploration`.

## Deferidos / limitaciones aceptadas (disciplina de alcance)

- **Race de mismo-archivo intra-ronda** (el diseño de completitud lo
  marcó): si el modelo emite `[edit_file(X), editor(X)]` en una ronda,
  el `edit_file(X)` de fondo y el `editor(X)` inline pueden escribir X a
  la vez. **Aceptado como limitación del lever experimental**, con el
  mismo criterio que J-20 (symlinks) fue ratificado MVP: (a) el A/B mide
  aislamiento de contexto, no seguridad de edición concurrente; (b)
  `write_file` es reemplazo atómico y `edit_file` es match-based, así que
  el peor caso es "una de dos ediciones se pierde", NO un archivo
  corrupto/torn. Si se promueve a default, drenar las mutaciones de fondo
  pendientes sobre `path` antes de correr el hijo es el fix.
- **Recuperación tras crash a mitad del hijo**: el `AssistantToolCall`
  del editor se persiste antes de correr el hijo; si braze crashea a
  mitad, la reparación de huérfanos N-4 sintetiza un resultado "falló" y
  el disco puede tener ediciones parciales. Es seguro: las ediciones son
  atómicas y match-based, así que reintentar re-aplica limpio o pega
  `old_string not found` (ya en su lugar), casos que L-10 y las notas de
  convergencia ya manejan. No se hace journaling transaccional del hijo
  (eso es gestión de estado sub-sesión = orquestación).
- Multi-archivo, sub-delegación, `shell_exec`/test runner en el hijo,
  best-of-n del hijo, protocolo de ida y vuelta padre↔hijo, streaming/
  persistir el transcript del hijo. Todo excluido: es orquestación
  genérica, justo lo que la auditoría prohíbe.

## Pasos incrementales (cada uno testeable, cierra en vivo)
1. `editor.rs`: constantes, `editor_tool_stub()`, `EditorOutcome`,
   `CompileStatus`. Tests: stub pide `["path","instruction"]`; allowlist
   excluye `editor` (profundidad 1) y `shell_exec`.
2. `AgentEvent::EditorDelegated` + test de serde round-trip.
3. Config + engine (`enable_editor`, `with_editor_enabled`, stub en
   `turn.rs` sii lever). Test: stub presente sii lever on.
4. `run_editor` en dispatch.rs (port de `run_exploration` + interlock
   propio + derivación de `landed`/`compiles`). Tests con provider de
   juguete: happy path (landed, Pass); dos edits fallidos → write_file
   bloqueado por el interlock del hijo; no-convergencia con edición
   previa → is_error + nota "a medias".
5. Interceptación en dispatch.rs + bookkeeping del padre (turn flags,
   seen_calls, doble entrada). Test: una delegación produce un `Usage`
   agregado + un `EditorDelegated`, el transcript del hijo no llega al
   log del padre, y `turn_did_edit` refleja `landed`.
6. Test de integración con `LocalToolsProvider` real sobre temp dir +
   allow-guard: delegar una edición, el archivo cambia en disco y el
   resumen refleja el verdict del post-edit check (prueba que guard +
   check viajan gratis).
7. **En vivo** (compilar ≠ funcionar): rollout real contra un modelo
   local con `+ablate:editor` sobre una tarea find-then-edit — el modelo
   llama `editor`, el hijo converge, el archivo queda bien, el contexto
   del padre no ve el churn, y una instrucción imposible da un
   `landed=false` honesto en vez de un archivo corrupto.
