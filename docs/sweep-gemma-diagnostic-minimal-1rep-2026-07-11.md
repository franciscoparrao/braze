# Sweep Gemma diagnostic minimal 1rep — 2026-07-11

Fuente: `docs/sweep-gemma-diagnostic-minimal-1rep-2026-07-11.json`  
Suite: `crates/braze-bench/suites/gemma-diagnostic.toml`  
Backends: `gemma4:e2b`, `gemma4:e4b`, `granite4.1:3b`, `lfm2.5:8b`, `ministral-3:3b`, `qwen2.5:3b`  
Repeticiones: 1. Este sweep sirve para selección de finalistas, no para inferencia estadística.

## Resultado ejecutivo

No hubo `harness_err`, `model_backend_error` ni `run_error`. El sweep es usable como señal inicial. Los mejores por pass rate bruto fueron `gemma4:e2b` y `qwen2.5:3b` con 9/12. Separando fallos de presupuesto de tokens, `gemma4:e2b` sube a 11/12 de comportamiento, porque sus dos fallos `assertion_max_tokens` sí contenían el texto esperado.

| backend | pass bruto | pass comportamiento | avg rounds | avg ms | avg input tok | avg output tok | schema fail | exec fail | denied | compactions |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `gemma4:e2b` | 9/12 | 11/12 | 2.33 | 6569 | 2781 | 268 | 0 | 1 | 1 | 0 |
| `qwen2.5:3b` | 9/12 | 9/12 | 1.83 | 2317 | 2190 | 58 | 0 | 3 | 1 | 0 |
| `gemma4:e4b` | 7/12 | 8/12 | 2.25 | 12350 | 2697 | 325 | 0 | 4 | 1 | 0 |
| `lfm2.5:8b` | 7/12 | 8/12 | 2.83 | 16135 | 4145 | 957 | 1 | 7 | 2 | 8 |
| `granite4.1:3b` | 6/12 | 6/12 | 2.00 | 1959 | 2360 | 35 | 0 | 2 | 1 | 0 |
| `ministral-3:3b` | 6/12 | 6/12 | 2.17 | 3240 | 2408 | 48 | 0 | 1 | 1 | 0 |

## Lectura por modelo

`gemma4:e2b` es el mejor candidato Gemma executor en este barrido. Falla `no_tool` y `spanish_instruction` solo por presupuesto de tokens, con texto esperado presente; su fallo real de comportamiento es `planner_stress` (`assertion_text`). Pasa `task_list_candidate`, `tool_search`, `error_recovery` y los casos de escritura.

`qwen2.5:3b` empata en pass bruto y gana en eficiencia. Es el mejor control no-Gemma: menos rondas, menos tiempo y salida mucho más corta. Sus fallos son `multi_step` (`assertion_text`), `error_recovery` (`assertion_text`) y `task_list_candidate` (`assertion_files`).

`gemma4:e4b` queda peor como executor que `gemma4:e2b` en este suite: 7/12 bruto, 8/12 si se perdona presupuesto. Aun así conviene mantenerlo como finalista secundario porque está validado como lead en sweeps previos y esta medición justamente cubre el hueco executor.

`lfm2.5:8b` no parece buen finalista para harness pequeño: costo de tokens alto, 8 compactaciones y degradación severa en `tool_search_noise_read`.

`granite4.1:3b` y `ministral-3:3b` son muy rápidos, pero no compiten en correctness dentro de esta suite. Pueden servir como brazos de latencia, no como candidatos principales.

## Tool search

| backend | pass | failure | rounds | tool calls | schema fail | exec fail | compactions | input tok | output tok |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| `gemma4:e2b` | yes | | 2 | 1 | 0 | 0 | 0 | 2469 | 392 |
| `gemma4:e4b` | yes | | 2 | 1 | 0 | 0 | 0 | 2471 | 326 |
| `qwen2.5:3b` | yes | | 2 | 1 | 0 | 0 | 0 | 2477 | 38 |
| `granite4.1:3b` | no | `assertion_tool_call` | 2 | 1 | 0 | 0 | 0 | 2472 | 31 |
| `ministral-3:3b` | no | `assertion_tool_call` | 2 | 1 | 0 | 0 | 0 | 2341 | 69 |
| `lfm2.5:8b` | no | `assertion_text` | 10 | 9 | 1 | 3 | 8 | 21754 | 3847 |

En logs, `lfm2.5:8b` y `ministral-3:3b` activaron `search_tools` con `hits=0`. `lfm2.5:8b` luego entro en bucle, compacto 8 veces y produjo al menos un `shell_exec` con schema invalido. Esto sugiere una interaccion mala entre deferral de catalogo y la forma en que el modelo formula busquedas.

## AssertionToolCall

El JSON actual no conserva nombres de `AssistantToolCall`, solo `tool_calls_total` y si `expect_tool_call` se cumplio. Por eso no se puede determinar post-hoc que herramienta exacta uso el modelo cuando `expected_text_found=true` pero `expected_tool_called=false`.

Casos importantes:

| backend | task | tool calls | text ok | files ok | exec fail | lectura |
|---|---|---:|---:|---:|---:|---|
| `gemma4:e4b` | `gemma_simple_read_lines` | 1 | yes | n/a | 0 | probable uso de herramienta alternativa con respuesta correcta |
| `gemma4:e4b` | `gemma_distractor_exact_file` | 1 | yes | n/a | 0 | probable uso de herramienta alternativa con respuesta correcta |
| `lfm2.5:8b` | `gemma_plan_prose_stress` | 1 | yes | n/a | 0 | herramienta distinta a `read_file`, pero respuesta textual correcta |
| `ministral-3:3b` | `gemma_permission_boundary` | 1 | yes | n/a | 1 | denegacion ocurrio, pero no se registro `write_file` esperado |

Siguiente mejora recomendada: agregar `tool_call_names: Vec<String>` a `TaskResult` y serializarlo en JSON, o conservar logs de eventos por tarea en modo diagnostico. Sin eso, `AssertionToolCall` mezcla dos fenomenos: fallo real de herramienta y solucion correcta por camino alternativo.

## Finalistas sugeridos

Para repetitions=3:

1. `ollama:gemma4:e2b` — mejor Gemma executor y mejor comportamiento si se separa verbosity.
2. `ollama:qwen2.5:3b` — control fuerte, eficiente y estable.
3. `ollama:gemma4:e4b` — mantener por relevancia para la tesis Gemma lead/executor, aunque su 1rep executor fue inferior.

Opcional si hay presupuesto: `ollama:lfm2.5:8b` solo para estudiar el fallo de `search_tools`, no como candidato principal.

## Comando finalistas propuesto

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 \
RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/gemma-diagnostic.toml \
  --backends ollama:gemma4:e2b,ollama:qwen2.5:3b,ollama:gemma4:e4b \
  --repetitions 3 \
  --temperature 0.2 \
  --task-timeout-secs 120 \
  --no-ollama-stop \
  --output docs/sweep-gemma-diagnostic-finalists-3rep-2026-07-11.json
```
