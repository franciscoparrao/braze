# A/B de SI-2: ¿ayuda `+lead:` en la práctica?

Fecha: 2026-07-09
Contexto: SI-2 (`docs/self-improvement-exercises.md` § SI-2) agregó la sintaxis `+lead:<spec>` a `BackendSpec` (commit `d89b134`) y quedó permanentemente medido como tarea de bench en `self_improvement.toml` (commit `00b3ab1`), pero ninguno de los dos cierra el loop real: ¿la escalación reactiva `EscalatingBackend` efectivamente mueve el pass rate, y a qué costo? Este documento es ese A/B.
Estado: **CERRADO, con corrección de interpretación** — ver § "CORRECCIÓN (2026-07-10)" antes de citar este documento. Datos crudos en `docs/sweep-si2-lead-ab-2026-07-09.json`/`.log`.

## ⚠ CORRECCIÓN (2026-07-10) — el mecanismo NO fue escalación reactiva

Hallazgo I-1 de `docs/AUDITORIA-2026-07-v6.md`, confirmado con un re-sweep instrumentado (H-3, mismo diseño de 3 backends × 19 tareas × 5 reps; datos en `docs/sweep-si2-lead-ab-h3-2026-07-09.json`/`.log`, `braze_git_commit: e11d628`):

- Este documento atribuye la mejora a la "escalación reactiva estilo Goose". **Eso es incorrecto.** Los knobs de `EscalatingBackend` no estaban expuestos en ningún composition root, y todo `+lead:` corrió con `DEFAULT_LEAD_TURNS = 3`: el lead maneja los primeros 3 rounds de cada turno **proactivamente**. Como las tareas del suite convergen en 2-4 rounds, el lead manejó casi todos los turnos completos.
- El re-sweep instrumentado lo cuantifica: en 190 corridas con `+lead:`, hubo **1 sola escalación reactiva** (`leader_escalations = 1`, en `distractor_selection` de gemma4:e4b). En `error_recovery` — la skill que pasa de 0-3/15 a 15/15 — hubo **cero**: el 100% de esa mejora es apertura proactiva del lead, no rescate reactivo del worker.
- La replicación del efecto agregado sí es sólida (re-sweep: baseline 63/95, `+lead:qwen3.5-coder` 91/95, `+lead:gemma4:e4b` 90/95 — consistente con las cifras de abajo dentro de los intervalos). Lo que cambia no son los números sino **qué palanca los produjo**: este A/B midió "lead proactivo los primeros 3 turnos" vs baseline, no "escalación reactiva" vs baseline.
- Dato adicional del re-sweep: `rescued_tool_calls = 0` en las 285 corridas — el rescate textual nunca se activó con estos modelos en este suite (los fallos de qwen2.5:3b son de validación de schema, no de formato textual de tool call).
- El A/B que sí separa los dos mecanismos (baseline / lead proactivo / lead puramente reactivo vía `+ablate:lead-turns=0`, habilitado por el cierre de I-1 en `9aff6aa`) corre como `docs/sweep-lead-3brazos-2026-07-10.*`.

Las secciones siguientes se conservan como estaban (los números son válidos); leer "lead proactivo" donde diga "escalación reactiva".
Reproducibilidad: `braze_git_commit: adbc9a4` (post-auditoría de los commits del corte de créditos, ver commit "Audita y corrige el trabajo de otros modelos..."), `suite_fingerprint: 8deba9d2bffdf3c1`, temp 0.2, sin seed fijo, sin `--top-p/--top-k/--repeat-penalty`.

## Diseño

`crates/braze-bench/suites/default.toml` (19 tareas, 5 skills) × 3 backends × 5 repeticiones = 285 corridas, contra Nitro (`BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434`, `--no-ollama-stop`):

- **`ollama:qwen2.5:3b`** — baseline, sin escalación.
- **`ollama:qwen2.5:3b+lead:ollama:qwen3.5-coder`** — el mejor modelo local del proyecto (6/6 en g10-weak-skills, ver CLAUDE.md) como lead.
- **`ollama:qwen2.5:3b+lead:ollama:gemma4:e4b`** — el modelo que el usuario está usando a diario con buenos resultados en tareas sencillas; candidato de lead más barato.

0 harness errors en las 285 corridas.

## Resultados — comparación por backend

| Backend | Pass rate (±95% Wilson) | avg rounds | avg ms | avg tok_in | avg tok_out | schema_fail | exec_fail |
|---|---|---|---|---|---|---|---|
| `qwen2.5:3b` (baseline) | 67/95 (70.5%, ±9pp) | 2.2 | 3,699 | 2,612 | 145 | 17 | 32 |
| `+lead:qwen3.5-coder` | 89/95 (93.7%, ±5pp) | 2.7 | 25,620 | 3,885 | 197 | 1 | 1 |
| `+lead:gemma4:e4b` | 88/95 (92.6%, ±5pp) | 2.5 | 13,874 | 2,978 | 388 | 8 | 12 |

Los intervalos de confianza del baseline (hasta 79.5% en el extremo superior) y de ambos backends con lead (desde 87.6%/88.7% en el extremo inferior) no se solapan — la mejora es real, no ruido de `--repetitions`.

## Resultados — comparación por skill

| Backend | skill | pass rate |
|---|---|---|
| baseline | single_tool | 28/35 (80%) |
| baseline | no_tool | 15/15 (100%) |
| baseline | multi_step | 8/15 (53%) |
| baseline | **error_recovery** | **3/15 (20%)** |
| baseline | distractor_selection | 13/15 (87%) |
| +lead:qwen3.5-coder | single_tool | 35/35 (100%) |
| +lead:qwen3.5-coder | no_tool | 12/15 (80%) |
| +lead:qwen3.5-coder | multi_step | 15/15 (100%) |
| +lead:qwen3.5-coder | **error_recovery** | **15/15 (100%)** |
| +lead:qwen3.5-coder | distractor_selection | 12/15 (80%) |
| +lead:gemma4:e4b | single_tool | 30/35 (86%) |
| +lead:gemma4:e4b | no_tool | 15/15 (100%) |
| +lead:gemma4:e4b | multi_step | 14/15 (93%) |
| +lead:gemma4:e4b | **error_recovery** | **15/15 (100%)** |
| +lead:gemma4:e4b | distractor_selection | 14/15 (93%) |

## Hallazgos

1. **El efecto de `+lead:` está casi enteramente en `error_recovery`**: 3/15 (20%) → 15/15 (100%) con ambos leads. Es la skill donde `qwen2.5:3b` falla más duro sola (necesita reconocer un error del propio tool call y corregir el approach, no solo ejecutar una tarea directa), y es exactamente donde escalar a un modelo más capaz cierra la brecha por completo. El resto de las skills se mueve mucho menos, y en dos casos empeora (punto 2).

2. **`+lead:` no es estrictamente superior — hay regresión en `no_tool` y `distractor_selection` con `qwen3.5-coder`**: `no_tool` 15/15 → 12/15, `distractor_selection` 13/15 → 12/15. La escalación reactiva se dispara en algunos casos donde el modelo chico ya andaba bien sola, y el lead introduce una fuente de error donde antes no la había (posible causa: el lead entra a mitad de una tarea trivial y la complica; no investigado en este documento — queda como hipótesis para revisar el criterio de trigger reactivo). `+lead:gemma4:e4b` no muestra esta regresión (`no_tool` se mantiene en 100%, `distractor_selection` sube a 93%), así que el efecto no es inherente a "tener un lead" sino a la interacción específica con `qwen3.5-coder`.

3. **El costo de latencia es real y no uniforme**: `+lead:qwen3.5-coder` es ~6.9× más lento en promedio que el baseline (25.6s vs 3.7s); `+lead:gemma4:e4b` es ~3.75× (13.9s). Ninguno de los dos es "gratis" — cualquier claim de "la escalación mejora el pass rate" en el paper necesita ir acompañada de esta cifra, no aislada.

4. **`gemma4:e4b` como lead es estadísticamente indistinguible de `qwen3.5-coder`** (88/95 vs 89/95, intervalos casi enteramente solapados) **a ~54% de su latencia promedio** (13.9s vs 25.6s) — valida empíricamente la elección de driver diario del usuario para este rol específico, sin necesidad de cargar el modelo más pesado.

5. **Outlier de degeneración incluso con lead**: `multi_step_sum_two_files` rep 5 con `+lead:gemma4:e4b` tardó 141.7s, 10 rounds, 7 schema_fail, 2 exec_fail, y **aun así falló** (`AssertionFiles`). La escalación reactiva no es una garantía — vale la pena mirar esta traza puntual si se investiga el criterio de trigger (punto 2).

## Limitaciones

- **`--repetitions 5`** da intervalos de confianza razonables para el agregado por-backend (±5-9pp) pero son anchos para el desglose por-skill (n=15 por celda) — un `no_tool` de 12/15 vs 15/15 es una diferencia de 3 corridas; no se puede descartar sampling noise sin más repeticiones.
- **`qwen3.5-coder`'s `ollama_model_digests` quedó `null`** en el metadata del JSON (posible mismatch de tag exacto vs `:latest` instalado) — no afecta el resultado, pero sí la trazabilidad exacta de versión del modelo si se re-corre este sweep más adelante y el modelo en Nitro cambió.
- Sin seed fijo (`--seed` no se pasó) — cada repetición usa el sampling no determinístico normal del proveedor, no reproducible byte-a-byte.
- No se corrió la matriz completa (executor solo / +planner / +lead / +planner+lead) — este documento cubre solo el eje `+lead:`, que era el bloqueante específico de SI-2.

## Cómo reproducir

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/default.toml \
  --backends "ollama:qwen2.5:3b,ollama:qwen2.5:3b+lead:ollama:qwen3.5-coder,ollama:qwen2.5:3b+lead:ollama:gemma4:e4b" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-si2-lead-ab-<fecha>.json
```

## Próximo paso recomendado

SI-2 queda cerrado en las tres capas: sintaxis (`d89b134`), medición permanente vía bench (`00b3ab1`), y ahora evidencia de que la sintaxis efectivamente mueve el resultado (este documento). Siguiente en el roadmap v5 Paquete 1 (sin cambios respecto a lo que ya estaba anotado):

1. H-3 métricas de palancas SLM (`TextualRescueApplied`, `EscalationToLead`, `CompactionOccurred`, `SummaryFallbackAttempted`) — permitiría explicar *por qué* `error_recovery` es la skill que se mueve (¿cuántas de esas 12 correcciones fueron vía rescate textual antes de escalar, vs escalación directa?).
2. H-2 `+ablate:no-caching` parser en bench.
3. H-18 Anthropic-native cache tokens.
4. Matriz executor solo / +planner / +lead / +planner+lead para la publicación del primer resultado — este documento cubre solo la columna `+lead`.
