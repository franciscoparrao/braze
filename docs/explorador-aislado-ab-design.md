# Diseño pre-registrado: A/B del explorador de contexto aislado (I.7)

Fecha: 2026-07-11
Estado: **DISEÑO — nada implementado.** Este documento es el gate que v6
puso antes de cualquier trabajo de subagent isolation ("diseñar A/B
primero") y el último ítem abierto del estudio consolidado
(`docs/harness-engineering-hooks-skills-2026-07-10.md` § I.7). Sigue la
misma disciplina de pre-registro del planner (PLAN.md § split), que
acaba de pagar: el criterio escrito antes del sweep es lo que convirtió
un resultado feo en diagnóstico accionable
(`docs/sweep-planner-ab-2026-07-11.md`).

## La hipótesis

La palanca NO es capacidad (eso ya lo cubre el lead) sino **aislamiento
de contexto**: delegar la exploración amplia ("¿en cuál de estos
archivos está X?") a un engine hijo desechable — el MISMO modelo chico —
que quema su propia ventana leyendo N archivos y devuelve solo la
conclusión. El colapso ACI mitiga las observaciones viejas *después* de
que entraron; el explorador evita que entren siquiera.

Predicción si la hipótesis es cierta: en tareas de búsqueda-amplia, el
brazo `+explore` mejora pass rate y/o reduce tokens del turno principal
frente a baseline, CON EL MISMO MODELO en ambos roles — es decir, la
ganancia no puede atribuirse a capacidad agregada, solo al aislamiento.

Predicción alternativa (la que mataría la palanca): el modelo chico no
sabe *formular* la pregunta de exploración (delegar requiere abstraer),
y el brazo `+explore` empeora o no mueve — el análogo del hallazgo del
A/B de 3 brazos, donde el trigger reactivo no disparaba porque el worker
no reconocía su propio estado.

## Mecanismo mínimo a implementar (solo si se decide correr el A/B)

Tool harness-owned `explore(question)` — mismo patrón de intercepción
que `search_tools`/`task_add` (sin registry, sin permission guard
propio):

1. Instancia un Engine hijo: **mismo backend/modelo** que el executor,
   solo tools read-only (`read_file`, `grep`, `glob`), `max_turn_iterations`
   bajo (6), `SessionStore` en memoria/descartable (su historia NO se
   persiste en el rollout log del padre — solo la tool call y su
   resultado, como cualquier observación).
2. System prompt del hijo: el del proyecto + "Answer ONLY the question,
   in at most 3 sentences. Do not propose actions."
3. El texto final del hijo vuelve como tool result del padre. Si el hijo
   no converge: tool result de error recuperable ("exploration failed;
   explore yourself with read_file/grep").
4. Eventos: `ExplorationDelegated { question, child_rounds, child_tokens }`
   (audit-only) para que el bench cuente costo real — los tokens del
   hijo SE SUMAN al costo del turno en las métricas (misma regla que
   best-of-n: cada llamada real se contabiliza).
5. Gate: `+ablate:explore` (habilitador off-by-default, mismo precedente
   documentado que `task-list`) — nunca en el inventario sin opt-in.

## Suite nueva: `suites/exploration.toml`

El suite default no ejercita búsqueda-amplia (sus tareas apuntan al
archivo por nombre). ~6 tareas nuevas, cada una con 12-15 archivos de
setup donde la respuesta vive en UNO y el resto es ruido plausible:

1. `find_config_value` — 15 archivos de config similares; "¿qué valor
   tiene `timeout` en el servicio de pagos?" (respuesta en 1).
2. `find_function_definition` — 12 archivos .rs sintéticos; "¿en qué
   archivo se define `parse_header`?"
3. `find_error_source` — 14 logs; "¿qué servicio reportó el error de
   conexión?"
4. `count_matches_across_files` — "¿cuántos archivos mencionan X?"
   (obliga a barrer todo, no basta encontrar uno).
5. `find_then_edit` — encontrar el archivo correcto entre 12 Y editarlo
   (mide si la conclusión del explorador es *accionable*, no solo
   correcta).
6. `question_answerable_without_exploring` — control negativo: la
   respuesta está en el prompt; un brazo que delega acá está delegando
   compulsivamente (el análogo de `no_tool` para esta palanca).

Asserts: `expect_text_contains`/`expect_file_contains` +
`expect_max_tokens` en las tareas 1-3 (el punto es el ahorro de ventana
del turno principal — un pass que quema 30K tokens no valida la
palanca). Los tokens del hijo cuentan (evento del punto 4 del mecanismo).

## Brazos (2 executors × 4 brazos × 6 tareas × 5 reps = 240 corridas)

Sobre qwen2.5:3b (el caso objetivo) y qwen2.5:7b (¿la ganancia decae con
escala, como el lead?):

| Brazo | Qué aísla |
|---|---|
| baseline | el modelo explora inline, colapso ACI on (default) |
| `+ablate:explore` | la palanca completa |
| `+ablate:explore;no-prune` | ¿el aislamiento SUBSUME al colapso? (si explore sin prune ≈ explore con prune, la ventana del padre ya casi no recibe observaciones grandes) |
| `+ablate:no-prune` | referencia: cuánto del baseline lo sostiene el colapso |

## Criterio pre-registrado

- **Adoptar (promover a implementación completa + considerar en Fase 2)**
  si `+explore` mejora pass rate en ≥8pp agregado sobre baseline en 3b
  (fuera del ruido con n=30/brazo en las 5 tareas no-control), O si
  iguala pass rate con ≥30% menos tokens de turno principal (medido por
  `expect_max_tokens`/tokens totales incluyendo al hijo).
- **Rechazar (cerrar I.7, no implementar isolation en Fase 2 por esta
  vía)** si `+explore` no mueve ninguna de las dos métricas, o si el
  control negativo (tarea 6) muestra delegación compulsiva (>2/5 reps
  delegando lo que estaba en el prompt).
- **Iterar UNA vez** (misma regla que el planner) solo si el modo de
  falla dominante es identificable y atacable (p.ej. el hijo converge
  pero el padre ignora su respuesta → problema de render del tool
  result, no de la palanca). Pre-registrado ahora para no re-litigar
  después.

## Riesgos anotados

- **Doble inferencia**: el hijo cuesta rondas reales; en Ollama
  serializado el wall time puede doblar. El A/B mide tokens y wall — si
  la palanca solo "gana" ignorando su costo, el criterio de adopción no
  se cumple por diseño (misma lección que la latencia del lead).
- **El modelo chico como formulador**: la predicción alternativa de
  arriba. Si falla ahí, el resultado sigue siendo publicable como
  negativo (delegación requiere una capacidad que el 3B no tiene — el
  espejo del hallazgo del trigger reactivo).
- **Recursión**: el hijo NO recibe la tool `explore` (profundidad 1 por
  construcción).

## Qué NO es este documento

No es un compromiso de implementación. El costo estimado del mecanismo +
suite es M (un día de trabajo); correrlo son ~1-2h de Nitro. La decisión
de gastarlo queda para cuando la cola de sweeps del paper esté vacía —
este diseño solo garantiza que, si se corre, se corre pre-registrado.
