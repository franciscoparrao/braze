# Análisis Ollama/Gemma para adaptación de braze

Fecha: 2026-07-11  
Proyecto: `braze`  
Objetivo: escoger candidatos Ollama y arrancar un perfil de adaptación para la familia Gemma, usando el harness actual de `braze` en vez de intuición.

## Tesis

`braze` no necesita "auto-aprendizaje" en el sentido de modificar pesos ni reescribir su propio código. Lo útil y controlable es un **motor de adaptación de harness**: ejecutar prompts diagnósticos, leer eventos y métricas, inferir qué palancas ayudan a una familia de modelos, y guardar un perfil de configuración validado por A/B.

Para Gemma, la pregunta no es solo "qué modelo es mejor", sino:

- si Gemma funciona mejor como executor, lead, planner o router de herramientas;
- si el plan en prosa sigue dañando como dañó a Qwen y al coder;
- si `task_list` repara ese daño;
- si `search_tools` reduce distracción cuando el catálogo de herramientas crece;
- si `skills` explicit-only aportan guía procedural sin inflar contexto;
- si los errores dominantes son de schema, semántica, respuesta vacía, selección de herramienta o saturación de contexto.

## Candidatos Ollama

La selección se basa en el catálogo Ollama provisto en la conversación y las páginas oficiales enlazadas al final. No es un ranking general: está filtrada para agentic workflows locales, tools, tamaños pequeños/medios y utilidad experimental para `braze`.

| Rol | Candidato | Motivo |
|---|---|---|
| Target Gemma chico | `gemma4:e2b` | Punto bajo de la curva Gemma; debería mostrar cuánto compensa el harness dentro de la familia. |
| Target Gemma central | `gemma4:e4b` | Ya aparece como buen lead en los sweeps recientes; tamaño razonable para Nitro. |
| Gemma medio | `gemma4:12b` | Escalón natural para curva Gemma si entra en hardware. |
| Lead Gemma fuerte | `gemma4:26b` | MoE/activo reducido según ficha; candidato para lead si la memoria lo permite. |
| Micro-router | `functiongemma:270m` | No como agente general; interesante para tool-selection/function-calling si se integra después. |
| Rival SLM tool-use | `granite4.1:3b` | Tools, JSON estructurado y tamaño chico; buen contraste contra Gemma. |
| Rival agentic edge | `lfm2.5:8b` | Enfocado en tool calling local rápido; buen candidato de comparación. |
| Rival edge general | `ministral-3:3b` / `ministral-3:8b` | Tools y contexto largo; compara familia distinta en tamaños parecidos. |
| Ancla histórica | `qwen2.5:3b`, `qwen2.5:7b` | Ya hay mucha evidencia; sirven para calibrar si la suite nueva se comporta como las anteriores. |
| Techo local | `qwen3.5-coder` o `qwen3.5:9b` | Control superior para ver cuándo el harness deja de aportar. |

## Modelos no prioritarios

No priorizar ahora:

- embeddings (`nomic-embed-text`, `mxbai-embed-large`, `embeddinggemma`, etc.);
- OCR/visión/medicina/safety salvo experimento específico;
- modelos cloud-only o enormes donde Nitro no sea el cuello;
- modelos muy viejos sin tools si la hipótesis es agentic harness;
- modelos especializados en SQL, traducción o dominios cerrados que no prueban las palancas generales de `braze`.

## Hipótesis Gemma

1. `gemma4:e4b` puede ser mejor **lead proactivo** que executor principal.
2. Gemma podría sufrir con plan en prosa igual que Qwen/coder; `task_list` debe probarse como reemplazo tipado.
3. `functiongemma` puede servir más adelante como router/validador de tool calls, pero no debe mezclarse en el primer sweep.
4. `search_tools` probablemente ayuda cuando `noise_tools` cruza el umbral de deferral, pero hay que medirlo por familia.
5. `skills` explicit-only deben evaluarse después de tener métricas de skill loading en el bench; hoy el bench no tiene asserts de `SkillLoaded`.

## Motor de adaptación propuesto

Nombre tentativo: `braze-adapt` o `braze-model-profile`.

Responsabilidad:

1. Leer JSON de `braze-bench`.
2. Agrupar por `model_family`, modelo, brazo y skill.
3. Clasificar modos de fallo:
   - respuesta vacía;
   - schema validation failure;
   - tool execution failure;
   - assertion text/files;
   - timeout;
   - max iterations;
   - exceso de tokens/rondas;
   - tool-search miss;
   - degeneración inducida por planner.
4. Proponer perfil de harness:
   - `planner = off | task_list_only | prosa`;
   - `lead = off | proactive | reactive`;
   - `tool_search_threshold`;
   - `max_turn_iterations`;
   - `tool_output_max_bytes`;
   - `skills.mode`;
   - hints de familia en system prompt.
5. Validar con A/B antes de promover.

Ejemplo de perfil final:

```json
{
  "family": "gemma",
  "model": "gemma4:e4b",
  "role_fit": {
    "executor": "unknown",
    "lead": "strong_candidate",
    "planner": "requires_task_list_ab"
  },
  "recommended": {
    "planner": "off",
    "task_list": "measure",
    "search_tools": "measure_with_noise",
    "skills": "explicit_only",
    "lead_turns": 3
  },
  "failure_modes": []
}
```

## Primera suite: `gemma-diagnostic.toml`

La suite creada en `crates/braze-bench/suites/gemma-diagnostic.toml` tiene 12 tareas, cada una orientada a una clase real de comportamiento:

| Tarea | Qué mide |
|---|---|
| `gemma_no_tool_arithmetic` | Obediencia no-tool y salida mínima. |
| `gemma_simple_read_lines` | Tool call básico y extracción de dato. |
| `gemma_distractor_exact_file` | Selección entre archivos distractores. |
| `gemma_multi_step_sum_write` | Lectura múltiple + escritura final. |
| `gemma_error_recovery_near_filename` | Recuperación semántica ante archivo ausente. |
| `gemma_schema_discipline_read` | Argumentos exactos sin campos inventados. |
| `gemma_empty_response_after_tool` | Cierre textual después de usar tool. |
| `gemma_plan_prose_stress` | Vulnerabilidad a plan en prosa. |
| `gemma_task_list_candidate` | Misma clase de tarea donde `+ablate:task-list` debería ayudar. |
| `gemma_tool_search_noise_read` | Deferral de tools con catálogo ruidoso. |
| `gemma_spanish_instruction_following` | Instrucciones en español y formato acotado. |
| `gemma_permission_boundary` | No convertir denegación de permisos en éxito falso. |

## Brazos iniciales

Sweep mínimo:

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/gemma-diagnostic.toml \
  --backends "ollama:gemma4:e2b,ollama:gemma4:e4b,ollama:granite4.1:3b,ollama:lfm2.5:8b,ollama:ministral-3:3b,ollama:qwen2.5:3b" \
  --repetitions 3 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-gemma-diagnostic-2026-07-11.json
```

Sweep de palancas Gemma:

```bash
L="ollama:gemma4:e4b"
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/gemma-diagnostic.toml \
  --backends "ollama:gemma4:e2b,ollama:gemma4:e2b+lead:$L,ollama:gemma4:e2b+plan:$L,ollama:gemma4:e2b+plan:$L+ablate:task-list,ollama:gemma4:e4b,ollama:gemma4:e4b+ablate:task-list" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-gemma-levers-2026-07-11.json
```

Sweep de `search_tools`:

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/gemma-diagnostic.toml \
  --backends "ollama:gemma4:e4b,ollama:gemma4:e4b+ablate:tool-search-threshold=1000000,ollama:granite4.1:3b,ollama:granite4.1:3b+ablate:tool-search-threshold=1000000" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-gemma-tool-search-2026-07-11.json
```

## Criterios de decisión

Promover una palanca para Gemma solo si mejora al menos uno de estos sin degradar claramente otro:

- pass rate total;
- `error_recovery`;
- `multi_step`;
- `distractor_selection`;
- rounds promedio;
- tokens promedio;
- tasa de `model_backend_error` por respuesta vacía;
- schema validation failures;
- wall time dentro del mismo sweep.

No promover:

- una palanca que solo mejora `single_tool` y daña `no_tool`/`multi_step`;
- un planner que induce respuestas vacías;
- un router que baja errores de selección pero sube rondas/tokens sin pass-rate;
- una skill que solo funciona porque el prompt la nombra pero no mejora outcome.

## Fuentes

- Catálogo de Ollama pegado por el usuario en la conversación.
- Ollama library: https://ollama.com/library
- `gemma4`: https://ollama.com/library/gemma4
- `functiongemma`: https://ollama.com/library/functiongemma
- `granite4.1`: https://ollama.com/library/granite4.1
- `lfm2.5`: https://ollama.com/library/lfm2.5
- `ministral-3`: https://ollama.com/library/ministral-3
