# Registro de ejecución — `gpt-oss:20b` en `braze-playground/log-insights`

**Fecha**: 2026-07-13
**Ejercicio**: sesión manual del usuario contra un proyecto trampa armado a
propósito (`~/proyectos/braze-playground/log-insights/PROMPTS.md`), con 2
bugs reales conocidos, nombres de archivo con errores tipográficos a
propósito, y distractores — 9 categorías espejando las skills del paper
(`no_tool`, `single_tool`, `multi_step`, `error_recovery`,
`distractor_selection`, más bug-hunt/refactor/explicación/verificación
abierta).
**Backend/modelo probado**: `ollama:gpt-oss:20b` (Nitro,
`http://192.168.1.8:11434`) — el modelo local recomendado del proyecto
desde `docs/sweep-capacity-hardware-2026-07-13.md`.
**Modo**: `braze chat --tui --backend ollama --model gpt-oss:20b
--ollama-url http://192.168.1.8:11434`, corrido desde
`braze-playground/log-insights/` (tools acotadas a ese workdir).
**Sesión**: no capturada por id en el transcript pegado.
**Por qué importa**: `gpt-oss:20b` saca 98.9% (94/95) en la suite
scripteada `default.toml` y 6/6 en `g10-weak-skills`
(`docs/sweep-capacity-hardware-2026-07-13.md`,
`docs/sweep-g10-weak-skills-gptoss20b-2026-07-13.json`) — este registro
es la primera vez que se lo somete a sesiones más largas y abiertas,
justo el tipo de cobertura que `\S\ref{sec:threats}` del paper
("Suite coverage") admite no tener: *"we do not know how these findings
transfer to a broader or externally validated suite."*

## Resumen por categoría

| Categoría | Resultado |
|---|---|
| `no_tool` (3 prompts) | Limpio — nunca abrió tools innecesariamente |
| `single_tool` (4 prompts) | Limpio — incluido reportar correctamente el estado real de los tests (2 pasan/4 fallan) |
| `multi_step` (3 prompts) | 2/3 limpios; el 3ro (comparar el promedio del CLI contra el cálculo a mano) sobrevivió un timeout de `shell_exec`, se recuperó leyendo a mano, y **diagnosticó correctamente** el bug real de `times[:-1]` en `stats.py` — el caso más difícil del set, resuelto bien |
| `error_recovery` (4 prompts) | 2/4 limpios (recuperación correcta de nombre de archivo equivocado vía `glob`); 2/4 terminaron en **crash duro** (ver U-21) |
| `distractor_selection` (3 prompts) | Limpio los 3, incluida la distinción sutil "distractor engañoso" (`app_backup.log` para "el log") vs. "fuente legítima" (`app_backup.log` para el incidente de mayo) |
| Bug hunt abierto (2 prompts) | 1/2 limpio; el otro (`average_response_time` no cierra) **agotó las 20 rondas sin converger** (ver U-22) |
| Refactor/feature-add (2 prompts) | 1/2 limpio (`most_frequent_level`); el otro (`--json` flag) terminó en el mismo crash que U-21, pero el fix **igual quedó aplicado** en el repo (ver nota de verificación abajo) |
| Explicar/code review (2 prompts) | Ambos de calidad alta — análisis genuinamente correcto y completo |
| Verificación abierta combinada (1 prompt) | No convergió — timeout + fallo de schema + quedó esperando una confirmación de permiso sin resolver |

**Verificación contra el estado real del repo** (no solo el transcript):
los 6 tests pasan (`python3 -m pytest tests/ -v`), `--json` está
implementado en `cli.py`, `most_frequent_level` existe en `stats.py`,
`data/error_count.txt` tiene el valor correcto (10). El trabajo
terminó bien pese a los tropiezos — los crashes fueron fallas de
*turno*, no pérdida de progreso ya escrito a disco (mismo patrón que
U-10 en `docs/usability-log-2026-07-07-si2.md`).

## Hallazgos que ameritan seguimiento

- **U-21**: dos crashes duros idénticos en las tareas más largas/abiertas
  (`test_stat.py` con nombre trampa, y agregar el flag `--json`):

  ```
  error: model backend error: request to model backend failed: ollama HTTP 500
  Internal Server Error: error parsing tool call: raw='We need to run tests
  again but earlier timed out. Maybe due to long test?...', err=invalid
  character 'W' looking for beginning of value
  ```

  El texto `raw='We need to...'` es el **razonamiento crudo del modelo**
  (prosa, no JSON) apareciendo justo donde se esperaba una tool call —
  y el error viene de **Ollama mismo** (HTTP 500), no de la escalera de
  rescate de `braze` (que nunca llega a intervenir porque el request
  entero falla). Confirmado con una llamada directa a
  `/api/chat`: `gpt-oss:20b` devuelve un campo `message.thinking`
  **separado** de `message.content` —
  ```json
  {"message": {"role": "assistant", "content": "4",
    "thinking": "The user asks \"What is 2+2?\"..."}}
  ```
  — es decir, es un modelo "thinking" en el mismo sentido que
  `qwen3.5-coder` (CLAUDE.md ya documenta ese caveat para Qwen). La
  hipótesis más plausible: en sesiones largas, con contexto acumulado
  cerca de `ollama_num_ctx` (default `8192`,
  `crates/braze-config/src/config.rs:558`), la generación se corta a
  mitad de la transición entre el bloque de razonamiento y el formato
  de tool-call nativo de `gpt-oss` (el formato "harmony" que Ollama
  parsea server-side) — produciendo un tool call sintácticamente roto
  que **Ollama** rechaza con 500 antes de que `braze` vea nada.
  `OllamaBackend` no hace nada con `message.thinking` hoy
  (`ollama_wire.rs::handle_line`, líneas 487-500 — solo lee `content` y
  `tool_calls`), lo cual es correcto para el caso normal, pero no ayuda
  en este caso porque el problema ocurre *antes*, en el parseo del lado
  de Ollama.

- **U-22**: la tarea de bug-hunt abierto ("`average_response_time` no
  cierra, encontrá el bug") agotó las 20 rondas
  (`braze_engine::Engine::MAX_TURN_ITERATIONS`) sin converger — el
  modelo quedó repitiendo `read_file` con argumentos ya usados,
  chocando reiteradamente contra el guardrail "ya llamaste esto con
  estos argumentos" sin pivotar a una estrategia distinta (probar
  `grep`, cambiar offset, etc., lo cual sí hizo eventualmente pero
  demasiado tarde). Mismo patrón de fondo que U-21: ambos son casos
  donde `gpt-oss:20b` se "atasca" en sesiones largas de forma que la
  suite scripteada (tareas cortas, timeout de 180s) no ejercita.

- **Relevancia directa para el paper**: ambos hallazgos son evidencia
  concreta, aunque informal (n=1, sin repetición), de que la brecha que
  `\S\ref{sec:threats}` ya admite como amenaza abstracta ("Suite
  coverage" — no se sabe cómo transfieren los resultados a una suite
  más amplia) es real: un modelo que satura la suite scripteada
  (98.9%, 6/6 en weak-skills) tiene modos de falla reales y repetibles
  en sesiones más largas que esa suite no captura.

## Palancas ya existentes en `braze` que podrían mitigar esto (sin construir nada nuevo)

Investigación de código, no probado todavía:

1. **`ollama_num_ctx`** — ya configurable hoy vía `BRAZE_OLLAMA_NUM_CTX`
   (`crates/braze-config/src/overrides.rs:478`, wireado en
   `braze-cli/src/main.rs:246`). Subir esto de 8192 a algo más generoso
   (16384/32768) es la prueba más barata para la hipótesis de
   truncamiento de U-21 — cero código nuevo, solo una env var distinta
   en la próxima sesión con `gpt-oss:20b`.
2. **`prompt_tools` mode** (`OllamaBackend::with_prompt_tools`,
   `crates/braze-model/src/ollama.rs:155`) — renderiza las tools en el
   system prompt en vez de usar el campo nativo `tools` de Ollama, lo
   que evita por completo el parseo server-side "harmony" de Ollama que
   parece estar generando el 500 de U-21 (la tool call vuelve como
   texto, y la parsea la escalera de rescate de `braze`, del lado
   cliente, más tolerante). **Ya implementado y probado** (es el brazo
   B del A/B de `docs/constrained-decoding-ab-design.md`), pero **solo
   expuesto vía `+ablate:prompt-tools` en `braze-bench`** — no hay
   ningún flag ni campo de `Config` que lo exponga en `braze chat`/
   `braze-cli` hoy. Habilitarlo para uso interactivo es wiring nuevo,
   chico (agregar el campo a `Config` + un flag de CLI), no una palanca
   nueva de harness.
3. **Retry en Ollama** — hoy explícitamente deshabilitado
   (`crates/braze-model/src/retry.rs`, doc del módulo: *"Ollama gets no
   retry, per the v5 dictamen: hammering a saturated local backend
   doesn't help... so `OllamaBackend` simply doesn't call this
   helper"*). Esa decisión asume que un 500 de Ollama es agotamiento de
   recursos (RAM/contención) — el 500 de U-21 es un **fallo de parseo
   de contenido**, una categoría distinta donde un reintento (con
   sampling no determinista) podría simplemente producir una generación
   nueva que sí parsee. Vale la pena reabrir esa decisión
   específicamente para este tipo de 500 (distinguible por el cuerpo
   del error, `"error parsing tool call"`), no para 500s en general.

**No implementado en esta sesión** — quedan como candidatos concretos
para la próxima vez que se retome trabajo sobre `gpt-oss:20b`, fuera del
alcance del plan de resolución de issues EMSE en curso.

## Comparación directa: `gemma4:e4b` (no-thinking) vs `gpt-oss:20b` (thinking)

Motivada por U-21/U-22: ¿un "modelo estrella no-thinking" ya disponible
en el proyecto (`gemma4:e4b`, sin campo `thinking` — confirmado vía
`/api/chat` crudo) cierra la brecha con `gpt-oss:20b` sin el riesgo de
esa clase de crash? Corridos en el MISMO sweep (`default.toml`,
$n{=}95$ cada uno, `--seed 42`, misma sesión/carga térmica de Nitro —
mismo formato que `sweep-capacity-hardware-2026-07-13.md`):
`docs/sweep-gemma4e4b-vs-gptoss20b-2026-07-13.json`.

| Modelo | Pass rate | Wilson 95% CI |
|---|---|---|
| `gemma4:e4b` (no-thinking) | 92/95 = 96.8% | [91.1, 98.9] |
| `gpt-oss:20b` (thinking) | 95/95 = 100.0% | [96.1, 100.0] |

Delta (gpt-oss − gemma4:e4b) = $+3.2$pp, Newcombe 95% CI
$[-1.2, +8.9]$ — **cruza cero**: `gpt-oss:20b` gana nominalmente pero
no es distinguible del ruido a este $n$. Las 3 fallas de `gemma4:e4b`
son las 3 mismas instancias de `read_file_basic` (`AssertionToolCall`)
— el patrón ya conocido de usar `shell_exec wc -l` en vez de
`read_file` con respuesta correcta pero tool "equivocada" según el
assert estricto, no una falla de capacidad real. **Cero crashes de
ningún lado** en esta corrida — la suite scripteada (tareas cortas,
timeout 180s) no genera la presión de contexto que dispara U-21/U-22 en
sesiones largas.

**Síntesis**: en la suite scripteada, empatados dentro del ruido —
`gpt-oss:20b` con el punto estimado más alto, pero sin ventaja
estadística. En uso abierto real, la asimetría es otra: `gemma4:e4b`
lleva cientos de corridas acumuladas entre las Fases 1-3 de este
proyecto **sin un solo crash de esta clase**, mientras que `gpt-oss:20b`
tuvo 2 crashes duros + 1 agotamiento de rondas en una sola sesión de
playground. No alcanza para una recomendación definitiva (falta
replicar la sesión de playground con `gemma4:e4b` para saber si
simplemente tuvo menos exposición a sesiones largas, o si genuinamente
no tiene ese modo de falla) — pero es la primera evidencia concreta,
no solo la hipótesis "sin thinking, sin ese crash", de que la vía
no-thinking no cuesta capacidad medible en la suite scripteada.
