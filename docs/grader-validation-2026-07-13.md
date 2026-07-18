# Validación del grader automático contra calificación humana

Fecha: 2026-07-13
Origen: `/paper-review-emse` sobre `paper/main.tex` (review completa en
`~/vault/journals/emse/reviews-generated/2026-07-13_16-34_braze-harness-paper.md`,
checklist en `docs/emse-review-2026-07-13-checklist.md`, Issue 4). El
paper ya documenta un bug real en un assert anterior (texto laxo que
aceptaba narración como respuesta válida, corregido antes de
`search-tools-ab-n15`) pero no reportaba ninguna validación humana
sistemática del grader actual contra una muestra de transcripciones.
Este documento cierra ese hueco.

## Prerrequisito de infraestructura

`BRAZE_BENCH_KEEP_SESSIONS` pasó de parche local no commiteado
(mencionado en `docs/sweep-search-tools-ab-n15-2026-07-12.md:116`) a
flag real (`crates/braze-bench/src/preserve.rs` +
`crates/braze-bench/src/runner.rs` +
`crates/braze-bench/src/main.rs`). Activado, copia el sandbox final y
la transcripción JSONL de cada run a
`braze-bench-preserved-sessions/<backend>/<task>/rep<N>/` ANTES del
borrado incondicional que corre en todo sweep — comportamiento
default (`BRAZE_BENCH_KEEP_SESSIONS` sin definir) queda byte-idéntico
al de antes. 141 tests verdes (3 nuevos en `preserve.rs`), clippy
`-D warnings` limpio en todo el workspace.

## Metodología

Muestra estratificada sobre los dos sweeps más citados del paper (los
que cargan los números más citados del abstract):

1. **Scale-curve** (`\S\ref{sec:curve}`): re-corridos completos
   ($n{=}19$ tasks $\times$ 1 rep) de los dos arms más centrales —
   `llama3.2:1b` baseline y `llama3.2:1b+lead:gemma4:e4b` — 38 runs.
   JSON: `docs/audit-transcripts-scale-curve-2026-07-13.json`.
2. **Tool-deferral** (`\S\ref{sec:searchtools}`): re-corridos de
   `qwen2.5:3b` deferred vs.\ full-inventory
   (`+ablate:tool-search-threshold=1000000`), 6 tasks $\times$ 2 reps
   $\times$ 2 arms — 24 runs. JSON:
   `docs/audit-transcripts-tool-deferral-2026-07-13.json`.

Total: **62 transcripciones preservadas y calificadas a mano contra el
veredicto del assert automático** — por encima del rango 30-50
originalmente planeado (el diseño de la muestra, 2 arms completos por
sweep, terminó dando ese tamaño de forma natural).

No son "semillas equivalentes" a los sweeps originales (son
re-corridas nuevas, sampling no determinístico salvo `--seed`) — el
objetivo es diversidad de tasks/outcomes para auditar el mecanismo del
grader, no reproducir números exactos ya reportados.

Para cada run: se leyó la transcripción completa (mensajes, tool
calls, resultados de tools, texto final del asistente) y el estado
final del sandbox, y se comparó el juicio humano (¿el resultado
observable — texto final + archivos — satisface lo que pide el
prompt?) contra `passed`/`failure_cause` del JSON.

## Resultado

**62/62 (100%) de acuerdo** entre calificación humana y veredicto
automático. Cero falsos positivos (el grader marcando PASS algo que un
humano diría que falló) y cero falsos negativos (FAIL en algo que un
humano diría que pasó) en esta muestra.

Desglose:
- Scale-curve: 38/38 de acuerdo (19 baseline + 19 +lead).
- Tool-deferral: 24/24 de acuerdo (12 deferred + 12 full-inventory).

## Hallazgos cualitativos (no son bugs del grader, pero informan cómo leerlo)

1. **El assert de tool call es estricto sobre identidad de la
   herramienta, no solo sobre el resultado final.** Varias tasks
   `single_tool` (p.ej. `read_file_basic`, `noisy_grep`) especifican
   `expect_tool_call: "<tool>"` — un run que resuelve la tarea
   correctamente vía una herramienta distinta pero equivalente (p.ej.
   `shell_exec` con `wc -l` en vez de `read_file`) falla
   `assertion_tool_call` aunque el texto final sea correcto. Esto es
   una decisión de diseño de la suite (mide selección de herramienta,
   no solo finalización de la tarea), no un bug — pero vale la pena
   documentarlo explícitamente para que un lector no confunda estos
   casos con falsos negativos del grader.
2. **El check AND de texto+archivo atrapó una confabulación real.**
   En `noisy_multi_step` (arm full-inventory, rep0), el modelo llamó
   `write_file` con el contenido literal `"int_a + int_b"` (nunca
   computó la suma) pero su texto final afirmó "the sum...is 30,
   which has been written to suma.txt" — una claim falsa sobre su
   propia acción. El check de texto solo (`expect_text_contains: 30`)
   habría marcado esto PASS por accidente (el string "30" aparece en
   la frase "is 30, which"); el check de archivo
   (`expect_file_contains: {suma.txt: [30]}`) lo atrapó, y el AND
   semantics del assert produjo el FAIL correcto. Evidencia directa de
   que el diseño de asserts duales (texto Y archivo cuando aplica) no
   es redundante — previene exactamente este modo de falla.
3. **Ningún caso de "narración aceptada como respuesta"** — el bug
   documentado que motivó el hardening pre-`search-tools-ab-n15` no
   reapareció en esta muestra (era un assert distinto, ya corregido
   antes de esta validación).

## Limitaciones de esta validación

- Muestra de 62 runs sobre 2 de los ~9 sweeps citados en el paper —
  no cubre `lead-3brazos`, `matriz-4brazos`, `planner-ab`, ni
  `constrained-decoding`. El mecanismo del grader es el mismo código
  compartido (`crates/braze-bench/src/metrics.rs`) en todos los
  sweeps, así que un acuerdo del 100% en estos dos es evidencia de que
  el mecanismo compartido es confiable, pero no audita asserts
  específicos de otras suites (p.ej. `tool-search.toml` vs
  `default.toml` comparten el mismo motor de asserts).
- Un solo calificador humano (yo, sin segundo revisor independiente)
  — no hay medida de inter-rater agreement.
- Las re-corridas no son deterministas frente a los sweeps originales
  citados en el paper (sampling nuevo, no mismas seeds) — la validación
  es del **mecanismo** del grader, no una re-verificación de los
  números puntuales ya publicados.

## Conexión con el paper

Entra a `\S\ref{sec:setup}` como párrafo nuevo "Grader validation",
citando el 62/62 y el hallazgo de la confabulación atrapada por el
check dual texto+archivo (evidencia positiva del diseño del assert,
no solo de su confiabilidad).
