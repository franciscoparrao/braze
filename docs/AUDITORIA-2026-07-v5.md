# Auditoría v5: braze — complemento integral del estado del motor

Fecha: 2026-07-08
Proyecto: `braze`
Base observada: commit `d89b134` (origin/main) + worktree sucio (prompt-caching OpenRouter, rescate GLM arg-tags U-15, strip leaked tool-calls U-16, compaction calibrada por budget U-17/U-18, spinner/banner/bordes TUI, read_file clamp al budget, cache tokens event layer — NO agregados a TaskResult).
Verificación ejecutada: `cargo test --workspace` — 773 tests verdes; `cargo clippy --workspace --all-targets -- -D warnings` — limpio. Sin tocar código en esta ronda.

## Meta y alcance

Esta auditoría complementa v1-v4 — NO las reemplaza:

- **v1** (`docs/AUDITORIA-2026-07.md`, 2026-07-04): bug-hunt MVP. 55 hallazgos (12 CRÍTICA, 14 ALTA, 16 MEDIA, 13 BAJA); Grupos A–H. Cerrados 2026-07-04/05.
- **v2** (`docs/AUDITORIA-2026-07-v2.md`, 2026-07-05): re-audita post-fix + superficie OpenRouter/TUI-2/best-of-n. Grupos I–N. Cerrados 2026-07-05/06, 519 tests verdes post-cierre. Verificación E2E contra Anthropic real encontró N-2b en vivo.
- **v3** (`docs/AUDITORIA-2026-07-v3.md`, 2026-07-06): lente SLM-first. 39 hallazgos agrupados O–S. Todos ABIERTOS (especificación de trabajo).
- **v4** (`docs/AUDITORIA-2026-07-v4.md`, 2026-07-08): revisión de estado. Tesis: brecha = gobernanza ejecución + medición realista + arquitectura editable. Hallazgos P0.1-P2.4 + roadmap 5 fases. Todos ABIERTOS.

v5 cubre:

1. Verificación del estado actual del código (build/test/clippy, worktree dirty).
2. Hallazgos NUEVOS no presentes en v1-v4 (encontrados en los diffs WIP y rechequeos dirigidos de cuatro subagentes).
3. RE-CONFIRM de hallazgos v3/v4 que siguen ABIERTOS con evidencia file:line actualizada.
4. FIXED desde v3/v4 confirmados en código.
5. Tabla consolidada de backlogs abiertos por prioridad.
6. Roadmap de cierre actualizado.

Qué NO cubre v5: rechequeo sistemático de Grupos A–N de v1/v2 (cerrados con tests + E2E Anthropic/Ollama — no se reabren sin razón).

## Estado de salud del motor

### Snapshot cuantitativo

| Métrica | Valor |
|---|---|
| Crates | 13 |
| Líneas Rust | ~35,733 |
| Líneas `engine.rs` | 7,655 |
| Tests | 773 verdes |
| `cargo clippy -D warnings` | limpio |
| Archivos en worktree dirty | 20 (1608 insertions) |
| Commits desde v4 | 1 (`d89b134` Add lead backend support to braze-bench) |

### FIXED desde v3/v4 — confirmados en código

Cerrados en el último tramo (commits `9f21eb3`, `69a4527`, `d89b134` + worktree actual):

- **`+lead:` en `braze-bench`** (v4 P0.1): `BackendSpec::build_lead` (backend_spec.rs:289-298), parser reconoce `+lead:` (backend_spec.rs:104-106), runner compone `EscalatingBackend` (backend_spec.rs:304-314), display_name lo preserva. Tests cubren combinaciones (executor solo, +planner, +lead, +ambos, nesting inválido, 2 leads rechazado, orden variable). backend_spec.rs:728-817. **CERRADO**.

- **WIP: cache tokens en CompletionEvent y AgentEvent** (v4 "cache write/read tokens" capa evento): `CompletionEvent::Usage` gana `cache_read_tokens`/`cache_write_tokens: Option<u32>` (backend.rs:42-56); `RoundUsage` struct (`engine.rs:166`) reemplaza el tuple de 3 que creció a 5 campos; propagado a `AgentEvent::Usage` (event.rs:96-109). `#[serde(default)]` para backward-compat con rollout logs antiguos (event.rs:97-108). Tests: backward-compat, OpenRouter stream parser, best-of-n summing Some/None mix, eventos persistidos. **CERRADO en la capa event**. Falta agregación en `metrics.rs::TaskResult` (ver H-1 NEW).

- **WIP: prompt caching breakpoints OpenRouter** (`openrouter_wire.rs:apply_cache_breakpoints:269-281`): 3 breakpoints (last tool, system message, last message) recomputados por llamada seguimiento la conversación creciente. `model_supports_explicit_caching` gatea `anthropic/`+`qwen/` only (openrouter_wire.rs:256-258); cualquier otro proveedor recibe byte-idéntico a pre-WIP (test `build_request_does_not_mark_breakpoints_for_a_provider_that_caches_automatically`). 4 tests cubren casos. Wireo vía `OpenRouterBackend::with_prompt_caching_enabled` (openrouter.rs:90+) + `Config.enable_prompt_caching` (default `true`, config.rs:158-174). Tests paridad env/override. **CERRADO para OpenRouter** (Anthropic-native caching fuera de scope de este pase — ver H-2 NEW).

- **WIP: rescate GLM arg-tags** (hallazgo U-15, `docs/usability-log-2026-07-07-si2.md`): `parse_glm_arg_tag_tool_call` (engine.rs:2263) añadido a escalera de rescates tagged tras qwen2.5/qwen3-coder. Gramática `z-ai/glm-5.2` observada in-vivo vía OpenRouter: nombre del tool como texto plano seguido de cero o más pares `KEY_OPEN KEY_CLOSE VALUE_OPEN VALUE_CLOSE` (motor de comillas inteligentes, no bytes ASCII). Requiere al menos un par para dispararse (un bare name sin tags es indistinguible de prosa y no se rescata — documentado en doc comment). Scalar values se quedan como strings (consistencia con `parse_function_xml_tool_call`); JSON estructurado si parsea. Tests: forma exacta observada, con prosa alrededor, coerción selectiva, no errores con gramática ausente, tags mal formados. **CERRADO**.

- **WIP: strip_leaked_tool_call_shapes en summary fallback** (hallazgo U-16): `attempt_tools_free_summary_round` (engine.rs:1125) originalmente persistía bloques de tool-call perdidos por el modelo (GLM habitúa su template nativa aunque el prompt diga "no tools disponibles") como AssistantText verbatim. Ahora bufferiza, detecta patrones tagged/pythonic, los striptea y recién emite AssistantText limpio al observer. `cleaned.trim().is_empty()` retorna Ok(false) en vez de persistir vacío. 3 tests cubren casos. Streaming live de deltas también desactivado aquí mismo (coincide con `complete_with_best_of_n`). **CERRADO**.

- **WIP: compaction calibrada por budget** (hallazgos U-17, U-18): `effective_tactical_compaction_threshold` + `effective_tactical_full_observations` + `full_observations_byte_budget` (engine.rs:2744-2873) escalan umbrales cuando NO hay `context_budget_tokens` configurado (backends cloud como Anthropic/OpenRouter). Bajo `NO_CONTEXT_BUDGET_SCALE_MULTIPLIER=10`:
  - threshold default 40 → 400 con cloud.
  - full_observations default 5 → 50.
  - byte_budget default 8KB → 80KB.
  - Override explícito via `+ablate:` se preserva verbatim (regresión test cubre `an_explicit_non_default_compaction_threshold_is_never_scaled`).

  Razón: el cap de 8KB era para `num_ctx=8192` (Ollama), pero copiarlo a Anthropic generaba compacciones agresivas y colapsos prematuros. 7 tests nuevos. **CERRADO**.

- **WIP: read_file clamp al output budget** (hallazgo U-6): si el caller pide `limit=N` con N grande y el body excede `MAX_TOOL_OUTPUT_BYTES=8000`, el `wrap`-generic truncation disparaba consejo "narrow your query" (correcto para grep pero perverso para read_file — el fix real es page forward con offset). `clamp_to_output_budget` (read_file.rs:106) reduce end_line para que la page que devuelva ya entre en el budget y dispare el continuation trailer propio "more lines below, use offset=X". 3 tests cubren: lim oversized, sin clamp si ya entra, one-line-oversized todavía se retorna. **CERRADO**.

- **WIP: spinner + banner + bordes composer TUI** (`docs/usability-log-2026-07-07-si2.md`): braille-dot spinner (`SPINNER_FRAMES` en app.rs:60) en hints de espera (turn_running o switching_model); banner block-icon en lib.rs:115 impreso antes de raw mode; composer ahora con bordes `─` arriba/abajo (app.rs:285 set_block). COMPOSER_ROWS sube de 3 a 5 (terminal.rs). 4 tests spinner cyclying, 3 of to_crossterm_color coverage. **CERRADO con caveat A-1** (ver NEW — el composer pierde bordes tras submit/backtrack porque las recreaciones de TextArea no llaman set_block).

## Hallazgos NUEVOS (no presentes en v1-v4)

### H-1 [CRÍTICA][NEW] — Cache tokens NO se agregan a `TaskResult` (WIP incompleto)

**Evidencia**: `metrics.rs:227-237` sólo agrega `input_tokens`/`output_tokens` desde `AgentEvent::Usage`. El WIP añade `cache_read_tokens`/`cache_write_tokens` a `CompletionEvent`, `RoundUsage` y `AgentEvent::Usage`, pero el fold del bench los ignora. JSON de resultados no los reporta por row ni aggregated. Tests en `metrics.rs:651-716` pasan `cache_read_tokens: None, cache_write_tokens: None` en construções puntuales pero no hay test que verifique la agregación si vinieran poblados.

**Impacto**: work completo en 2/3 del stack y roto en la métrica final. Para un A/B paper "con vs sin prompt caching" (uso central en `docs/usability-log-2026-07-07-si2.md`), el bench no puede aislar. V4 pidió cache tokens en métricas — media-cerrado.

**Arreglo**: añadir `cache_read_tokens: Option<u32>`/`cache_write_tokens: Option<u32>` a `TaskResult` (y/o `RunMetadata`), sumar vía `metrics()` alongside input/output. Test con dos `AgentEvent::Usage` que reportan cache y assert agregar.

### H-2 [ALTA][NEW] — `bench::BackendSpec::build` no invoca `with_prompt_caching_enabled`

**Evidencia**: `BackendSpec::build` (backend_spec.rs:378-406) construye `OpenRouterBackend` sin llamar `.with_prompt_caching_enabled(...)`. Como el default del backend es `true` (openrouter.rs:57), el bench siempre manda markers — la config `BRAZE_ENABLE_PROMPT_CACHING=false` es respetada por `braze-cli/src/main.rs:198` (production) pero **ignorada en braze-bench**. No existe `+ablate:no-caching`.

**Impacto**: para un ablation "con vs sin caching" en el bench, no hay knob. La variable aparente del paper queda bloqueada.

**Arreglo**: o (a) `BackendSpec::build` respeta un flag `enable_prompt_caching: bool` de `SamplingSpec`/`BackendSpec` y el CLI lo pasa, o (b) añadir `+ablate:no-caching` parser que construye `OpenRouterBackend::with_prompt_caching_enabled(false)` explícito. Test de paridad.

### H-3 [ALTA][NEW] — `metrics.rs` no captura rescates, escalaciones, compaction count, summary fallbacks

**Evidencia**: `grep -rn 'compaction_count|leader_escalations|summary_fallbacks|rescued_tool_calls|estimated_cost' crates/` → 0 hits. v4 pidió todas como métricas. Las acciones SÍ existen en el engine (rescates en engine.rs:1975, escalación en escalation.rs:184 `tracing::info!`, compaction en engine.rs `tracing::warn!`, summary fallback en engine.rs:1125) pero no hay `AgentEvent` correspondiente, ni `RunMetadata` lo recoge.

**Impacto**: el instrumento del paper no puede medir lo que más distingue a braze SLM-first (rescates textuales, escalación lead/worker, colapso de observaciones). Comparación entre configuraciones queda ciega a estas palancas.

**Arreglo**: añadir variantes a `AgentEvent` (`TextualRescueApplied { parser_name }`, `EscalationToLead { trigger }`, `CompactionOccurred`, `SummaryFallbackAttempted`) — O emitirlas vía un canal lateral para el bench. `metrics.rs` las cuenta y `RunMetadata` las reporta. v4 listó 17 métricas nuevas recomendadas — host still all 0 in bench.

### H-4 [ALTA][NEW] — `attempt_tools_free_summary_round` sigue dropeando `Usage` (acknowledged)

**Evidencia**: engine.rs:1181-1186 comentario explícito: "Usage is fine to skip too: this degraded round isn't worth the same bookkeeping as a normal one". v4 P1.3 flagueó como subestimación de coste justo en rondas degradadas. `max_tokens` del request usa `self.max_tokens` completo (engine.rs:1140) — una ronda summary puede consumir la misma cota que una de trabajo.

**Impacto**: una sesión larga con varios fallback sessions subestima tokens acumulados (sesión documentada `ccd4621b`: 481K tokens acumulados en 40 rondas).

**Arreglo**: (a) acumular `Usage` también para summary fallback; (b) limitar `max_tokens` de summary a `min(max_tokens, 768)` configurable; (c) añadir `phase` o evento en `AgentEvent::Usage` para distinguir `work`/`planner`/`summary`/`rescue`.

### H-5 [ALTA][NEW] — `shell_exec` sin timeout de pared

**Evidencia**: `shell_exec::run` (shell_exec.rs:41-63) usa `kill_on_drop(true)` (v2 N-33 fix) pero ningún `tokio::time::timeout`. `tail -f`, prompts interactivos, cat de FIFO bloquean indefinitely. Test actual prueba abortar via drop del awaiting future, no expiración.

**Impacto**: un comando que cuelga mata el turno sin timeout visible. Sólo la cancelación externa del motor lo despierta.

**Arreglo**: timeout default configurable (p.ej. 300s) con `tokio::time::timeout`. Exponer via config.

### H-6 [ALTA][NEW] — `env` solo (sin subcomando) filtra API keys al contexto del modelo

**Evidencia**: `classifier.rs:74, 192-208` — `env` puro (sin seguir) → Reversible → sin confirmación → stdout volcado a `ToolResult` content → entra en el prompt del próximo round. Varias `ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, `BRAZE_*` acaban en el cloud con el siguiente mensaje.

v1/E2 cerró `env <program>` pero no el leak de `env` solo. Test `safe_readonly_commands_are_reversible` incluye `["env"]` como Reversible.

**Impacto**: secrets highlight del proveedor. PELIGROSO en sesión abierta.

**Arreglo**: quitar `env` del allowlist Reversible (default-deny como cualquier programa no auditado). "Listar entorno" no es caso de uso válido del agente. Test covers no-leak.

### H-7 [ALTA][NEW] — `durable_events` crece sin cota

**Evidencia**: `simple_compactor.rs:200-255` — `split` mueve todo `ToolCallCompleted`/`AssistantToolCall`/`PermissionDecided`/`CompactionOccurred` a `durable_events` y nunca los colapsa. `DurableState::summary` tiene `MAX_SUMMARIES_KEPT=5` (v2 N-25 fix) pero `durable_events` Vec no tiene cota.

Una sesión con 200 tool calls × 8KB por result → ~1.6MB durable block re-renderizado en cada prompt. v2 N-25 sólo tapó summary, no este Vec.

**Impacto**: crecimiento monótono de tokens en cada próximo round — el característica que `clear_tool_uses_20250919` de Anthropic resuelve y que v4 P1.5 (`ToolCatalog` por ronda) pide.

**Arreglo**: implementar `clear_tool_uses` estilo Anthropic (mantener `tool_use`, colapsar `tool_result` a 1-línea placeholder para los no en últimas N). Se puede mover al compactor o vivir en engine.rs. Test acota tamaño de `durable_events` después de 100+ tool calls.

### H-8 [MEDIA][NEW] — Composer pierde bordes tras submit y backtrack en TUI

**Evidencia**: `app.rs:285-286` hace `composer.set_block(...)` una vez en `App::new`. Pero `submit` (app.rs:1049 y 1055) y `backtrack_to` (app.rs:893) reconstruyen el composer con `TextArea::default()`/`TextArea::from(...)` sin llamar `set_block` de nuevo. El approve-overlay lo dibuja "manualmente" con `Block` (app.rs:1441), reforzando que el composer debería conservarlo en sus recreaciones. Tras cualquier submit (y tras cualquier Esc-Esc BACK), el composer queda sin bordes hasta el próximo arranque de braze.

**Impacto**: regresión visual introducida por el WIP. Snapshot tests no cubren el composer vivo.

**Arreglo**: extraer `fn bordered_composer()` y llamar en las 3 recreaciones (`new`, `submit`, `backtrack_to`). Snapshot test de un composer con bordes en un dummy test.

### H-9 [MEDIA][NEW] — `KNOWN_OVERRIDE_KEYS` omiten 5 sampling keys

**Evidencia**: `file.rs:13-38` lista 24 claves pero no `ollama_temperature`, `ollama_seed`, `ollama_top_p`, `ollama_top_k`, `ollama_repeat_penalty`. Las 5 están correctamente definidas en `overrides.rs:36-44` y parsean de `BRAZE_OLLAMA_*` (overrides.rs:127-181), se aplican (config.rs:374-388). Pero si aparecen en `config.json`, disparan `tracing::warn!("unrecognized config file key; ignored")` (file.rs:73). Los valores SÍ se aplican correctamente, pero el mensaje contradice — usuario piensa que no surtieron efecto.

**Impacto**: UX. Inconsistencia introducida en commit de sampling knobs (ítem 7 backlog 2026-07-06) que no se propagó a `file.rs`.

**Arreglo**: añadir 5 claves a `KNOWN_OVERRIDE_KEYS`. Test que no dispara warning.

### H-10 [MEDIA][NEW] — `edit_file` fuzzy destruye CRLF→LF fuera de la ventana editada

**Evidencia**: `replace_line_window` (edit_file.rs:285, 309) reconstruye el archivo con `original.lines().collect()` (que strippa `\r`) y `out.join("\n")`. Rungs 2 y 3 fuzzy pierden todos los `\r` — incluidas las líneas fuera de la ventana editada. Rung 1 (exacto, `replacen`) preserva bytes. No hay test CRLF (todos los tests existentes usan `\n` puro).

**Impacto**: edición fuzzy sobre archivos Windows-style corrompe line endings en regiones untouched. Silencioso.

**Arreglo**: detectar `original.contains("\r\n")` y rehacer join con `\r\n`. O rehusar fuzzy y steering al rung 1.

### H-11 [MEDIA][NEW] — `edit_file` OOM en archivos muy grandes

**Evidencia**: `edit_file.rs:69` usa `tokio::fs::read_to_string`. Sin budget ni streaming. Para logs/datasets multi-GB se cae por OOM o UTF-8. Mismo patrón que v4 P2.1 para `read_file` pero no estaba flagueado para edit.

**Impacto**: el mismo archivo grande que `read_file` pagina correctamente crashea vía `edit_file`.

**Arreglo**: stats previas via `tokio::fs::metadata`; reject > N MB con steering a `shell_exec sed`/`grep -n`. O streaming edit por byte-zonas.

### H-12 [MEDIA][NEW] — `kill_on_drop` no mata grandchildren

**Evidencia**: `shell_exec.rs` usa `kill_on_drop(true)` pero sólo mata child directo. `sh -c "sleep 100 &"` deja `sleep` vivo (grandchild). El test actual usa `sleep 1` como child directo.

**Impacto**: proceso zombie si el comando hace fork.

**Arreglo**: usar `Command::process_group(0)` + kill PG vía `libc::kill(-pgid, SIGKILL)` en Unix. Complicado en stable — si no viable, documentar limitación.

### H-13 [MEDIA][NEW] — `top_p`/`top_k`/`repeat_penalty` aún sólo a Ollama (v3 F8 sin cerrar)

**Evidencia**: `backend_spec.rs:346-406` — Spec `.build()` aplica `top_p/top_k/repeat_penalty` sólo para Ollama (backend_spec.rs:367-375); para Anthropic/OpenRouter las ignora (backend_spec.rs:346-353, 397-406). El doc comment en :540-543 lo acknoledge: "Ignored (no warning — uniformity across a mixed sweep isn't achievable here)".

**Impacto**: comparar `ollama:qwen2.5:3b` con knobs recomendados contra `openrouter:anthropic/X` está desbalanceado en 3 dimensiones no-flagged. Abation cross-backend sesgada.

**Arreglo**: documentar prominentemente que estos 3 parámetros no viajan a Anthropic native (la API no los expone) — OK; pero añadir warning en el bench cuando una suite los especifica para un backend no-Ollama, en vez de silenciar. Alternativamente, Anthropic/OpenRouter los aceptan via mapping a temperature (pérdida de info). Decisión de diseño.

### H-14 [MEDIA][NEW] — Loops de escalación sin test "lead also failing"

**Evidencia**: `escalation.rs` — worker floundering re-escalation: si el streak se mantiene, `escalated_remaining=0` → payload re-detecta → nuevo `LeadEscalating` → nuevos `escalation_turns-1`. Sí teóricamente loop si el lead también falla (no hay corte "lead also failed → abort"). Test actual cubre hasta `LeadEscalated` activación, no el caso de lead que falla y repite.

**Impacto**: un lead con fallos sostenidos puede re-escalar repeatedly sin nunca escalating a abort. Evita el circuit-breaker hard.

**Arreglo**: corte tipo "lead failing N veces en un row → abort con respuesta honesta". Test: mock donde lead siempre falla, assert abort tras N re-escalamientos.

### H-15 [MEDIA][NEW] — `Timeout` no aborta explícito Ollama, contención RAM en Nitro

**Evidencia**: `runner.rs:220-232` — `tokio::time::timeout` dropea el future, lo que dropa `Engine::run_turn` mid-flight. Pero el stream Ollama sigue corriendo server-side hasta que la conexión aborta, y con `--no-ollama-stop` el modelo no se libera hasta `stop_ollama_model` (main.rs:247). Sequentials timeouts contienen RAM en Nitro contiguously.

**Impacto**: sweeps en Nitro con tasks largas + timeouts crescentes → contención de RAM, acá ya observado en benchmarks previos.

**Arreglo**: tras timeout, notificar explícito a Ollama grab/stop. Exponer `cancel_ongoing` hook en `ModelBackend`.

### H-16 [MEDIA][NEW] — Cache breakpoints sin límite de 4 (riesgo Anthropic)

**Evidencia**: `openrouter_wire.rs:269-281` marca 3 breakpoints (last tool, system, last message). Anthropic API permite max 4 cache_control markers por request. `apply_cache_breakpoints` no limita.

**Riesgo**: si en el futuro se añaden más breakpoints (historial checkpoint, tools-early), request podría exceder y Anthropic devuelve 400. No hay test del caso ">4".

**Arreglo**: cap a 4 (ó 3 con headroom) con TODO comentado. Test aserce.

### H-17 [MEDIA][NEW] — Metadata no serializa ablation overrides activos como field estructurado

**Evidencia**: `metadata.rs:15-35` sólo guarda `SamplingSpec` y `repetitions`/`task_timeout`/`suite_path`/`fingerprint`/`commit`/`ollama_model_digests`. Un override como `+ablate:no-rescue` queda sólo en el `display_name` del `backend` field — no en `metadata.active_ablations`.

**Impacto**: análisis de JSONs consolidados requiere re-parse del nombre. Reproducibilidad débil si el nombre seactualiza.

**Arreglo**: `RunMetadata.active_ablations: Vec<String>` field serializable. Tests de paridad.

### H-18 [MEDIA][NEW] — Anthropic-native cache tokens NO capturados

**Evidencia**: `anthropic_wire.rs:324-332` comentario explícito: cache tokens for Anthropic-native path are `None`. Algunos modelos Anthropic exponen `usage.cache_read_input_tokens` y `cache_creation_input_tokens`, pero el path Anthropic-direct no genera los equivalentes CompletionEvent::Usage cache_read/write.

**Impacto**: una sessión con `braze chat --backend anthropic` no reporta cache tokens, aunque sí los hay. WIP cerró OpenRouter pero no Anthropic-native.

**Arreglo**: añadir parseo de `usage.cache_read_input_tokens`/`cache_creation_input_tokens` en `anthropic_wire.rs:328-332` y mapear a las nuevas `cache_read_tokens`/`cache_write_tokens`. Tests con fixture de response real.

### H-19 [MEDIA][NEW] — Sin retry/backoff en 429 (HTTP rate limit)

**Evidencia**: `http_error.rs:22-23` mapea status 429 a `ModelError::RateLimited`, propagated eventualmente a `Engine::run_turn`. No hay retry con jitter. `http_client.rs:33-49` no configura retries.

**Impacto**: flautitaciones transitorias en Anthropic/OpenRouter abortan el turno. Para SLM-first usando Ollama es correcto no-reintentar (sobrecargar backend no ayuda), pero para cloud backends un backoff capado mejoraría fiabilidad.

**Arreglo**: política configurable (p.ej. `max_retries: 3`, jittered backoff, Retry-After header respetado) en backends cloud. Opt-in para Ollama (default off).

### H-20 [BAJA][NEW] — `print_banner` no chequea `isatty`

**Evidencia**: `lib.rs:87` llama `print_banner` antes que `terminal::setup()`. Si `braze chat --tui` con stdout redirigido (p.ej. `braze chat --tui | tee`), banner escribe ANSI escapes a un pipe.

**Impacto**: cosmetic. El siguiente `terminal::setup()` fallaría/clampearía en raw mode si no es TTY, y banner es "ruido" previo.

**Arreglo**: `if std::io::stdout().is_terminal() { print_banner(...) }`. O error claro early-exit si no es TTY.

### H-21 [BAJA][NEW] — Spinner `tokio::time::interval` con `MissedTickBehavior::Burst` por default

**Evidencia**: `app.rs:334` crea `tokio::time::interval(SPINNER_FRAME_DURATION)` sin `set_missed_tick_behavior`. Comentario :329-333 afirma "no ticks accumulate" — pero sólo aplica el guard el `select!`; mientras `turn_running || switching_model` es true, branches lentas (`update_rx.recv()` que blockea >80ms) acumulan ticks; al desbloquearse, se disparan en ráfaga y `spinner_frame` avanza several posiciones seguidas → animación "apresurada" tras pausa.

**Impacto**: cosmetic.

**Arreglo**: `spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);` en 1 línea.

### H-22 [BAJA][NEW] — `read_file` sobre archivo binario produce error críptico

**Evidencia**: `read_file` produce "failed to read '...': stream did not contain valid UTF-8". v1 D11 ya flagueado; sigue. No distingue "binario" de "I/O error".

**Arreglo**: catch `ErrorKind::InvalidData` con mensaje accionable ("binary file; use shell_exec xxd").

### H-23 [BAJA][NEW] — `post_edit_check` filter `line.contains("error")` false-positives

**Evidencia**: `post_edit_check.rs:75` filtra warnings cuyo path/texto contiene "error" en cualquier posición. False-positive minor.

**Arreglo**: filtrar por prefix `error:` o usar `--message-format=json` con `reason=compiler-message` y `level=error`.

### H-24 [BAJA][NEW] — `config.rs` tests no cubren `enable_prompt_caching` default/override

**Evidencia**: WIP añade el campo + 2 tests en `overrides.rs` env parsing, pero `config.rs` tests no aserton `enable_prompt_caching == true` en `defaults_without_file_or_env` (el único campo con `#[serde(default = "default_true")]`). No hay clón de `best_of_n_is_overridable_via_env` para `ENABLE_PROMPT_CACHING`.

**Arreglo**: copiar el patrón de test existente. 2 tests nuevos.

### H-25 [BAJA][NEW] — `prompt_cfg chars cast as u32` potencial truncation

**Evidencia**: `prompt.rs:171-173` — `prompt_side_chars as u32`. Resto del código usa `saturating_*`. Patológicamente imposible (un system prompt >4GB), pero contradictor con la convención.

**Arreglo**: `u32::try_from(...).unwrap_or(u32::MAX)`.

## RE-CONFIRM de v3/v4 — siguen ABIERTOS con evidencia file:line actualizada

Se verificó que estos hallazgos de v3/v4 siguen sin resolver:

| ID v3/v4 | Estado | Evidencia actual | Severidad |
|---|---|---|---|
| v4 P0.2 TurnBudget duro | ABIERTO | `MAX_TURN_ITERATIONS=20` (engine.rs:31), sin coste/tokens/walltime hardcap. `Usage` no detiene. | CRÍTICA |
| v4 P0.3 write_file destructivo post-hoc | ABIERTO | `SHRINK_WARNING_THRESHOLD_BYTES=500` (write_file.rs:22), warning dispara post-escritura (write_file.rs:50-62). No hay `allow_shrink`, `expected_previous_sha256`, ni preflight. | CRÍTICA |
| v4 P0.4 suite self_improvement / SI-2 como bench permanente | ABIERTO | `task.rs` sólo asserts de temperatura/seed, no `expect_max_rounds`/`tokens`/`cost`. SI-2 no está como TaskDef en una suite. | CRÍTICA |
| v4 P1.1 `engine.rs` 7655 líneas troppo grande | ABIERTO | 7655 líneas confirmadas. Mismo conjunto de responsabilidades que v4 midió. | ALTA |
| v4 P1.2 ModelFamily compartido | PARCIAL | `prompt.rs` enum `ModelFamily { QwenTagged, Qwen3CoderXml, Generic }` (prompt.rs:31-40) está pero sólo Qwen recibe hint non-None. Otros SLMs caen a Generic → None. Rescues en engine.rs despacho declarativo. | ALTA |
| v4 P1.4 best_of_n secuencial | ABIERTO | `complete_with_best_of_n` (engine.rs:605) itera candidatos en bucle secuencial. No hay `best_of_n_concurrency`. CLI advierte con Ollama. | ALTA |
| v4 P1.5 ToolCatalog por ronda | ABIERTO | `ToolRegistry::all_stubs_lossy()` reconstruye cada r; dispatch re-resuelve provider/schema. No hay snapshot explícito. | ALTA |
| v4 P1.6 post-edit check no-Rust | ABIERTO | `post_edit_check.rs:41`: `if Path::new(path).extension().is_none_or(|ext| ext != "rs")`. Sólo .rs en cargo project. | ALTA |
| v4 P2.1 read_file streaming | ABIERTO | `read_file.rs` carga archivo completo en `tokio::fs::read_to_string` (paginación correcta en la page devuelta, pero in-memory). | BAJA |
| v4 P2.2 Config perfiles SLM-first | ABIERTO | Config monocapa, sin profiles. Default `llama3.1`/4096 sigue disparando warning. | BAJA |
| v4 P2.3 MCP taxonomía fina | ABIERTO | `classifier.rs:122` — `McpToolCall` siempre Irreversible. Sin hints `readOnlyHint`. | MEDIA |
| v4 P2.4 límites hardcodeados | ABIERTO | `MAX_TURN_ITERATIONS=20`, `TOOL_COMPLETION_TIMEOUT=120s`, `PLANNER_MAX_TOKENS=1024` (engine.rs:31, 37, 74) hardcodeados. No expuestos en CLI/config. | BAJA |
| v3 Grupo O (A2) write_file destructive warning | RE-CONFIRM v4 P0.3 | abierto. | — |
| v3 Grupo O (A3) error matching sin líneas cercanas | ABIERTO | `edit_file.rs` error paths no muestran contexto lineal cercano. | MEDIA |
| v3 Grupo O (A4) edit_file sobre inexistente sin steering a write_file | ABIERTO (parcial) | la escalera de matching (commit `baab38b`) steering a write_file NO aplica al caso "file not found" de edit_file, sólo al "matching failed". | BAJA |
| v3 Grupo P (B1) cap agregado 5 observaciones | PARCIAL | full_observations calibrado por budget (U-17) ahora; el cap default 8KB escala a 80KB sin context_budget. B1 original pedía "cap agregado sobre la cola de 5 observaciones" — el cap existe y escala, pero sigue siendo 5 observaciones full (no reducido por `num_ctx`-awareness). | MEDIA |
| v3 Grupo P (B2) estimador mide táctica en crudo | ABIERTO | `estimate_prompt_tokens` (engine.rs:2873) no mide en forma colapsada. Sobre-compacta prematuramente. | MEDIA |
| v3 Grupo P (B4) chars/4 subestima ~20% | ABIERTO | `estimate_message_tokens` (engine.rs:2895) sigue chars/4. | BAJA |
| v3 Grupo P (B5) `max_tokens=4096` sobre `num_ctx=8192` | ABIERTO | config.rs default 4096, validate() warning-only (config.rs:334-345). | BAJA |
| v3 Grupo Q (D1) cero adaptación prompt/formato por familia proactive | PARCIAL | `prompt.rs` ModelFamily enum + hints Qwen; GLM/Llama/Mistral caen a Generic → None. Rescues en engine.rs son reactivos. | ALTA |
| v3 Grupo Q (D2) sampling knobs cableados SOLO al bench | ABIERTO | `braze chat` clavado en `DEFAULT_TEMPERATURE=0.2` (engine.rs). Knobs top_p/top_k/repeat_penalty sólo via `--top-p`/etc en braze-bench, no en chat. | ALTA |
| v3 Grupo Q (F2) coerción tipos XML qwen3-coder | ABIERTO | `parse_function_xml_tool_call` (engine.rs:2200) no coerciona tipos — fallo sistemático con qwen3.5-coder (mejor modelo local). | ALTA |
| v3 Grupo R (F3) guardrail post-check deja is_error:false; escalación no cuenta | ABIERTO | `post_edit_check.rs` feedback lleva `is_error:false`; `EscalatingBackend` sólo cuenta `is_error:true` → lead nunca vuelve en flounder de edición. Palancas se anulan. | ALTA |
| v3 Grupo R (D3) escalación no distingue falla-modelo vs entorno | ABIERTO (deliberado) | `escalation.rs:255-261` doc comment lo acknowledge; "deliberadamente narrow". D3 marcó como decisión. | MEDIA |
| v3 Grupo S (E1) ablation infra en bench | PARCIAL | `+ablate:` parser existe (backend_spec.rs admite `+ablate:full-observations=N`, `+ablate:tactical-threshold=N`). NO cubre `no-rescue`, `no-planner`, `no-lead`, `no-compaction`, `no-preflight` (v4 Fase 5 las pide). | ALTA |
| v3 Grupo S (E2) baseline externo mini-swe-agent | ABIERTO | `external.rs` trait `ExternalHarness` + `external_outcome_to_task_result` definidos y tested, pero `#[allow(dead_code)]`, no hay CLI `--external` flag, ni implementador real. | BAJA |
| v3 Grupo S (E3) suite ampliada | PARCIAL | `task.rs` asserts `>=18` tasks, n>=3 por skill (salvo single_tool). Edits sólo 2. Poder estadístico borderline con rep=5. | BAJA |
| v3 Grupo S (E5) tradeoff costo/calidad por skill | ABIERTO | No hay coste estimado (v4 P0.2 + H-3). Sin coste, tradeoff no se reporta. | MEDIA |

## Tabla consolidada del backlog ABIERTO por prioridad

Incluye v3 (O–S) + v4 (P0–P2) + v5 NEW (H-1 – H-25). Totales: **41 ítems abiertos**.

### CRÍTICA (3)

| ID | Tema | Fuente |
|---|---|---|
| H-1 | Cache tokens no agregados a `TaskResult` | v5 NEW (WIP incompleto) |
| v4 P0.2 | TurnBudget duro (rondas, tokens, coste, walltime, tool calls) | v4 |
| v4 P0.3 | `write_file` destructivo sin preflight | v4 + v3 A2 |

### ALTA (14)

| ID | Tema | Fuente |
|---|---|---|
| H-2 | `BackendSpec::build` no invoca `with_prompt_caching_enabled` en bench | v5 NEW |
| H-3 | Métricas: rescates, escalaciones, compaction count, summary fallbacks no trackeados | v5 NEW |
| H-4 | `attempt_tools_free_summary_round` dropea `Usage` (acknowledged) | v5 NEW + v4 P1.3 |
| H-5 | `shell_exec` sin timeout de pared | v5 NEW |
| H-6 | `env` solo filtra API keys al contexto | v5 NEW |
| H-7 | `durable_events` crece sin cota | v5 NEW |
| v4 P0.4 | Suite self_improvement / SI-2 como bench permanente | v4 |
| v4 P1.1 | `engine.rs` 7655 líneas demasiado grande | v4 |
| v4 P1.2 | ModelFamily compartido unificado | v4 (parcial) |
| v4 P1.4 | `best_of_n` secuencial multiplica latencia | v4 |
| v4 P1.5 | ToolCatalog snapshot por ronda | v4 |
| v4 P1.6 | post-edit check Rust-only | v4 |
| v3 D1 | Cero adaptación proactiva prompt/formato por familia | v3 (parcial) |
| v3 D2 | Sampling knobs cableados solo al bench | v3 |
| v3 F2 | Coerción tipos XML qwen3-coder | v3 |
| v3 F3 | Guardrail post-check is_error:false, escalación no cuenta | v3 |
| v3 D4 | best_of_n × EscalatingBackend interactúan mal | v3 (triple corroboración) |
| v3 E1 | Abation infra en bench sin knobs `no-rescue`/`no-planner`/etc | v3 (parcial) |

### MEDIA (18)

H-8, H-9, H-10, H-11, H-12, H-13, H-14, H-15, H-16, H-17, H-18, H-19, v4 P2.3, v3 A3, v3 B1 (parcial), v3 B2, v3 D3 (deliberado), v3 E5.

### BAJA (14)

H-20, H-21, H-22, H-23, H-24, H-25, v4 P2.1, v4 P2.2, v4 P2.4, v3 A4, v3 B4, v3 B5, v3 E2, v3 E3.

## Roadmap actualizado (paquetes de trabajo)

Los 3 paquetes de v4 siguen siendo la estructura correcta, con añadidos de v5:

### Paquete 1 — Medición y harness (desbloquea el resto)

Orden dentro del paquete:

1. **H-1 cache tokens en TaskResult** (CRÍTICA, WIP 90% hecho) — finish-line del WIP actual.
2. **v4 P0.4 SI-2 como benchmark permanente + suite self_improvement** — ya tiene `+lead:` (d89b134), falta TaskDef con `expect_max_rounds`/`tokens`/`cost` y la suite.
3. **H-3 métricas rescates/escalaciones/compaction/summary** — añadir AgentEvent variants + folds en metrics.rs.
4. **H-2 `+ablate:no-caching`** en bench — para A/B paper prompt caching.
5. **H-18 Anthropic-native cache tokens** — completar caching cross-backend.
6. Matriz executor solo / +planner / +lead / +ambos (con métricas H-3 + cache H-1) — primeira publicación de resultados.

### Paquete 2 — Seguridad para auto-mejora

1. **v4 P0.3 preflight write_file** (`allow_shrink` / `expected_previous_sha256`).
2. **v4 P0.2 TurnBudget** (max_rounds, max_tokens, max_cost, max_walltime, max_tool_calls, max_repeated).
3. **H-4 summary fallback Usage + max_tokens** limiting.
4. **H-5 shell_exec timeout de pared**.
5. **H-6 quitar `env` del allowlist Reversible**.
6. **H-7 cap durable_events** (clear_tool_uses style).
7. Checkpoint automático antes de escrituras grandes.
8. Validadores repo-level configurables (P1.6 generalización).

### Paquete 3 — Especialización por familias

1. **v4 P1.2 ModelFamily** compartido (Qwen, Qwen3Coder, GLM, Llama, Mistral, Generic) — unifica prompt.rs + engine.rs rescues.
2. **v3 F2 coerción tipos XML** qwen3-coder.
3. **v3 D2 sampling knobs a producción** (`braze chat` no sólamente bench).
4. **v3 F3 guardrail post-edit is_error:false cuentas**.
5. **H-13 documentar sampling parity cross-backend** (top_p/top_k/repeat_penalty differences).
6. Perfiles SLM-first config (`small-local-coding`, `bench-slm`, `cloud-leader`).
7. `MCP readOnlyHint` (P2.3 + H-7 equipo).

### Paquete 4 — Refactor arquitectura

Prioridad **después** del Paquete 1 (medición) para detectar regresiones:

1. Extraer `completion.rs`, `rescue.rs`, `dispatch.rs`, `budget.rs`, `planning.rs`, `memory.rs` de engine.rs (v4 P1.1).
2. `ToolCatalog` snapshot por ronda (v4 P1.5).
3. Mover tests grandes a módulos por responsabilidad.
4. Íntegro con `clear_tool_uses` del Paquete 2 (#6).

### Paquete 5 — Investigación y paper trail

1. `+ablate:no-rescue`/`no-planner`/`no-lead`/`no-compaction`/`no-preflight` (v3 E1 completar).
2. Mini-swe-agent como baseline externo (v3 E2 — adapter sobre `ExternalHarness` ya definido).
3. Suite ampliada (v3 E3 — más edits tasks, rep >=10).
4. Curvas coste/pass-rate y rondas/pass-rate.
5. Ablations cruzadas con familias (Paquete 3).
6. Registro de fallos cualitativos.
7. Reporte reproducible por commit (`RunMetadata.active_ablations` H-17).

## Cambios sugeridos por archivo (cortocircuito top-10)

| Archivo | Cambio | Origen |
|---|---|---|
| `crates/braze-bench/src/metrics.rs` | Añadir cache tokens, rescates, escalaciones, compaction, summary fallbacks a `TaskResult` | H-1, H-3 |
| `crates/braze-bench/src/backend_spec.rs` | `with_prompt_caching_enabled` respeta `+ablate:no-caching` | H-2 |
| `crates/braze-tools-local/src/write_file.rs` | Preflight destructivo: `allow_shrink`/`expected_previous_sha256` | v4 P0.3 |
| `crates/braze-engine/src/engine.rs` | `TurnBudget` + summary Usage + cap durable_events (vía AgentEvent variants) | v4 P0.2, H-4, H-7, H-3 |
| `crates/braze-tools-local/src/shell_exec.rs` | `tokio::time::timeout` configurable | H-5 |
| `crates/braze-permissions/src/classifier.rs` | Quitar `env` de safe_readonly | H-6 |
| `crates/braze-config/src/file.rs` | Añadir 5 sampling keys a `KNOWN_OVERRIDE_KEYS` | H-9 |
| `crates/braze-tui/src/app.rs` | `fn bordered_composer()` + 3 recreaciones | H-8 |
| `crates/braze-model/src/anthropic_wire.rs` | Parseo `cache_read_input_tokens`/`cache_creation_input_tokens` | H-18 |
| `crates/braze-engine/src/engine.rs` | Refactor modular (Paquete 4, post-medición) | v4 P1.1 |

## Criterios de salida para la próxima versión fuerte

Una versión candidata debería cumplir (extiende v4):

- `cargo test --workspace` pasa.
- `cargo clippy --workspace --all-targets -- -D warnings` pasa.
- `TaskResult` reporta cache tokens, rescates, escalaciones, compaction count, summary fallbacks.
- `braze-bench` acepta `+ablate:no-caching`.
- Anthropic-native reporta cache tokens.
- Suite self_improvement con SI-2 como TaskDef permanente.
- Existe matriz publicada comparando modelos pequeños con/sin líder y con/sin caching.
- `write_file` no reduce drásticamente archivo existente sin confirmación explícita.
- `TurnBudget` configurable por turno (rondas, tokens, coste estimado).
- `shell_exec` tiene timeout configurable.
- `env` solo no es auto-aprobado.
- `durable_events` tiene cota.
- `engine.rs` reducido a <3000 líneas con módulos extraídos (ó, alternativamente, archiva este criterio como diferible si medición muestra que SLMs editan bien el monolito).
- `ModelFamily` compartido con al menos 1 familia no-Qwen (GLM o Llama) con hints + rescue ordering.

## Conclusión

**braze está más cerca de "mejor harness SLM-first"** que en v4: `+lead:` en bench cierra P0.1, cache tokens están en 2/3 del stack, prompt caching OpenRouter implementado y testeado, rescate GLM arg-tags + strip leaked tool-calls + compaction calibrada cierran hallazgos U-15/16/17/18 del log SI-2, y read_file clamp al budget cierra U-6.

**Las 3 CRÍTICAS restantes son work terminal del WIP actual** (H-1 cache tokens en metrics.rs), gobernanza de ejecución (v4 P0.2 TurnBudget) y seguridad de escrituras (v4 P0.3 preflight write_file). Las 14 ALTA son mayormente medición (H-3 métricas de palancas SLM, H-2 ablation de caching) y seguridad (H-5 timeout, H-6 env leak, H-7 durable_events growth) — no son conceptualmente difíciles, todas con arreglos claros.

**El refactor de `engine.rs` (v4 P1.1) sigue siendo el ítem estructural más caro pero debe ir DESPUÉS de medición** — sin métricas de rescates/escalaciones/compactiones, reorganizar 7655 líneas sin detectar regresiones en los modos de falla centrales es arriesgado. v4 ya lo decía; v5 lo confirma tras rechequear la complejidad funcional del archivo.

**Recomendación: commitear el WIP actual primero** (una vez H-1 cerrado) — 1608 insertions en 20 archivos sin commitear es demasiado surface para revisión segura. Squash a un commit coherente "Close v5 H-1: aggregate cache tokens in TaskResult; resto del log SI-2 (prompt caching, GLM rescue, strip leaked, compaction budget, read_file clamp, spinner)" o partir en 3-4 commits temáticos.