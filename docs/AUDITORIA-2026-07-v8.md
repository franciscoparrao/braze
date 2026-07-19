# Auditoría 2026-07 v8 — auditoría integral + delta de literatura "harness de frontera"

**Fecha**: 2026-07-18. **Base**: main `fedbc3e` + worktree sucio (circuit
breaker, suite memory-distillation ampliada, `braze run --output-format
json`, CLAUDE.md). **Método**: 6 agentes en paralelo — 4 de auditoría de
código (engine core, capa de modelos, tools/sesión/memoria/permisos,
bench/TUI/CLI) + 2 de investigación web (literatura de harnesses,
update Gemma4). Cada agente leyó la v7 primero para no duplicar; los 3
hallazgos de mayor impacto se verificaron a mano contra el código antes
de escribir este doc. Los agentes de código corrieron `cargo
test`/`clippy` con los diffs aplicados: **verdes** (154 tests
braze-model, 151 braze-bench, 14 braze-cli).

**Numeración**: serie K unificada (dos agentes usaron "K" en paralelo
con colisiones; aquí están renumerados y deduplicados).

**Estado de aplicación (2026-07-18, cierre del día — 14 commits en
main)**: TODO el roadmap ejecutable de esta auditoría quedó aplicado el
mismo día:

- **Paquete 0**: K-1 (breaker rediseñado completo, kill-switch
  `BRAZE_CIRCUIT_BREAKER=off`, `CircuitOpen`→`HarnessError`) y K-11
  (`OLLAMA_HOST`, también en el worktree bfcl-anchor); sweep BFCL
  lanzado y corrido el mismo día.
- **Paquete 1** (`3164e93`): K-6, K-7, K-8 (canal+task+`flush()`),
  K-9 (`expect_cargo_check`) y K-10 (fixtures del grading).
- **Paquete 2** (`200a020`): K-2, K-3 (4 capas), K-4, K-5. **J-20:
  aceptación MVP RATIFICADA por el autor el 2026-07-18** — la variante
  de escritura vía symlink queda como riesgo aceptado del MVP,
  reevaluar con Landlock (Fase 2).
- **Paquete 3 / P1.1** (`583c6f6`, `311ffe1`, `3313734`): split de
  producción completo en 9 módulos según § 3; pendiente solo el reparto
  del `mod tests`.
- **Paquete 4, top-6 S/M**: pass^k (`8279d3e`), prompt caching
  Anthropic (`806231d`), summary-por-lead (`c47c478`), TTC local
  (`908348b`), McNemar+Holm K-19 (`69681b8`), y `braze run
  --output-format json` (`d6cdfdd`).

Quedan de esta auditoría: los ítems L del Paquete 4 (Landlock,
Viewer/Editor, background trans-ronda), K-16 (negative-cache MCP),
K-17, la cola P2 menor, y el reparto del `mod tests`. Workspace al
cierre: ~1.008 tests, clippy `-D warnings` limpio.

## Veredicto ejecutivo

1. **Los cierres de v7 cierran de verdad**: 0 regresiones, 0 parciales
   (J-1/J-2 en `escalation.rs:311-390`, J-3/J-4 en `engine.rs:1201-1202`,
   J-13, J-18 con tests de regresión, J-19 en la seam correcta).
2. **El circuit breaker sin commitear NO está listo** (K-1): bien
   escrito mecánicamente, pero calibrado contra un modo de fallo que el
   proyecto no sufre — y con riesgo activo de contaminar sweeps
   multi-brazo. Tres agentes convergieron en esto de forma independiente.
   No correr sweeps de paper con ese diff aplicado.
3. **Una línea bloquea el pendiente #1** (K-11): `ollama stop` nunca
   apunta a Nitro — la higiene de memoria de la que depende
   `bfcl-anchor-RESUME.md` es un no-op remoto y el OOM (~22 GB vs 16 GB)
   puede repetirse.
4. **ProjectMemory (Paper 2) se está contaminando en su fuente** antes
   de generar evidencia: inyección de prompt persistente (K-3),
   señales duplicadas (K-5), grading gameable y sobreajustado a
   gpt-oss:20b (K-9/K-10).
5. **P1.1 se agravó de nuevo**: engine.rs +1.245 líneas desde v7
   (11.382 hoy; producción ~4.230, tests ~7.150). Hay plan de split
   concreto abajo con fronteras observadas en el código real.
6. **La tesis del proyecto ahora tiene nombre de disciplina y segunda
   cita cuantitativa**: "harness engineering" (Fowler, Anthropic,
   estudio empírico arXiv 2602.14690) y +13.7pp solo-harness en
   Terminal-Bench 2.0 (Trivedy 2026). La literatura además valida dos
   decisiones de diseño de braze (rescate textual vs constrained
   decoding — "Constraint Tax"; diferir multi-agente genérico) y marca
   el norte del lead/worker (SWE-Protégé: escalación aprendida, 7B a
   42.4% en SWE-bench Verified con 11% de tokens de experto).

## 1. Veredictos sobre el trabajo sin commitear

| Diff | Qué es | ¿Listo para commit? |
|---|---|---|
| `braze-model/*` (circuit_breaker.rs + wiring) | Breaker tri-estado por destino, registro global por proceso | **NO** — bloquear por K-1 (a-f). Con los fixes, sí vale |
| `braze-cli/{cli_args,main}.rs` | `braze run --output-format json` (observer acumulador, JSON único al final) | **Sí** — diseño limpio, 2 tests, default `plain` intacto. Mejora opcional K-23 |
| `braze-bench/src/task.rs` | Ajuste de conteos de test por la suite ampliada | **Sí** — trivial |
| `braze-bench/suites/memory-distillation.toml` | +4 tareas (par B-loop E0502, par B-move E0382), bitácora de 3 revisiones en comentarios | **Funcional para pilotos diagnósticos** con spot-check manual; **no publicable** hasta K-9/K-10 |
| `CLAUDE.md` | Referencia a wiki | Sí |

## 2. Hallazgos nuevos — serie K

### P0

**K-1 [P0][PAPER] — Circuit breaker: no commitear tal como está.**
Convergencia independiente de 3 agentes. Es limpio en lo mecánico
(`std::sync::Mutex` nunca cruza await, poison recuperado, transición
Open→HalfOpen atómica, 8 tests) pero:

- **(a) La ventana de 60s lo hace matemáticamente incapaz de abrirse
  con fallos lentos** — `circuit_breaker.rs:55` + `:120-124` exige 5
  muestras en 60s; una muestra fallida con retry cuesta ~43s
  (Anthropic/OpenRouter) y una conexión colgada 600s (`http_client.rs:27`).
  Solo abre con fallos instantáneos. Fix: fallos consecutivos sin
  ventana temporal (breaker clásico), o ventana ≥ read timeout.
- **(b) "Éxito" se registra al recibir headers, no al terminar el
  stream** (`circuit_breaker.rs:228-238`): los `StreamError` mid-stream
  — el ~2% de Nitro que motivó el breaker — se registran como ÉXITO.
- **(c) Cuenta 4xx determinísticos y la clave no incluye el modelo**
  (`ollama.rs:227`): el caso documentado `gemma3:1b` → HTTP 400
  instantáneo abre el breaker de TODO Nitro en 5 llamadas y el brazo
  siguiente del sweep arranca dentro del cooldown → **fallos del modelo
  B causados por el modelo A**. Fix: no contar 4xx≠429 (paridad con
  `retry.rs:65-66`) y/o clave `provider:url:modelo`.
- **(d) `CircuitOpen` se clasifica `ModelBackendError` en braze-bench**
  (`metrics.rs:547`): un outage de infraestructura se contabiliza como
  fallo del modelo — la contaminación exacta que H-19/N-37 evitan, pero
  fallando en 0ms. Necesita bucket propio excluible.
- **(e) Probe half-open reclamable a los 30s** (`circuit_breaker.rs:146`)
  vs inferencias CPU legítimas de 90-400s → probes concurrentes contra
  un backend recuperándose.
- **(f) Sin `tracing` en transiciones** (rompe la convención
  `RUST_LOG=braze_engine=info`), clave sin normalizar
  (`:11434` vs `:11434/` = dos breakers), constantes hardcoded sin
  opt-out, el warm-up J-6 no pasa por `guarded()` (un warm-up exitoso
  no cierra el breaker), y `EscalatingBackend` propaga `CircuitOpen`
  del worker en vez de intentar el lead (`escalation.rs:269`) — el
  failover que el breaker haría casi gratis.

### P1 — seguridad

**K-2 — `git diff --no-index` lee cualquier archivo del sistema sin
confirmación** — `classifier.rs:243-252` (verificado a mano).
`is_safe_git` solo bloquea `-o/--output/--ext-diff`; a diferencia de
`cat`/`ls`/`wc` (N-8b/J-18) no pasa por `all_path_like_args_allowed`.
`git diff --no-index /etc/shadow /dev/null` clasifica `Reversible`.
Fix S: path-check en args de `diff`/`log`/`show` o bloquear
`--no-index` (y `-O<orderfile>`).

**K-3 — Inyección de prompt persistente cross-sesión vía
ProjectMemory.** Tres piezas que componen mal: (i) `render.rs:51-84`
inyecta verbatim al system prompt texto escrito por el modelo
(`completed_signals` vía task list) sin `sanitize_control_chars` ni
neutralización de newlines — se pueden fabricar encabezados falsos;
(ii) `.braze/memory.json` vive en el workdir ⇒ el modelo puede
sobreescribirlo con `write_file` `Reversible` (sin confirmación) y
poblar `objective`/`notes`, que el render prioriza; (iii)
`project_key.rs:44-50` recomienda versionarlo junto al repo ⇒ clonar un
repo ajeno con `memory.json` pre-sembrado = inyección supply-chain del
system prompt. Mitiga hoy: `enable_project_memory` default off. Fixes
S/M: sanitizar por campo, enmarcar la sección como datos no-confiables,
tratar escrituras bajo `.braze/` como `Irreversible`, no renderizar
`objective`/`notes` hasta que un canal confiable los llene.

**K-4 — `ask_user` imprime pregunta/opciones del modelo crudas a
stdout** — `terminal_question.rs:50-59`. Misma clase que J-19 pero esa
superficie quedó fuera de la seam: ANSI en la pregunta puede repintar
el terminal o forjar visualmente un prompt de aprobación. Fix S: pasar
por `sanitize_control_chars` (ya es `pub`).

**K-5 — La reparación N-5 del rollout log escribe sin el flock N-27** —
`file_store.rs:259-271`. Un segundo proceso de solo-lectura (`braze
permissions suggest` hace `load` sobre TODAS las sesiones) que lea
mid-`write_all` interpreta la línea parcial como crash-artifact y
**trunca el archivo del proceso vivo**. Fix S/M: `try_lock_exclusive`
antes de reparar; si está tomado, tolerar en memoria.

### P1 — integridad del Paper 2 (ProjectMemory + bench)

**K-6 — Done→Done re-emite `TaskCompleted` y contamina
`completed_signals`** — `task_list.rs:98-113` + `memory.rs:141-159`.
Sin dedup y con cap de 30 (`remove(0)`), cada duplicado expulsa una
señal antigua distinta; un 3B re-marca "done" con frecuencia. El doc
del test (`task_list.rs:236-240`) dice lo contrario de lo que el test
asserta. **El fix de mejor ratio valor/esfuerzo para el Paper 2.** S.

**K-7 — `project_key` nunca se valida al cargar** — `memory.rs:75-80`
promete rechazo por mismatch; `store.rs:82-92` ni recibe el key y el
hook ignora su parámetro. Un `memory.json` ajeno se inyecta sin
verificación. Fix S: validar en `ProjectMemoryHook::new`.

**K-8 — `ProjectMemoryHook` hace I/O de disco bajo `HOOK_TIMEOUT` de
250ms** — `project_memory_hook.rs:149,163` + `hooks.rs:36`. (a) 3 saves
lentos → hook auto-desactivado, la captura muere en silencio; (b) el
timeout dropea el future pero el rename en vuelo puede aterrizar
DESPUÉS de un save posterior y regresar `memory.json`. El timeout se
diseñó para observadores puros; este hook es un escritor. Fix M: canal
+ task dedicada que serializa saves. (Relacionado: lost update
last-writer-wins entre sesiones concurrentes — `store.rs:94-125`; la
justificación "single writer" ya es falsa dentro del repo,
`runner.rs:285-291`. Fix M: flock + reload-merge.)

**K-9 — El grading de memory-distillation nunca verifica lo que el
prompt pide ("cargo check passes")** — `runner.rs:497-516` + suite.
Gameable (substring en comentario/código muerto pasa) Y con
sub-conteo probado (diagnóstico 2026-07-16: 4/4 fixes compilaban, 4/4
FAIL del grader). El sandbox ya ejecuta cargo (`is_benchable_cargo`,
`runner.rs:85-137`): `expect_cargo_check = true` post-run + test oculto
sería grading semántico real. Fix S/M.

**K-10 — Grader ajustado post-hoc sobre transcripciones del modelo bajo
prueba** — las revisiones 1-3 del TOML se calibraron mirando fixes de
gpt-oss:20b: cualquier comparación entre modelos queda sesgada a su
favor. Además el needle `"let mut owned_items = self.items;"` ya está
literal en el `setup_files` buggy (cero poder discriminante) y no hay
test en el repo que fije los needles contra fixtures del fix canónico.
Fix S: fixtures + test de grading.

### P1 — resto

**K-11 [bloquea pendiente #1] — `ollama stop` nunca apunta al nodo
remoto** — `braze-bench/src/main.rs:520-536` (verificado a mano): el
`Command` no setea `OLLAMA_HOST` desde `config.ollama_base_url`, así
que contra Nitro es un no-op salvo que el ambiente lo exporte.
`bfcl-anchor-RESUME.md` depende de ese stop para evitar el OOM que mató
el intento v1. **Fix de una línea**:
`.env("OLLAMA_HOST", &config.ollama_base_url)`. Aplicarlo (también en
el worktree bfcl-anchor) ANTES de correr el sweep BFCL.

**K-12 — Summary fallback Empty→Ok con vara laxa** —
`engine.rs:1551-1560`: termina Ok si `any_tool_calls_this_turn`, pero
el flag se setea aunque todas las calls hayan sido sintéticas fallidas
(bloqueo J-9, nudge, schema fail). Turno mudo reportado Ok. Fix S:
exigir ≥1 `ToolCallCompleted` con `is_error=false`.

**K-13 — `PlanCreated` de turnos anteriores se re-renderiza como
vigente** — `history.rs:406-414`: "…you have NOT executed any of it
yet" para planes ya ejecutados. Misma clase que J-3 (HarnessNote), no
aplicada a `PlanCreated`; invisible para el bench single-turn. Fix M
(mismo patrón que J-3).

**K-14 — J-17 sigue vivo en braze-cli** — `main.rs:611-619` calcula el
budget de contexto sobre `all_stubs_lossy()` pre-deferral (el bench ya
usa `initially_visible_stubs`, `runner.rs:348`). Con gateway de 1.500
tools → compactación prematura justo en el escenario que motivó la
deferral. Fix S: una línea.

### P2 (tabla)

| ID | Hallazgo | Ubicación | Esf. |
|---|---|---|---|
| K-15 | `shell_exec.timeout` promete 3600s pero el dispatch del engine corta a 120s con mensaje genérico — incoherencia ACI schema/realidad | `schema.rs:132-135` / engine | S |
| K-16 | Sin negative-cache ante server MCP muerto: cada ronda re-paga 60s de timeout; turno de 20 rondas ≈ 20 min de walltime perdido | `provider.rs:181-211` | S/M |
| K-17 | Clearing durable acota results pero clona verbatim `arguments` del tool_use (un `write_file` de 50KB queda íntegro para siempre) — faceta nueva de H-7 | `history.rs:482-521` | S/M |
| K-18 | `suite_fingerprint` usa `DefaultHasher` (inestable entre versiones de Rust) — rompe la reproducibilidad declarada | `metadata.rs:58-63` | S |
| K-19 | Wilson pooleado + seeds pareados sin explotar + cero manejo de comparaciones múltiples (4-5 brazos × 5-6 skills a α=0.05 ⇒ ≥1 falso positivo esperado por sweep) | `report.rs:119-166` | M |
| K-20 | Truncado de memoria silencioso en bench (`budget_lines` sin marcador ni flag) — mina para playbooks destilados largos | `memory.rs:126-141` (bench) | S |
| K-21 | `memory_condition` desconocida cae en sniffing heurístico en vez de fallar | `memory.rs:55-67` (bench) | S |
| K-22 | `braze run --output-format json`: en error a mitad de turno no emite JSON alguno y el session_id solo se conoce en éxito | `main.rs` (diff) | S |
| K-23 | Default `ollama_model = "llama3.1"` contradice la evidencia propia (18% pass-rate su clase) | `config.rs:557` | S |
| K-24 | Gate J-9: colisión de nombre visible/oculto bloquea la versión legítima; agrava J-28 | `engine.rs:2265` | S |
| K-25 | Summary fallback ignora `stop_reason` max_tokens — resumen cortado persiste como convergido (inconsistente con N-24) | `engine.rs:2149-2174` | S |
| K-26 | Gate J-9 con snapshot por ronda: `[search_tools, tool_activada]` misma ronda bloquea la segunda; cuesta una ronda | `engine.rs:1349/2265` | S |
| K-27 | Mensaje de schema-repair inyecta `input_schema` completo sin cota | `engine.rs:2462-2468` | S |
| K-28 | Sin `OLLAMA_HOST`… ver K-11; aquí: breaker sin kill-switch por config/env para A/Bs | breaker | S |
| K-29 | `collapsed_observation_content` mezcla bytes/chars ("N chars omitted" impreciso en UTF-8) | `history.rs:300-312` | S |
| K-30 | `+ablate:full-observations=0` rompe el invariante "observación más nueva siempre visible" | `history.rs:263-265` | S |
| K-31 | `TouchedFile`/hook persisten paths crudos del modelo relativos al cwd de esa sesión; dedup por string | `project_memory_hook.rs:146` | S |
| K-32 | Misceláneos braze-memory: `schema_version` jamás validado, save sin fsync pre-rename, timestamps mal documentados, heading colgante en render | braze-memory | S c/u |
| K-33 | Supuesto "un Usage por ronda" triplicado sin test de contrato compartido (metrics, JsonSummaryObserver, TUI status bar) | varios | S |
| K-34 | NDJSON: última línea sin `\n` al EOF con `done:true` ya recibido → `StreamError` espurio (teórico) | `ollama.rs:324-335` | S |
| K-35 | Docs: CLAUDE.md dice "14 crates" (son 15, falta braze-memory), "~900 tests", líneas de engine.rs desactualizadas | CLAUDE.md | S |

**Vigentes de v7 sin cambio**: J-9 (semántica), J-11, J-12, J-14/J-34,
J-20 (sin ratificar), J-25, J-26, J-28, J-31, H-7, I-6, I-7, P0.2
restante, P1.1 (agravado).

## 3. Plan de split de engine.rs (P1.1) — fronteras observadas

Producción: líneas 1-4232; `mod tests`: 4235-11382. Ninguna frontera
cruza `&self` de forma incómoda; los parsers son puros.

| Módulo destino | Contenido (líneas actuales) | ~Líneas |
|---|---|---|
| `engine/mod.rs` | struct + campos + `new` + builders `with_*` (1-614) | ~610 |
| `engine/turn.rs` | `run_turn` + `TurnGuard` + `TurnDispatchState` (1183-1686, 307-348) + skills (1718-1807) | ~660 |
| `engine/round.rs` | `complete_once{,_with}`, `complete_with_best_of_n`, `RoundUsage/Outcome` (745-1161, 3101-3138) | ~470 |
| `engine/dispatch.rs` | `dispatch_tool_calls` + `handle_task_tool_call` (2193-2724, 1817-1857) | ~575 |
| `engine/planner.rs` | `attempt_planning_round` + prompt + `count_numbered_steps` (1859-1987, 3940-3972) | ~200 |
| `engine/fallback.rs` | `attempt_tools_free_summary_round` + `strip_leaked_tool_call_shapes` (2009-2185, 3610-3650) | ~230 |
| `engine/context.rs` | load/repair/orphans/`pair_aware_tail_start`/`merge_summary` + estimadores + scaling (2736-3138, 3987-4232) | ~700 |
| `engine/hooks_dispatch.rs` | `append_and_notify` + `dispatch_hooks_*` (615-744) | ~130 |
| `src/rescue.rs` | la escalera completa: tagged/function-XML/GLM/pythonic/envelope/fences/coerce (3140-3609, 3652-3935) | ~800 |

Tests: repartir por módulo destino (parsers ~9876-11382 → `rescue.rs`;
scaling/budget 7939-8118 → `context.rs`; task/skills/hooks/deferral
8249-9600 → sus módulos; loop core → `turn.rs`). **Orden de ejecución**:
1º `rescue.rs` (puro, cero `&self`, cero async — riesgo nulo), 2º
`context.rs`, 3º `planner`/`fallback`/`dispatch`, 4º `turn`/`round`.
`cargo test --workspace` tras cada paso. Solo `synthesize_orphan_repairs`
e `initially_visible_stubs` son API pública; el resto `pub(crate)`.

## 4. Compactación y memoria — evaluación honesta vs frontera

**Compactación**: es diferencial de verdad (split durable/táctico,
pares tool_use/result íntegros, digest determinista, idempotencia) y el
resume es de lo más robusto del MVP (flock, reparación, replay de
permisos). Lo que pierde: (1) digest extractivo (15 palabras/request),
no semántico — dos compactaciones y queda casi ilegible; (2)
`MAX_SUMMARIES_KEPT=5` dropea historia vieja sin re-síntesis
jerárquica; (3) `durable_events` sin cota (H-7 + K-17); (4) resume
pierde skills (J-12) y task list. La palanca con más upside para SLMs:
**summary por lead** (reusa `EscalatingBackend`) + la rúbrica de
*cuándo* compactar de Self-Compacting Agents (§6).

**Memoria (braze-memory)**: V1 disciplinado — captura determinista
(coherente con no pedirle narrativa a un 3B), caps, write atómico,
budget de inyección, hook audit-only. Frente a frontera: sin memoria
curada (nada llena `objective`/`notes` y aun así se renderizan — vector
de K-3), sin recall selectivo (todo entra bajo budget fijo), señales
semánticamente débiles. Es un piloto correcto para el Paper 2, no una
memoria de frontera — y K-3/K-6/K-7/K-8 deben resolverse antes de
promover `enable_project_memory` o versionar `memory.json`.

## 5. Brechas consolidadas vs harness de frontera 2026

| Capacidad | Estado en braze | Esf. |
|---|---|---|
| Adaptación de formato al modelo (rescate por familia, colapso ACI) | **Al nivel de frontera** — y ahora con validación académica (Constraint Tax) | — |
| Tool deferral dos niveles + gate | Al nivel de frontera; restan K-24/J-28 | S |
| Planning medido con sweeps | Mejor que varios harnesses; falta replan intra-turno + K-13 | S/M |
| Escalación lead/worker | Proactiva por fase + reactiva; falta failover por error (`CircuitOpen`→lead) y el norte es escalación aprendida (SWE-Protégé) | S/M → L |
| Compactación con summary de calidad | Digest extractivo; summary-por-lead reusa infraestructura existente | S/M |
| Test-time compute local (N rollouts + selección) | `complete_with_best_of_n` existe a nivel ronda; falta a nivel tarea/turno con selección por torneo | M |
| Verificación en el loop | Guardrail cargo check ✓; falta rúbrica/check declarativo por tarea | S/M |
| Memoria entre sesiones | Piloto V1 (endurecer K-3/6/7/8; recall selectivo heurístico) | M |
| Subagentes contexto-angosto (Viewer/Editor) | Nada — pero la evidencia 2026 dice: SOLO esta variante, no orquestación genérica | L |
| Background trans-ronda con push | `TaskNotifier` es push real pero intra-ronda; falta ciclo de vida trans-ronda | M/L |
| Hooks mutantes (H2) / plugueables | Audit-only; el contrato de 250ms ya cruje con su primer escritor (K-8) — diseñar H2 resuelve ambos | M |
| Checkpointing de filesystem (undo) | Solo conversación (backtrack TUI); shadow-git estilo Cline es lo más barato | L |
| Sandboxing OS (Landlock/seccomp) | Nada; Codex CLI ya lo normalizó como table stakes. K-2/J-20/J-31 muestran lo poroso del gate léxico. Una capa Landlock write-only sería M | M/L |
| Prompt caching Anthropic directo | `apply_cache_breakpoints` ya existe para OpenRouter (`openrouter_wire.rs:269-311`); portar es mecánico | S |
| Thinking tokens (Anthropic/Ollama/OpenRouter) | Se ignoran — el caveat de qwen3.5-coder es indiagnosticable sin capturarlos | M |
| Token counting pre-request / rate limit proactivo | Solo Usage del stream / solo reactivo | M |
| AGENTS.md interop | No se lee; emergió como estándar (arXiv 2602.14690) | S |
| Harness multi-ventana (inicializador + progress file) | No existe; patrón Anthropic barato de adoptar en `braze run` | S/M |
| Interfaz JSON para CI | En el diff actual (listo para commit) + K-22 | S |

## 6. Delta de literatura (sobre `docs/SOTA-2026-07.md`)

Lo nuevo con relevancia ALTA para la tesis (URLs en el reporte del
agente, resumidas aquí):

1. **"Harness engineering" ya es disciplina con nombre** — Fowler
   (martinfowler.com/articles/harness-engineering.html), Osmani,
   awesome-harness-engineering, y el estudio empírico de 2.853 repos
   (arXiv 2602.14690: context files dominan, AGENTS.md como estándar,
   skills/subagents con adopción baja). Encuadre citable para el paper.
2. **Trivedy 2026 (faros.ai/blog/harness-engineering)**: +13.7pp en
   Terminal-Bench 2.0 con modelo fijo, solo harness — segunda cita
   cuantitativa de la tesis junto al TR de Qwen3-Coder-Next.
3. **SWE-Protégé (arXiv 2602.22124)**: escalación *aprendida*
   chico→experto; Qwen2.5-Coder-7B a 42.4% Pass@1 SWE-bench Verified
   (+25.4pp sobre SOTA de chicos) con ~4 llamadas al experto/tarea (11%
   de tokens). El techo natural del `--lead` de braze.
4. **Constraint Tax (arXiv 2606.25605)**: el enforcement de JSON schema
   en decoding suprime tool-calling, más cuanto más chico el modelo —
   validación académica directa del rescate textual + escalera de
   reparación de braze; argumento para NO adoptar structured outputs
   de Ollama en el ejecutor.
5. **SWE-Edit (arXiv 2604.26102)**: Viewer/Editor como subagentes de
   contexto angosto; formato de edición entrenable en un 8B. La única
   variante de multi-agente con evidencia a favor.
6. **Self-Compacting Agents (arXiv 2606.23525)**: compactación como
   tool auto-invocada con rúbrica de *cuándo* (-30-70% tokens, +5-18
   pts) — ortogonal y componible con el compactor determinístico.
7. **MemCoder (arXiv 2603.13258) + MemoryArena (arXiv 2602.16313)**:
   related work directo del Paper 2 (+9.4% en SWE-bench Verified con
   memoria estructurada; benchmark externo de memoria multi-sesión).
   Citar y diferenciarse (braze destila desde rollout logs, no commits).
   El marco rate-distortion "Remember the Decision" (arXiv 2605.10870)
   fundamenta teóricamente qué es seguro olvidar.
8. **TTC agéntico (arXiv 2604.16529)**: Recursive Tournament Voting
   sobre resúmenes de rollouts paralelos — con inferencia local los
   tokens son casi gratis: la palanca de test-time más natural para
   braze (encaja con las repeticiones ya existentes de braze-bench).
9. **Escepticismo multi-agente consolidado**: con compute igualado, un
   agente único iguala o supera al multi-agente genérico (overhead
   58-285%, handoffs con pérdida) — confirma la decisión de braze de
   diferirlo; la excepción es §5.
10. **Anthropic "Effective harnesses for long-running agents"**: prompt
    inicializador + progress file para tareas que cruzan ventanas.
11. **Evaluación 2026**: τ²-bench cuasi-saturado y SWE-bench Verified
    >90% en frontera desplazan la vara a Terminal-Bench 2.x / SWE-bench
    Pro; **BFCL v4** re-pesó (Agentic 40% + Hallucination 10%) y agregó
    "format sensitivity" — el eje más alineado con la tesis de braze.
    tau-bench introdujo **pass^k** (fiabilidad): para un harness que
    vende confiabilidad de modelos chicos es LA métrica de la tesis, y
    se calcula offline sobre los JSON existentes (esfuerzo S).
12. Ecosistema: Gemini CLI → Antigravity CLI (18-jun); Codex CLI
    documenta sandbox Landlock+seccomp (el plan Fase 2 de braze, ya
    normalizado como table stakes) y auto-compactación; Goose a la
    Agentic AI Foundation (LF); mini-swe-agent agregó tolerancia a
    FormatErrors de modelos chicos.

## 7. Gemma4 — qué pasó esta semana

**Sí hubo update real: "stealth refresh" del 15-jul-2026** (pesos,
kernels y plantillas en HF, sin bump de versión). Lo relevante para
braze: **fixes de tool calling** (τ²-Telecom +10.1pp en 31B, τ²-Airline
+8.0pp en E4B) y fix de plantilla de chat (menos fugas de role-tags —
la clase de problema de `ollama-gemma-adaptation-2026-07-11.md`). Flash
Attention 4 y visión: irrelevantes para Nitro (CPU).

**Hallazgo crítico verificado**: los pesos refrescados **NO están en el
registry de Ollama** — `gemma4:e4b` sigue con digest `c6eb396dbd59`,
idéntico al del sweep del 13-jul. `ollama pull` hoy es un no-op. En
cambio, **Ollama v0.32.1 (16-jul) trae su propio fix**: "Improved Gemma
4 tool calling… more reliable tool-response continuations".

**Plan de sweeps** (en orden):
1. **A/B de runtime**: actualizar Ollama en Nitro a ≥0.32.1 y re-correr
   exactamente `default.toml --repetitions 5 --temperature 0.2 --seed
   42` con `gemma4:e4b,gpt-oss:20b`. Mismo digest que el 13-jul ⇒ la
   comparación aísla el efecto del runtime. Hipótesis falsable: los 3
   fallos `assertion_tool_call` de `single_tool` desaparecen (e4b quedó
   96.8% vs 100% de gpt-oss:20b; su modo de fallo es exactamente lo que
   este fix repara). Registrar versión de Ollama en la metadata.
2. **Oportunista**: `gemma4:12b` (7.6GB, 256K contexto, nunca medido en
   braze) en `g10-weak-skills` — cabe en Nitro.
3. **Cuando cambie el digest** en ollama.com/library/gemma4: re-pullear
   y repetir el sweep 1 para medir el efecto de los pesos. Recién ahí
   se decide si e4b desafía a gpt-oss:20b (si empata, gana como default
   por ~40% menos RAM residente).

## 8. Roadmap v8 — priorizado

**Paquete 0 — bloqueadores de evidencia (antes de CUALQUIER sweep):**
1. K-11 (`OLLAMA_HOST` en `stop_ollama_model` — una línea; aplicar
   también en el worktree bfcl-anchor).
2. K-1: arreglar el breaker (a/b/c/d mínimo + tracing) o desaplicar el
   diff antes de sweeps. Commitear `braze run --output-format json` y
   `task.rs` (listos).
3. Correr el sweep BFCL pendiente (`bfcl-anchor-RESUME.md`) — sigue
   siendo la respuesta al issue #1 de las 4 rondas de review.

**Paquete 1 — integridad del Paper 2 (antes de más pilotos):**
4. K-6 (dedup Done→Done) + K-7 (validar project_key) — ambos S.
5. K-9/K-10: `expect_cargo_check` + fixtures con test de grading.
6. K-8 (hook escritor fuera del timeout de 250ms).

**Paquete 2 — seguridad (misma familia que el Paquete 3 de v7):**
7. K-2 (`git diff --no-index`) + K-4 (`ask_user` ANSI) — ambos S.
8. K-3 (ProjectMemory como dato no-confiable + `.braze/` Irreversible)
   — condición para promover `enable_project_memory` o versionar
   `memory.json`.
9. K-5 (flock en la reparación N-5).

**Paquete 3 — P1.1: ejecutar el split de engine.rs** siguiendo §3
(empezar por `rescue.rs`, riesgo nulo). v7 lo pidió "antes de la
próxima ronda de features"; desde entonces +1.245 líneas.

**Paquete 4 — camino a frontera (informado por §5-6, orden por
impacto/esfuerzo para modelos pequeños):**
10. pass^k sobre los JSON existentes (S — alto retorno narrativo).
11. Prompt caching Anthropic directo (S — portar de OpenRouter).
12. Summary-por-lead en compactación + failover `CircuitOpen`→lead
    (S/M — reusa EscalatingBackend).
13. TTC local: N rollouts por tarea + selección (M — braze-bench ya
    repite; el diferenciador experimental más barato).
14. AGENTS.md interop + patrón inicializador/progress-file (S/M).
15. K-19 (estadística pareada + Holm-Bonferroni) para el análisis
    offline del paper (M).
16. Landlock write-only (M) — cierra de raíz la clase K-2/J-20/J-31.
17. Subagente Viewer/Editor estilo SWE-Edit (L — solo tras el split).

**Citas nuevas para el paper**: Constraint Tax (§6.4 valida el rescate
textual), Trivedy (§6.2 segunda evidencia solo-harness), arXiv
2602.14690 (related work de harness engineering), MemCoder/MemoryArena
(Paper 2), pass^k de tau-bench (Threats/metodología).

**Gemma4**: actualizar Ollama de Nitro a ≥0.32.1 cuando esté libre y
correr el A/B de runtime (§7) — puede compartir la misma sesión de
Nitro que el sweep BFCL.
