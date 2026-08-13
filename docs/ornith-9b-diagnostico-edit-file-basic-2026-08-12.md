# Diagnóstico: ornith:9b y `edit_file_basic` — segundo caso de la arista MODEL—BENCH

**Fecha**: 2026-08-12. **Contexto**: sweep de modelos nuevos
(`docs/sweep-nuevos-locales-2026-08-12.json`, default.toml n=95 seed 42) —
ornith:9b quedó 90/95 con sus 5 fallos concentrados en `edit_file_basic`
(5/5, pass^k plano en 94.7%: fallo sistemático, no flakiness).

## Método

Mini-suite con solo `edit_file_basic`, 5 repeticiones seed 42,
`BRAZE_BENCH_KEEP_SESSIONS=1`, contra Nitro (Ollama 0.32.1,
`--keep-alive 2m`). Se leyeron las transcripciones y los sandboxes
preservados de las 5 corridas.

## Hallazgo

Las 5 corridas fallan con `[AssertionToolCall]` — NO con la aserción de
contenido. La secuencia es idéntica en las 5:

1. `read_file {"path": "config.txt"}`
2. `write_file {"path": "config.txt", "content": "version=2\n"}`

El `config.txt` final contiene `version=2` **en los 5 sandboxes**. La
tarea se resuelve funcionalmente el 100% de las veces; lo que falla es
`expect_tool_call = "edit_file"`: el modelo prefiere la vía
read→rewrite-completo sobre la edición dirigida. `schema_fail=0`,
`exec_fail=0`, 3 rondas limpias (~17-35s).

## Lectura

Es la misma estructura del caso gemma4:e4b con `read_file_basic`
(2026-08-10, `docs/gemma4-e4b-diagnostico-read-file-basic-2026-08-10.md`):
preferencia de política, no capacidad — arista MODEL—BENCH, lado banco.
Con grading de equivalencia funcional, ornith:9b queda **de facto 95/95
(100%)** en default.toml — empatando a gpt-oss:20b como el único otro
modelo local que satura la suite (dense 9B, 5.6GB, cabe entero en Nitro).

## La decisión de banco, ahora con dos casos

La pregunta abierta desde e4b (¿aceptar equivalencia funcional o exigir
la tool nombrada?) gana un segundo ejemplar y un matiz nuevo:

- **A favor de aceptar equivalencia**: el prompt dice "edita el archivo",
  no "usa la tool edit_file" — el modelo cumple lo pedido. Penalizarlo
  mide adherencia a una convención no comunicada.
- **A favor de mantener la aserción estricta**: read→rewrite-completo NO
  es una vía neutral en la práctica — sobre archivos grandes es propensa
  a corrupción por un modelo chico (la clase de riesgo que motivó la
  guarda por tamaño de `write_file`, 2026-07-28) y quema tokens
  proporcionales al archivo entero. `expect_tool_call = "edit_file"`
  codifica una preferencia de ingeniería real, aunque la tarea del bench
  sea demasiado chica para que el riesgo se manifieste.

**Decisión del autor (2026-08-12, mismo día): opción intermedia —
equivalencia funcional como métrica oficial, con reporte dual.**
Implementada en braze-bench:

- `passed` (oficial: pass rate, pass^k, McNemar) acepta equivalencia
  funcional: la aserción de ruta (`expect_tool_call`) se exime SOLO
  cuando otra aserción verifica el logro (texto/archivos/cargo check);
  sin aserción de logro, la ruta sigue vinculante (eximirla dejaría la
  tarea sin chequeo). `expect_no_tool_call` nunca se exime (mide
  disciplina de ruta a secas).
- `passed_strict` (funcional Y ruta respetada) viaja en cada fila del
  JSON y como columna `strict` de la tabla; las filas funcional-pass con
  ruta desviada se marcan `[RouteMiss]` en el detalle.
- Procedencia: `metadata.grading = "functional-primary+strict-secondary/
  2026-08-12"`; DBV trata un ref pre-dual como drift (su `passed` era
  estricto — parearlo contra funcional cruza semánticas justo en la
  clase e4b/ornith).

Verificado en vivo (mini-suite de este doc, mismo seed): las 5 corridas
pasan `[RouteMiss]`, pass^5=100%, `strict 0/5`. **Bajo la métrica
oficial, ornith:9b queda 95/95 en default.toml** — segundo modelo local
que satura la suite. La brecha pass/strict (95 vs 90) queda visible como
dato de adherencia de ruta, no enterrada en el grading.

## Nota de salud del banco

El sweep marcó `edit_file_basic` con r_pbis=-0.064 (discriminación
negativa). Es el mismo hallazgo visto desde el lado del ítem: el ítem
anti-discrimina porque el único modelo que lo falla lo hace por política,
no por capacidad. Si la decisión de banco mantiene la aserción estricta,
considerar si el ítem debe además exigir contenido inicial más largo
(que la vía rewrite-completo sí arriesgue algo) para que mida lo que
dice medir.
