---
type: wiki-page
created: 2026-07-14
tags: [braze-bench, grader, validacion]
---

# Validación del grader automático + preservación de transcripciones

## Qué es

`BRAZE_BENCH_KEEP_SESSIONS` es un flag real (env var) que preserva el
sandbox final y la transcripción JSONL de cada run de `braze-bench`
antes del borrado incondicional que corre por defecto en cada sweep.
Se usó para muestrear 62 transcripciones reales y calificarlas a mano
contra el veredicto del assert automático: **62/62 (100%) de
acuerdo**.

## Por qué existe

La review EMSE (Issue 4, [[venue-y-review-emse]]) señaló que ~4.000+
runs del paper se califican por asserts scripteados sin ninguna
validación humana sistemática — el paper ya documentaba un bug real en
un assert anterior (texto laxo que aceptaba narración como respuesta
válida). `BRAZE_BENCH_KEEP_SESSIONS` existía antes como parche local no
commiteado (mencionado en `docs/sweep-search-tools-ab-n15-2026-07-12.md`);
se promovió a flag real para poder hacer esta validación.

## Detalles

### Implementación

`crates/braze-bench/src/preserve.rs` (nuevo módulo) — copia el
sandbox + sesión a `braze-bench-preserved-sessions/<backend>/<task>/
rep<N>/` justo antes de los dos `remove_dir_all` que ya existían en
`runner.rs`. Comportamiento default (env var sin definir) queda
byte-idéntico al de antes — verificado con smoke test con y sin el
flag. 3 tests nuevos, 141→143 tests totales del crate, clippy limpio.

### Metodología de la validación

Muestra sobre los dos sweeps más citados del paper: 38 transcripciones
de la curva de escala (`llama3.2:1b` baseline + `+lead`, 19 tasks
c/u) y 24 de tool-deferral (`qwen2.5:3b` deferred + full-inventory, 6
tasks × 2 reps c/u). Para cada una: transcripción completa leída a
mano (mensajes, tool calls, resultados, texto final) y comparada contra
`passed`/`failure_cause` del JSON.

### Resultado y hallazgo cualitativo notable

62/62 de acuerdo. El hallazgo más interesante no fue un desacuerdo sino
una confirmación del diseño: en un run de `noisy_multi_step`, el modelo
escribió el string literal `"int_a + int_b"` en el archivo de salida
(nunca computó la suma) pero su texto final afirmó *"the sum...is 30"*
— una confabulación que un check de solo-texto habría dejado pasar
(el string "30" aparece en esa frase), pero que el check de archivo
(`expect_file_contains`) atrapó correctamente. Evidencia de que el
diseño de asserts duales (texto Y archivo) no es redundante.

También se documentó (no es un bug): varias tasks son estrictas sobre
qué herramienta específica se usa (`expect_tool_call`), no solo si la
respuesta final es correcta — un run que resuelve la tarea vía una
herramienta distinta pero equivalente falla el assert aunque el texto
sea correcto. Es una decisión de diseño de la suite (mide selección de
herramienta), no un falso negativo del grader.

## Relacionado

- [[venue-y-review-emse]] — el review que motivó esta validación

## Referencias

- `docs/grader-validation-2026-07-13.md`
- `crates/braze-bench/src/preserve.rs`
- `docs/audit-transcripts-scale-curve-2026-07-13.json`
- `docs/audit-transcripts-tool-deferral-2026-07-13.json`
