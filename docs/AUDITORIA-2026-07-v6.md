# Auditoría v6: braze — optimización del harness para modelos pequeños/medios

Fecha: 2026-07-09
Proyecto: `braze`
Base observada: commit `29cfc92` (origin/main + 2 commits locales) + worktree sucio (12 archivos, +562/-14: el cierre de H-3 — eventos de palancas SLM — verificado pero SIN commitear).
Verificación previa ejecutada (misma sesión, antes del sweep en curso): `cargo test --workspace` — 802 tests verdes; `cargo clippy --workspace --all-targets -- -D warnings` — limpio.
Método: 3 subagentes de investigación en paralelo (verificación de abiertos v4/v5 contra HEAD, revisión escéptica del backlog opencode, pasada fresca de código sobre las palancas SLM) + 1 hallazgo confirmado en vivo con un smoke sweep real contra Nitro.

## Meta y alcance

v6 complementa v1-v5 — NO las reemplaza. Lente única: **qué le falta al harness para
compensar mejor la escala de modelos pequeños/medios**, que es la tesis del proyecto.

Cubre:

1. Verificación del estado real de los hallazgos que v4/v5 dejaron abiertos (los docs
   quedaron desactualizados: varios se cerraron en `2923f63`/`00b3ab1`/`adbc9a4`/worktree).
2. Hallazgos NUEVOS (serie **I-\***) — el central confirmado en vivo, no especulado.
3. Revisión escéptica del backlog de `docs/opencode-a-braze.md` (los 7 ítems restantes
   del top-10), incluyendo una corrección de severidad a uno de sus hallazgos.
4. Roadmap re-priorizado por ratio valor-SLM/esfuerzo.

No cubre: rechequeo de grupos cerrados de v1/v2, TUI (sin cambios desde v5), MCP.

## Estado de salud del motor

| Métrica | Valor | Δ vs v5 |
|---|---|---|
| Líneas Rust | ~37.594 | +1.861 |
| Líneas `engine.rs` | 7.887 | +232 (P1.1 sigue creciendo) |
| Tests | 802 verdes | +29 |
| `cargo clippy -D warnings` | limpio | = |
| Worktree dirty | 12 archivos (+562) | cierre H-3 sin commitear |

### FIXED desde v5 — confirmados en código (no en docs)

- **H-1** cache tokens en `TaskResult` (`2923f63`; reporte en `docs/H-1-cierre-cache-tokens.md`).
- **H-9** sampling keys en `KNOWN_OVERRIDE_KEYS` (`2923f63`).
- **opencode ítems 1/3/4** — `max_turn_iterations`/`planner_max_tokens`, `tool_output_max_bytes`/`max_lines`, formatter per-extension (`2923f63`; el bug del byte-cap del ítem 3 y el test muerto del ítem 4 se corrigieron en `adbc9a4`).
- **v4 P0.4** budget assertions + suite `self_improvement.toml` (`00b3ab1`; guard `+plan:` fortalecido en `adbc9a4`).
- **SI-2 completo**: sintaxis `+lead:` (`d89b134`) + medición permanente (`00b3ab1`) + **A/B real con datos** (`docs/sweep-si2-lead-ab-2026-07-09.md`, commit `29cfc92`): error_recovery 3/15 → 15/15 con ambos leads. PERO ver I-1 — la interpretación "escalación reactiva" de ese A/B está en revisión.
- **H-3** métricas de palancas SLM (worktree actual): `AgentEvent::TextualRescueApplied`/`EscalationToLead`/`SummaryFallbackAttempted` + conteos `rescued_tool_calls`/`leader_escalations`/`compaction_count`/`summary_fallbacks` en `TaskResult` y ambas tablas del reporte. 802 tests verdes. **Pendiente: commit.**
- Cierres de v3 que v5 daba abiertos: **A3** (`find_closest_line` en edit_file), **A4** (steering a write_file en not-found), **F2** (`coerce_arguments_to_schema`), **D4** (tests best-of-n×escalación), **D2 medio** (`braze chat/run` sí aplica `ollama_temperature/top_p/top_k/repeat_penalty` vía config/env — falta solo flags CLI por invocación).

## Hallazgos NUEVOS (serie I)

### I-1 [ALTA][CONFIRMADO EN VIVO] — Los knobs de escalación no están expuestos: todo A/B de `+lead:` midió apertura proactiva, no escalación reactiva

**Evidencia**: `EscalatingBackend` tiene builders `with_lead_turns`/`with_failure_threshold`/`with_escalation_turns` (escalation.rs:98-114, documentan incluso "0 = purely reactive") pero `grep` da **cero call sites fuera de braze-model y sus tests**: ni `braze-bench` (`backend_spec.rs:304-314` construye `EscalatingBackend::new(lead, worker)` pelado) ni `braze-cli` (`main.rs:271-272`, ídem). Corren siempre con `DEFAULT_LEAD_TURNS=3`.

**Confirmación en vivo** (smoke sweep g10-weak-skills, 2026-07-09, con la instrumentación H-3 recién construida): `error_recovery_wrong_filename` baseline 0/3 → `+lead:qwen3.5-coder` 3/3, con `leader_escalations = 0` en todas las corridas. Las tareas del suite convergen en 2-4 rounds — caen enteras dentro de la ventana de apertura proactiva (`RouteDecision::LeadOpening`), y el disparo reactivo (`LeadEscalating`) nunca ocurre.

**Impacto**: el A/B de SI-2 (`docs/sweep-si2-lead-ab-2026-07-09.md`) atribuye la mejora a "escalación reactiva estilo Goose" cuando el mecanismo dominante fue "el lead maneja los primeros 3 turnos". Son palancas distintas con costos distintos (la apertura proactiva paga lead-latency SIEMPRE; la reactiva solo al detectar floundering). Para el paper esto es una confusión de variables directa. Además: nadie puede correr el modo "puramente reactivo" (lead_turns=0) que es el que la narrativa SOTA/Goose describe.

**Arreglo**: (a) config keys `lead_turns`/`lead_failure_threshold`/`lead_escalation_turns` + env vars `BRAZE_LEAD_*`, aplicadas en ambos composition roots; (b) claves `+ablate:lead-turns=N;lead-threshold=N;lead-window=N` en el bench para el A/B de 3 brazos que corresponde: baseline / lead proactivo (default actual) / lead puramente reactivo (`lead_turns=0`). Un re-sweep con instrumentación H-3 está corriendo al escribir esto — sus columnas `escalat`/`rescues` cuantificarán el confound sobre las 19 tareas.

### I-2 [ALTA] — Los caps tácticos escalan por *presencia* del context budget, no por su *tamaño*

**Evidencia**: `full_observations_byte_budget`, `effective_tactical_compaction_threshold`, `effective_tactical_full_observations` (engine.rs:2885-2965) hacen `match context_budget_tokens { Some(_) => default, None => default*10 }` — el **valor** del budget se ignora. Todo backend Ollama tiene budget (`ollama_context_budget_tokens` se computa siempre), así que todo modelo local corre con los caps mínimos (8KB de observaciones full / 5 obs / threshold 40) aunque sea qwen3.5-coder con `num_ctx` grande en Nitro. Peor: `MAX_FULL_OBSERVATIONS_TOTAL_CHARS` (8.000) coincide con el cap por-tool-result de braze-tools-local (~8.000) — **una sola página de `read_file` consume el budget entero de observaciones**, degradando "keep last 5 full" a "keep last 1".

**Impacto**: la patología de U-17 (loops de relectura por pérdida de la observación que el modelo necesitaba) se arregló para backends cloud (multiplicador ×10 cuando no hay budget) pero sigue viva exactamente para la población objetivo de la tesis: modelos locales medianos con contexto real de 32K+.

**Arreglo**: derivar los tres caps proporcionalmente del budget (p.ej. `bytes = budget_tokens*4*fracción` con la fracción actual como piso), en vez del binario Some/None. Conservar el respeto a overrides `+ablate:` explícitos.

### I-3 [MEDIA] — La clasificación de fallos para escalación se rompe en las capas de render

**Evidencia**: `observation_is_a_failure` (escalation.rs:303-332) clasifica por contenido del string (`"\"exit_code\""`, `starts_with("failed to read '")`, marker `[post-edit check]`), pero dos capas reescriben ese contenido antes de que la escalación lo lea: el clearing de resultados viejos (history.rs:450-453, reemplaza todo por `"[tool result cleared: ...]"` conservando `is_error`) y el colapso ACI (history.rs:282-297, conserva solo la primera línea — el marker post-edit va en la 2ª). Efecto: un exit_code≠0 viejo pasa a contar como fallo del modelo (anula D3) y una regresión post-edit deja de contar (anula F3). Ocurre justo en turnos largos de floundering, donde ambas capas se activan.

**Arreglo**: clasificar sobre el evento crudo (campo `failure_kind` en `ToolResult` o `AgentEvent`) en vez del string renderizado; o preservar los markers en clearing/colapso.

### I-4 [MEDIA] — El hint de familia de modelo está gateado a `backend == "ollama"`

**Evidencia**: `braze-cli/main.rs:391-392` y `braze-bench/runner.rs:159-161` solo pasan `model_name` al system prompt si el backend es Ollama. Un `openrouter:qwen/qwen3-coder` o `z-ai/glm-5.2` no recibe hint — y el leak de template GLM (U-15/U-16) se observó precisamente vía OpenRouter. GLM tiene rescue dedicado (`parse_glm_arg_tag_tool_call`) pero ningún `ModelFamily` ni hint (prompt.rs:31-40 cubre solo Qwen; tampoco gemma, hoy driver diario).

**Arreglo**: pasar el nombre de modelo también en OpenRouter; añadir familias GLM y Gemma a `ModelFamily` con sus hints.

### I-5 [BAJA] — D4 dedup por message-count vs compaction mid-turn

**Evidencia**: el dedup de rondas de `EscalatingBackend::route` (escalation.rs:128-134) asume que la historia solo crece dentro de un turno; la compaction táctica (engine.rs:1810) puede encogerla. Una colisión exacta de counts haría que una ronda genuina reuse una decisión stale (no consume lead_turns/ventana, no evalúa la racha). Probabilidad baja, fallo silencioso. **Arreglo**: comparar además un hash del último mensaje.

### I-6 [BAJA] — La escalera de rescate corta tras el primer rung que matchea

**Evidencia**: `break` en engine.rs:613 — una respuesta que mezcle un `<tool_call>` válido Y un `<function=>` desnudo pierde el segundo (queda como texto). Raro (los modelos no mezclan gramáticas en una respuesta) pero es pérdida silenciosa. El orden de rungs en sí está bien: un rung que no matchea devuelve vacío y el siguiente escanea el texto completo.

### I-7 [corrección al backlog opencode] — "H-26 chunkTimeout [ALTA]" estaba mal diagnosticado

`reqwest::read_timeout` se resetea con cada read exitoso (`http_client.rs:19-27`) — **ya es un chunk timeout** de 600s, y el caso "conexión colgada sin chunks" está cubierto y testeado. El gap real es menor: 600s es enorme para backends cloud y no es configurable per-backend. Re-etiquetado BAJA; arreglo S si se quiere (parametrizar `READ_TIMEOUT`).

### Áreas auditadas y encontradas genuinamente bien

- Falso rescate plain-text: bien defendido (exige respuesta 100% JSON con shape de tool call, filtro F1, fences excluidos).
- Paralelización de best-of-n (P1.4): **es segura hoy** — el observer solo se toca con `emit_deltas=true` y el dedup D4 es order-independent; `futures::future::join_all` sobre `complete_once` funcionaría sin refactor. Ganancia real solo en cloud (Ollama serializa salvo `OLLAMA_NUM_PARALLEL>1`) — documentar al implementar.
- System prompt default: longitud/tono apropiados para SLMs (~180 palabras); el único gap es I-4.

## Backlog opencode — dictamen sobre los 7 ítems restantes

| # | Ítem | Dictamen | Esf. |
|---|---|---|---|
| 2 (resto) | Knob on/off del colapso ACI (`+ablate:no-prune`) + exponer `MAX_FULL_OBSERVATIONS_TOTAL_CHARS`/multiplicador | **HACER** — es LA ablation que el paper necesita para aislar la palanca central; hoy no se puede apagar. Se fusiona naturalmente con I-2 | S |
| 10 | `references` con descriptions (dirs externos + descripción en system prompt) | **HACER** — único ítem restante con argumento SLM-first directo ("un SLM no sabe dónde buscar") y medible en bench | S-M |
| 5 | Permission declarativo por patrón | Válido pero NO es SLM-first (seguridad/ergonomía); su sub-propuesta "deny si el archivo no existe" es inexpresable con patrones estáticos. Solo si la prioridad es cerrar H-6 y compañía de una vez | M-L |
| 6 | chunkTimeout | Ver I-7 — re-etiquetado BAJA | S |
| 7 | Skills loader | Diferir — las 116 skills existentes están escritas para modelos frontier; en un 3B serían distractor. Requeriría skills escritas para SLMs | L |
| 8 | `experimental.policies` | Sobre-ingeniería — el sub-caso real (tope de costo por turno) es P0.2 y no necesita policy engine | M |
| 9 | Hook surface | Estructural, no SLM; el claim "H-3 más barato vía plugins" quedó refutado por la práctica (H-3 se cerró hoy vía variantes de enum en una sesión). Fase 2 | L |

Hipótesis fuera del top-10 que sube al backlog: **subagent isolation para tareas finitas** — como experimento medible (A/B contra colapso-solo), no como implementación directa. Ataca el mismo problema que el colapso ACI por otra vía; el caso de 481K tokens/turno la motiva.

## RE-CONFIRM — abiertos de v4/v5 verificados hoy (selección priorizada)

| ID | Título | Sev. | Evidencia HOY | Esf. |
|---|---|---|---|---|
| P0.2 | TurnBudget (tokens/costo/walltime por turno) | CRÍT | cero tope de tokens/costo; sin `turn_budget_exhausted`; `expect_max_cost_usd` se parsea pero NO se enforcea (bloqueado por falta de pricing → E5) | M |
| P0.3 | write_file preflight destructivo | CRÍT | solo warning post-hoc por shrink; sin `allow_shrink`/hash | M |
| H-2 | `+ablate:no-caching` | ALTA | `RECOGNIZED_KEYS` sin la clave; `build` nunca llama `with_prompt_caching_enabled` | S |
| H-4 | Summary fallback dropea su `Usage` | ALTA | engine.rs ~1291 — el costo del fallback es invisible (H-3 ya cuenta el intento, falta el costo) | S |
| H-6 | `env` sin args clasifica seguro | ALTA | classifier.rs:192 | S |
| F3 | Post-edit regression (`is_error:false`) no cuenta para escalación | ALTA | sin cambio; interactúa con I-3 | S |
| E1 | Ablations faltantes (`no-planner`/`no-lead`/`no-compaction`/`no-preflight`) | ALTA | `RECOGNIZED_KEYS` backend_spec.rs:447-471 | M |
| H-7 | `durable_events` sin cota | ALTA | simple_compactor.rs:200-262 | M |
| P1.2/D1 | `ModelFamily` solo Qwen | ALTA | fusionado con I-4 | M |
| P1.4 | best_of_n secuencial | ALTA | viable sin refactor (ver arriba) | M |
| P1.1 | engine.rs 7.887 líneas | ALTA | +232 desde v5 — cada cierre lo agranda | L |
| H-17 | `active_ablations` ausente de `RunMetadata` | MEDIA | metadata.rs:16-34 | S |
| H-18 | Cache tokens Anthropic-native | MEDIA | anthropic_wire.rs:331 | S |
| H-13 | Sampling knobs ignorados sin warning fuera de Ollama | MEDIA | backend_spec.rs:540 | S |
| — | Matriz executor/+planner/+lead/+ambos | — | PARCIAL: faltan brazos `+planner` y `+planner+lead` | (sweep) |
| — | 11 de las 17 métricas v4 | — | destacan `estimated_cost_usd` (bloquea P0.2/E5), `planner_rounds`, `turn_budget_exhausted` | M |

(El resto de MEDIAs/BAJAs de v5 — H-8/10/11/12/14/15/16/19/20-25, P2.x, B*, E2/E3 — siguen abiertos sin cambio; ver v5 para el detalle. No se re-listan para no duplicar.)

## Roadmap v6 — priorizado por valor-SLM/esfuerzo

**Paquete 0 — higiene inmediata (hoy):**
1. Commitear el cierre H-3 (worktree, 12 archivos, verificado).
2. Actualizar `docs/sweep-si2-lead-ab-2026-07-09.md` con la corrección I-1 cuando termine el re-sweep en curso.

**Paquete 1 — desconfundir la palanca estrella (I-1 + ablations, ~1 sesión):**
3. I-1: knobs de escalación en config/env/bench (`lead_turns=0` habilita el modo puramente reactivo).
4. A/B de 3 brazos: baseline / lead proactivo / lead reactivo — la tabla que el paper necesita.
5. H-2 `+ablate:no-caching` + opencode-2 `+ablate:no-prune` + E1 (`no-planner`/`no-lead`/`no-compaction`) + H-17 `active_ablations` en metadata — juntos convierten el bench en la matriz de ablations completa.

**Paquete 2 — el harness respeta el contexto real del modelo (I-2, ~1 sesión):**
6. I-2: caps tácticos proporcionales al budget (con piso = valores actuales).
7. H-4 (Usage del summary fallback) + F3/I-3 (clasificación de fallos robusta a render).

**Paquete 3 — gobernanza de costo (P0.2, requiere pricing):**
8. Pricing table mínima por backend/modelo → `estimated_cost_usd` → enforcement de `expect_max_cost_usd` + `TurnBudget` con `turn_budget_exhausted`.

**Paquete 4 — resto por conveniencia:** I-4 (hints multi-familia), opencode-10 (references), P0.3 (preflight), H-6, P1.4 (paralelizar best-of-n), H-18, H-13. Diferidos explícitos: P1.1 (split engine.rs — hacerlo antes de Fase 2), skills/hooks/policies (Fase 2), subagent isolation (diseñar A/B primero).
