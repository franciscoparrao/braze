# A/B stencil GBNF (Fase 3) — 2026-07-21

**Pregunta**: ¿el constrained decoding del envelope
(`crates/braze-model/src/stencil.rs`) mejora pass rate / elimina
schema_fail en el LocalBackend, y a qué costo?

**Diseño**: suite `default.toml` (19 tareas × 3 reps = 57 corridas por
brazo), `local:qwen2.5:3b`, CPU en Nitro, seed 42, timeout 180s.
Brazo ON = stencil activo (default); brazo OFF = `BRAZE_LOCAL_GRAMMAR=off`.
Datos crudos: `sweep-stencil-on-v2.json` / `sweep-stencil-off-v2.json`.
Nota metodológica: el LocalBackend samplea greedy puro → las reps son
control de no-determinismo de floats/tarea, no muestreo; el poder del
test viene del pareo por (tarea, rep).

## Resultado

| métrica | ON (stencil) | OFF (baseline) |
|---|---|---|
| pass rate | **41/57 (72%)** | 40/57 (70%) |
| pass^2 / pass^3 | 66.7% / 63.2% | 66.7% / 63.2% |
| schema_fail (validación engine) | 7 | 4 |
| exec_fail | 25 | 19 |
| denials | 8 | 3 |
| rescues (extracción textual) | 57 | 58 |
| backend_errors / timeouts | 0 / 0 | 0 / 0 |
| avg_ms / median_ms | 8342 / 6139 | 6727 / 5519 |

**McNemar pareado**: solo-ON=2, solo-OFF=1, **p=1.0** — sin diferencia.
Discordantes: ON ganó `multi_step_multiply_and_write` (reps 0 y 2), OFF
ganó `edit_file_function_body` (rep 1). Por skill: idénticos salvo
multi_step (ON 4/9 vs OFF 2/9) y single_tool (ON 19/21 vs OFF 20/21).

## Lectura honesta

1. **La hipótesis pre-registrada ("schema_fail + rescues → 0") no se
   cumple, por dos razones estructurales**, no por defecto del stencil:
   - `rescues` en el LocalBackend cuenta la extracción textual normal
     (es prompt-tools total): NO puede bajar a 0 por diseño. Métrica mal
     elegida en la hipótesis.
   - `schema_fail` que mide el bench es la validación del engine contra
     el `input_schema` (campos requeridos, tipos). El envelope GBNF
     garantiza **JSON válido + nombre real + tag cerrado**, no
     conformidad de args. Esa clase ya la había vaciado el fix del
     preámbulo de Fase 1; lo residual (4-7 por brazo) es args
     no-conformes, que el envelope no ataca.
2. **Sin constraint tax medible**: pass rate igual (41 vs 40, p=1.0),
   pass^k idéntico. La latencia media es ~24% mayor en ON pero la
   mediana solo ~11% — el overhead por token de la gramática es
   pequeño y el resto es varianza de trayectorias multi-ronda (n=57).
3. **Lo que sí compra el stencil** (no medible en esta suite): la
   garantía por construcción — JSON roto, nombre alucinado o tag sin
   cerrar son *ingenerables*, no "reparables". En `default.toml` con
   qwen2.5:3b el baseline ya no comete esa clase de error; la garantía
   pagaría en modelos/situaciones off-distribution (la clase #17).
4. Diferencial claro de ON en `multi_step` (4/9 vs 2/9) — direccional,
   n chico, no concluyente.

## Los 3 bugs que el proceso destapó (el dividendo real del A/B)

Los tres latentes desde Fase 1, invisibles a los smokes, corregidos:

1. **Double-accept del sampler** (`9a38f22`… en `e21bbe6`):
   `llama_sampler_sample` ya acepta internamente; el accept explícito
   duplicado era inofensivo con greedy y fatal con gramática
   (`GGML_ASSERT(!stacks.empty)` → SIGABRT).
2. **Prompt > n_batch = abort C++** (`9a38f22`): un prompt de ronda
   >2048 tokens mataba el proceso entero. Ahora decode en chunks +
   guard legible de n_ctx. (Esto tumbó el primer intento del A/B.)
3. **Token de control espurio = error duro** (`916c0f1`): un
   `<|im_start|>` sampleado a mitad de generación fallaba
   `token_to_piece` y abortaba el stream — 3 fallos asimétricos en el
   brazo OFF de la primera pasada (contaminación que obligó al re-run).
   Ahora es fin-de-turno limpio.

La primera pasada (v1, `sweep-stencil-{on,off}.json` en Nitro) queda
como procedencia: ON 41 / OFF 40 con la contaminación del bug 3.

## v3 — gramática schema-derivada (mismo día)

Se implementó la gramática derivada del `input_schema` por tool
(conversor en `stencil.rs`: requeridos forzados en el orden de la lista
`required`, tipos/enums cerrados, `additionalProperties` cerrado;
envelope qwen brancheado por tool, selección por destinatario en
harmony) y se repitió el A/B (`sweep-stencil-{on,off}-v3.json`):

| métrica | ON (schema-derivada) | OFF |
|---|---|---|
| pass rate | 40/57 | 40/57 |
| McNemar | solo-ON=4, solo-OFF=4, p=1.0 | — |
| schema_fail | 9 | 3 |
| rescues | 63 | 56 |

**Dos lecturas importantes y una anomalía honesta:**

1. **El loop de reparación del engine ya absorbe los schema_fail**: en
   v3, TODAS las corridas con schema_fail (ambos brazos) terminaron en
   pass. `schema_fail → 0` no compra pass rate en esta suite — a lo más
   rondas/tokens. La palanca del stencil compite contra un harness que
   ya tiene la clase cubierta río abajo (repair message + retry).
2. **Pass rate empatado por tercera vez** (41/40, 41/40, 40/40): el
   resultado robusto del A/B es "sin ganancia, sin constraint tax".
3. **Anomalía no resuelta**: el schema_fail del brazo ON (9) está
   concentrado en UNA trayectoria degenerada (`edit_file_function_body`
   rep1: 8 fallos en un loop de 9 edit_file, igual pasó) que NO
   reproduce en aislamiento (5/5 limpio con sesiones preservadas) y
   cuyo mecanismo quedó indiagnosticable porque el engine no trazaba
   los fallos de validación — corregido (dispatch ahora loguea tool +
   args + error), la próxima ocurrencia será inspeccionable. Hipótesis
   abierta: la gramática convierte fallos de *parseo* (invisibles al
   contador) en calls bien formadas que llegan a validación, moviendo
   fallos de un contador a otro sin cambiar el resultado.

**Veredicto de la Fase 3 tras tres A/Bs:** el stencil es correcto,
gratis en capacidad, y su garantía es real — pero en `default.toml` con
qwen2.5:3b el harness existente (preámbulo con schema + escalera de
rescate + retry de validación) ya deja al stencil sin clase de error
que matar. El A/B que queda con señal esperable: modelos/suites donde
el baseline NO está saturado (p.ej. gemma4:e4b y sus 3 fallos
sistemáticos de single_tool, o una suite adversarial de escaping).
