# Auditoría v7: braze — post-cierre del roadmap v6 y del estudio consolidado

Fecha: 2026-07-11
Proyecto: `braze`
Base observada: commit `ad4bb5a` (HEAD de main, pusheado) + worktree con 1 archivo sin
commitear (`crates/braze-bench/src/task.rs`, +79: test de regresión de la suite
`gemma-diagnostic.toml` — verificado verde, pendiente de commit por la otra sesión).
Verificación previa ejecutada (esta sesión): `cargo test --workspace` — **895 tests verdes**;
`cargo clippy --workspace --all-targets -- -D warnings` — limpio (ambos incluyendo el
worktree sucio).
Método: 4 subagentes de investigación en paralelo — (1) verificación de los abiertos de
v6 contra HEAD, (2) revisión escéptica del código nuevo del rango `b1fcf12..HEAD`
(24 commits), (3) pasada fresca sobre el núcleo del engine + validez del harness de
evidencia (braze-bench, del que dependen los números del paper), (4) superficies nunca
re-auditadas (permisos desde v3, TUI, MCP, sesión, config).

## Meta y alcance

v7 complementa v1-v6 — NO las reemplaza. Dos lentes:

1. **Cierre del ciclo v6**: verificar en código (no en docs) qué pasó con cada abierto
   de v6 tras los Paquetes 1-4 y el estudio consolidado A′-E′.
2. **La evidencia del paper**: el manuscrito en `paper/` cita números de braze-bench;
   los hallazgos que amenazan su validez llevan etiqueta **[PAPER]**.

Novedad de alcance vs v6: esta vez SÍ se auditaron TUI, MCP, sesión y permisos
(superficies que v6 excluyó explícitamente).

## Estado de salud del motor

| Métrica | Valor | Δ vs v6 |
|---|---|---|
| Líneas Rust | ~44.455 | +6.861 |
| Líneas `engine.rs` | 10.137 | **+2.250 (+28% en un día — P1.1 se aceleró)** |
| Tests | 895 verdes | +93 |
| `cargo clippy -D warnings` | limpio | = |
| Crates | 14 (`braze-skills` nuevo) | +1 |
| Worktree dirty | 1 archivo (+79, test suite gemma) | mejor que v6 (12 archivos) |

## Verificación de los abiertos de v6 — resumen de dictámenes

**15 FIXED, 4 PARCIAL, 6 OPEN.** Ninguno de los OPEN contradice lo que los commits
claiman haber cerrado: todos quedaron fuera de los Paquetes 1-4 comprometidos.

Nota de arqueología: los cierres de I-1/I-2/I-3/H-4/H-3 aterrizaron en los commits
inmediatamente **anteriores** a `b1fcf12` (`e11d628`, `9aff6aa`, `6668f94`, `658fd1a`) —
el doc v6 se commiteó después de esos fixes. Los 24 commits posteriores cubren el resto
del Paquete 1 (`912fedb`), el Paquete 3 (`8b720f1`), el Paquete 4 (`e16143e`) y el
estudio consolidado A′-E′.

### FIXED (verificados en código)

| ID | Evidencia | Nota |
|---|---|---|
| I-1 knobs de escalación | config `file.rs:52-54`, env `BRAZE_LEAD_*`, seam `EscalatingBackend::with_configured_knobs` (escalation.rs:126-142), ambos roots, `+ablate:lead-*` | Modo puramente reactivo usado en el 3-brazos: proactivo 92.6% vs reactivo 75.8% vs baseline 67.4% |
| I-2 caps proporcionales | `tactical_cap_scale` engine.rs:3690-3700 — proporcional al budget, clamp [1,10], guard para `+ablate:` explícitos | Tests 8K→×1, 32K→×5, cloud→×10 |
| I-3 clasificación vs render | escalation.rs:357-369 (cleared nunca cuenta) + history.rs:298-308 (colapso preserva marker post-edit) | Robusteció el render, no el campo tipado — sigue acoplado por strings (ver J-33) |
| I-4 hint multi-familia | hint name-based en todos los backends (main.rs:505-516, runner.rs:168-171); `ModelFamily::GlmArgTags` | Gemma queda `Generic` a propósito (test: no leak grammar observed) |
| P0.3 preflight destructivo | write_file.rs:21,58-66 — **rechaza** el shrink >50% y >200 bytes sin `allow_shrink:true`, antes de tocar disco | Pasó de warning post-hoc a refusal preflight |
| H-2 `+ablate:no-caching` | parse backend_spec.rs:666 + aplicación real :510-516 | La llamada `with_prompt_caching_enabled` faltaba entera |
| H-4 Usage del fallback | engine.rs:1948-1970 + test | |
| H-6 `env` clasifica seguro | classifier.rs:75,202 — `is_safe_env` retorna false siempre | Con tests dedicados |
| F3 post-edit para escalación | escalation.rs:325,370-372 + history.rs:302-308 | Cerrado junto con I-3 |
| P1.4 best-of-n paralelo | engine.rs:945-953 `join_all`, orden determinista | Tal como v6 predijo: sin refactor |
| H-17 ablations en metadata | metadata.rs:35-43 `backend_specs` con sufijo `+ablate:` completo | Cumple la sustancia (registro run-level) |
| H-18 cache tokens Anthropic | anthropic_wire.rs:255-317 | Pero ver J-29: cambió la semántica de `input_tokens` |
| H-13 warning sampling knobs | backend_spec.rs:257-280 + main.rs:181-193 | Por spec afectado, nombrando knob y mitad |
| opencode-10 references | `ReferenceConfig{path,description}`, sección en prompt, allowlist de permisos | bench pasa `&[]` deliberado (hermeticidad) |
| Paquete 0 completo | H-3 en `e11d628`; sweep-si2 doc corregido con la sección "⚠ CORRECCIÓN (2026-07-10)" | 1 escalación reactiva en 190 corridas — confound cuantificado |

También: la matriz executor/+planner/+lead/+ambos quedó **completa** (380 corridas,
`sweep-matriz-4brazos-2026-07-10.md`) y extendida con el planner arreglado
(570 corridas, `sweep-planlead-2026-07-11.md`).

### PARCIAL

| ID | Qué falta | Evidencia |
|---|---|---|
| P0.2 TurnBudget | Existe breaker de **tokens** (`max_turn_total_tokens`, enforceado en ambos roots, `FailureCause::TurnBudgetExhausted`) y pricing→`estimated_cost_usd`→`expect_max_cost_usd` como expectativa del bench. Falta: tope de **costo USD en runtime** (engine.rs:429 lo declara token-only "on purpose") y tope de **walltime** por turno. Default `None` = apagado salvo config explícita | config.rs:435-441, engine.rs:1223 |
| E1 ablations | `no-planner`/`no-lead`/`no-compaction` parseadas Y aplicadas. Falta `no-preflight` — el preflight P0.3 recién creado no es ablacionable | backend_spec.rs:668-670 |
| opencode-2 | `+ablate:no-prune` cerrado (runner.rs:224). Falta la mitad de los bytes: `MAX_FULL_OBSERVATIONS_TOTAL_CHARS` no expuesto como knob (`full-observations=N` controla el conteo, no los bytes) | backend_spec.rs:667 |

### OPEN (sin cambio, fuera de los Paquetes)

| ID | Sev. v6 | Estado hoy |
|---|---|---|
| I-5 dedup por message-count | BAJA | **ESCALADO A ALTA en esta auditoría** — ver J-1: bajo compactación la premisa de monotonicidad es falsa |
| I-6 break tras primer rung | BAJA | engine.rs:866-879 — sigue; ver J-22 nota |
| I-7 READ_TIMEOUT 600s fijo | BAJA | http_client.rs:27 — sigue const |
| H-7 durable_events sin cota | ALTA | simple_compactor.rs:200-262 — sigue |
| P1.1 engine.rs gigante | ALTA | **Peor: 10.137 líneas (+2.250).** Cada paquete lo agrandó; hacerlo antes de Fase 2 sigue siendo el plan, pero la tendencia se aceleró |
| MEDIAs/BAJAs de v5 no re-listadas | — | Sin cambio (H-8/10/11/12/14/15/16/19/20-25, P2.x, B*, E2/E3) |

## Hallazgos NUEVOS (serie J)

Provenientes de los 4 informes, deduplicados (3 hallazgos aparecieron en dos agentes
independientes — señal de robustez, se listan una vez con ambas evidencias).

### ALTAS

#### J-1 [ALTA][PAPER] — El dedup de rondas del `EscalatingBackend` se rompe bajo compactación (escalación de I-5)

`escalation.rs:74-79` asume "history only grows within a turn" y dedupea por
`messages.len()`. Con compactación por presupuesto activa (Ollama con `num_ctx` chico —
exactamente la población del paper), `load_messages` pliega todo al último
`CompactionOccurred` y dos rondas consecutivas con la misma forma (1 tool call por
ronda) producen **el mismo count** → `route` replay-a `last_decision` (escalation.rs:175-181).
Dos modos de falla: (a) última decisión `Worker` → la racha de fallos nunca se re-evalúa
y **la escalación jamás dispara justo en los turnos floundering**; (b) última decisión
`LeadEscalating` → un `AgentEvent::EscalationToLead` re-estampado POR RONDA →
`leader_escalations` inflado en el bench y ventana de lead infinita (costo inflado del
brazo `+lead:`).
**Arreglo**: dedupear por hash del contenido de `messages` (no toca el trait congelado)
o contador de ronda explícito. Esfuerzo M.

#### J-2 [ALTA][PAPER] — La task list y las harness notes matan la escalación reactiva por construcción

`trailing_failed_observations` (escalation.rs:295-317) hace `break` en el primer mensaje
`Role::User` sin `ToolResult`. Pero el harness inyecta mensajes User de texto al final de
cada ronda: el resumen de la task list (engine.rs:1251-1262, cada ronda con tareas
abiertas) y las `HarnessNote` (history.rs:439-444, emitidas justo cuando el turno se
degrada — budget/iteration-cap). El scan reverso los ve primero → `failures = 0` siempre.
**En el brazo `+lead:`+task-list la escalación reactiva está muerta por construcción**, y
con `harness_notes_enabled` (default ON) se anula exactamente cuando ambas palancas
deberían componerse. Relevante para la interpretación del A/B planner→tasks+lead
(`sweep-planlead-2026-07-11.md`): "tasks+lead RESTA" podría incluir este mecanismo — el
lead nunca pudo re-activarse reactivamente en ese brazo.
**Arreglo**: en el scan, saltar (no romper) mensajes User con prefijos del harness
(`[harness]`, `Task list:`, `[Resumen de contexto previo]`, `Plan for this request`) —
mismo acoplamiento-por-convención que ya usa `[tool result cleared:`. Esfuerzo S.

#### J-3 [ALTA] — Los `HarnessNote` persisten y se re-renderizan en TODOS los turnos posteriores

Emisión persistida en engine.rs:1466-1502; render incondicional como mensaje user en
history.rs:439-446; el compactor solo los excluye del digest durable, no de la ventana
táctica. En chat multi-turno, un `[harness] ... Answer now with what you already have
instead of calling more tools.` del turno 1 sigue siendo instrucción activa en los
turnos 2..N — para un 3B puede suprimir tool calls de un turno nuevo que sí los
necesita. Se acumulan hasta que una compactación los barre. El bench es single-turn,
así que el A/B `no-harness-notes` **nunca ve este modo de falla**.
**Arreglo**: render turn-scoped (solo notes posteriores al último `UserMessage`) o
volverlos ephemeral request-scoped como el resumen de la task list. Esfuerzo S/M.

#### J-4 [ALTA] — La `TaskList` nunca se limpia entre turnos

No existe `clear()`/reset (task_list.rs:64-124 solo agrega; engine.rs:208,363). El
planner siembra en CADA turno (engine.rs:1801-1810) y el resumen re-inyecta TODAS las
entradas cada ronda mientras haya alguna abierta. En sesión multi-turno: mezcla de
planes de temas distintos, pendientes muertos arrastrados para siempre, costo por ronda
monótonamente creciente — lo contrario del objetivo declarado de C′.2. Invisible en el
bench (single-turn).
**Arreglo**: reset de la task list al inicio de `run_turn` (o poda de `done` de turnos
anteriores antes de sembrar). Esfuerzo S.

#### J-5 [ALTA][PAPER] — La tabla del bench imprime el IC de Wilson mal: half-width centrado en p̂

`report.rs:81-94` computa Wilson correcto como `(center, half_width)`, pero
`report.rs:250-252` imprime `"{passed}/{total} (±{half_width}pp)"` **descartando el
center**. Caso dominante del paper (celdas 6/6 y 0/6, n chico): 6/6 → Wilson real
[61%, 100%] (0.805±0.195); la tabla muestra "6/6 (±20pp)" que se lee como [80%, 120%].
La cota inferior real queda 19pp por debajo de lo implicado. **Las figuras en R con
Newcombe desde el JSON crudo NO están afectadas** — el riesgo es cualquier cifra de la
tabla stdout citada en texto/tablas del manuscrito.
**Arreglo**: imprimir `[lo, hi]` o el par (p̂, IC asimétrico). Esfuerzo S.

#### J-6 [ALTA][PAPER] — Sesgo de warm-up entre brazos del sweep

No hay ronda de calentamiento (main.rs:209-284: el "probe" solo construye el backend,
no emite request) y el orden brazo→tarea→repetición hace que (a) la primera tarea del
primer brazo pague la carga del modelo (decenas de segundos; riesgo de `[Timeout]` no
atribuible al modelo en CPU) y (b) con `--no-ollama-stop` (la regla operativa de Nitro)
los brazos tardíos hereden el modelo residente. Sesgo sistemático a favor de brazos
tardíos en `avg_ms`/`median_ms` y potencialmente en pass rate vía timeouts, siempre en
la misma tarea (la primera del TOML).
**Arreglo**: completion trivial de warm-up descartada por modelo Ollama antes de la
primera tarea de cada brazo. Esfuerzo S.

### MEDIAS

#### J-7 [MEDIA][PAPER] — `expect_text_contains` se evalúa sobre TODOS los `AssistantText` del turno

`metrics.rs:455-462` concatena todo `AssistantText` (incluida la narración pre-tool-call
persistida en engine.rs:1442-1449). Una tarea que espera `"2"` puede dar **PASS falso**
si el modelo narró "voy a leer las 2 primeras líneas" antes de responder mal. El
bounded-token matching (E4) mitiga filenames pero no narración intermedia, abundante en
modelos chicos — falsos positivos direccionales (favorecen a modelos verbosos).
**Arreglo**: evaluar solo el último `AssistantText` (o el tramo posterior al último tool
call). Re-correr las celdas apretadas del paper tras el fix. Esfuerzo S.

#### J-8 [MEDIA] — El rescate pythonic no chequea code fences (asimetría con el fix F1)

Los rungs tagged y `<function=` aplican `is_inside_code_fence` (engine.rs:2969-2974,
3048-3053); `extract_pythonic_tool_calls` (engine.rs:3221-3250) no. Un ejemplo fenced
```` ```[get_weather(city="SF")]``` ```` se extrae y despacha — el mismo bug que F1
arregló para los otros dos formatos. **Arreglo**: replicar el chequeo. Esfuerzo S.

#### J-9 [MEDIA] — Una tool diferida es invocable sin activarla (hallado por 2 agentes independientes)

La intercepción del dispatch solo cubre `search_tools` y las task tools (engine.rs:2117);
cualquier otro nombre va directo a `tools.resolve()` (engine.rs:2206) — el registry no
sabe de la deferral. Un modelo que nombra una tool oculta (memoria de rondas
pre-compactación, adivinanza) la ejecuta sin `search_tools`, y además no queda en
`activated_deferred_tools` (sigue sin listarse). Para la Figura 3: el brazo "deferral"
no es estrictamente "solo puede usar lo listado" — la claim del mecanismo queda más
débil de lo que el texto sugiere (improbable con el fixture de ruido, pero posible).
**Arreglo**: decidir explícitamente — rechazar el dispatch de ocultas no activadas con
error accionable ("use search_tools first"), o documentar la semántica ("deferral =
espacio de prompt, no invocabilidad"). Esfuerzo S(doc)/M(gate).

#### J-10 [MEDIA][PAPER] — El `Usage` de una ronda se pierde en dos paths de error

(a) Stream que falla mid-round: engine.rs:801-810 retorna `Err` antes de persistir el
`Usage` ya recibido. (b) Timeout del bench: runner.rs:275-287 dropea el futuro de
`run_turn` con la ronda en vuelo. Una fila `FailureCause::Timeout` tras 12 rondas
reporta tokens de ~11 — comparaciones de tokens entre brazos con distinta tasa de
timeout quedan sesgadas a favor del brazo que más falla (subcuenta donde más se gastó).
**Arreglo**: adjuntar el usage capturado al error / documentar en el paper que los
tokens de filas Timeout son cota inferior. Esfuerzo S(doc)/M(código).

#### J-11 [MEDIA] — Parser de frontmatter de skills matchea por prefijo de línea

`braze-skills/src/lib.rs:264-268`: `strip_prefix("name:")` acepta `namespace: gis`
(name = `"space: gis"` → normalizado `"space:-gis"`, que contiene `:` y es
**inmencionable** — `explicit_mentions` solo acepta `[a-z0-9_-]`), y la última
ocurrencia gana. Un SKILL.md real con `name:` + `namespace:` desaparece en silencio.
**Arreglo**: `split_once(':')` y comparar la clave exacta. Esfuerzo S.

#### J-12 [MEDIA] — `--resume` pierde las skills cargadas en silencio

`loaded_skills` es solo memoria (engine.rs:204-206); los eventos `SkillLoaded` quedan en
el log pero nada los re-hidrata. Tras restart, la conversación referencia una guía que
el system prompt ya no lleva — el modelo chico pierde exactamente la palanca que D′
existe para darle. (task_list.rs:19-23 sí documenta su pérdida equivalente; el doc de
skills no.) **Arreglo**: re-ejecutar `load_body()` por cada `SkillLoaded` del log al
cargar sesión (M), o documentar la limitación (S).

#### J-13 [MEDIA] — `ask_user` corre bajo el `tool_completion_timeout` de 120s

El dispatch usa `next_completed(timeout)` (engine.rs:42,2367 — el test de la línea 4543
confirma que cancela). Si el humano tarda >120s: la call se cancela (el modelo recibe
timeout), y la línea que el usuario teclea después la consume el loop de chat como
mensaje nuevo (main.rs:500) — turno basura con prompt "2". La herramienta cuyo propósito
es evitar que el modelo adivine termina adivinando + inyectando un turno espurio.
**Arreglo**: exceptuar `ask_user` del timeout (dispatch inline como `search_tools`) o
timeout por-tool con valor humano; al expirar, drenar el próximo line del stdin.
Esfuerzo M.

#### J-14 [MEDIA] — El planner y el summary-fallback esquivan los hooks Y el breaker de tokens (hallado por 2 agentes)

`dispatch_hooks_before_model_request` solo se invoca en el loop del executor
(engine.rs:1274); `turn_total_tokens` no suma el usage del planner (engine.rs:1189,
1743-1756). (a) `PromptBudgetAuditHook` no ve el request más pesado de una fila `+plan:`
(el planning prompt lista el inventario completo de tools). (b) Un planner cloud caro
consume presupuesto que el breaker no ve — pero `expect_max_tokens` del bench SÍ lo
cuenta (metrics.rs:380-390): incoherencia entre las dos cotas. Además el summary-fallback
arma su prompt desde `self.system_prompt` crudo (engine.rs:1856-1866) — pierde los
addenda de skills y el estado de la task list justo en la ronda que salva el turno (J-34).
**Arreglo**: dispatch de hooks + suma al acumulador en `attempt_planning_round` y
`attempt_tools_free_summary_round`. Esfuerzo S.

#### J-15 [MEDIA][PAPER-diagnóstico] — `schema_validation_failures` es un cajón de sastre y las denegaciones cuentan doble

metrics.rs:361-373: la heurística "error sin `ToolCallStarted` = schema failure" también
captura el nudge A5, unknown-tool, errores de task tools y reparaciones de huérfanos. Y
una denegación de permiso cuenta como `permission_denials` + `tool_execution_failures`.
No toca pass/fail, pero cualquier análisis diagnóstico del paper ("el brazo X falla por
schema") atribuye mal. **Arreglo**: clasificar por prefijo o campo `cause` tipado en el
`ToolResult` sintético. Esfuerzo M.

#### J-16 [MEDIA] — Fuga de tareas de fondo tras timeout del bench

runner.rs:275-287: `tokio::time::timeout` dropea `run_turn` sin abortar las tasks ya
spawneadas (el abort de N-33 vive dentro del loop que muere con el futuro). Un
`shell_exec` colgado sigue consumiendo CPU durante las tareas siguientes del sweep —
la misma clase de contaminación que motivó la regla "benchear en Nitro". Sin falso PASS
(pasa requiere `converged`), pero sí trabajo zombie y wall-times contaminados.
**Arreglo**: retener los `TaskHandle` a nivel runner / `abort_all` en el notifier tras
timeout. Esfuerzo M.

#### J-17 [MEDIA][PAPER-conservador] — El budget de contexto Ollama se calcula sobre TODOS los stubs, pre-deferral

runner.rs:191-199: `tool_stub_definition_bytes(&tools.all_stubs_lossy())` incluye las N
noise tools aunque la deferral las oculte del prompt real → el brazo `search_tools`
recibe un budget artificialmente chico y compacta/colapsa antes de lo necesario. Sesgo
**contra** el brazo deferral: la Figura 3 es conservadora, no inflada. Conviene
arreglarlo y anotarlo en el paper. **Arreglo**: computar sobre
`apply_deferral(...).visible` + stub del meta-tool. Esfuerzo S.

#### J-18 [MEDIA] — `ls` y `wc` eluden el gate de lectura fuera del workdir (regresión parcial de N-8b)

classifier.rs:69: ambos se clasifican `Reversible` incondicionalmente, sin
`all_path_like_args_allowed` (que sí gatea `cat`/`head`/`tail`/`file`/`diff`/`grep`/`find`).
`["ls","-la","/home/otro/.ssh"]` o `["wc","-c","/etc/shadow"]` corren sin confirmación —
la misma clase de fuga que N-8b cerró para los reads de contenido. **Arreglo**: moverlos
al brazo path-checked (los que no toman rutas — `pwd`/`echo`/`whoami`/... — pueden
quedar). Esfuerzo S.

#### J-19 [MEDIA] — Los prompts de aprobación (CLI y TUI) renderizan la descripción sin sanitizar ANSI/control-chars

CLI: terminal_prompt.rs:57 escribe `{action}` crudo a stdout. TUI: app.rs:1457-1462 —
y `truncate_for_display` presupuesta por `c.width()`, donde ESC tiene width 0: **los
control-chars no cuentan contra el budget y se preservan**. El proyecto ya tiene
`sanitize_tool_output` (history_cell.rs:124) aplicado al historial — pero no a la
descripción de la aprobación, que es donde el humano decide. Un argv del modelo (o un
nombre de tool de un server MCP hostil — action.rs:52-55) puede reposicionar cursor,
ocultar la parte peligrosa del comando o falsificar la línea "y permitir · n denegar".
El usuario aprueba algo distinto de lo que cree ver. Mismo problema en
`permissions_report.rs:134-148` (render del reporte con keys persistidas crudas).
**Arreglo**: sanitizar control-chars (CSI + CR/backspace/newlines embebidos) en
`action.to_string()`/`request.description` en ambos front-ends y en el reporte.
Esfuerzo S.

#### J-20 [MEDIA] — Escape por symlink en escritura/lectura (limitación MVP documentada — decisión pendiente de ratificar)

allowlist.rs:42-46: normalización léxica, nunca `canonicalize`; el doc-comment lo
declara ("Symlink escapes are NOT caught in the MVP"). Un symlink `./notas -> ~/.bashrc`
(pre-existente o creado por un `write_file` previo dentro del workdir) permite escribir
fuera del allowlist sin confirmación. Se reporta para que la aceptación sea consciente,
no por omisión — la escritura a través de symlink es la variante grave.
**Arreglo si se decide cerrar**: canonicalizar el directorio padre (existe aunque el
target no) en el path de escritura. Esfuerzo M.

### BAJAS

| ID | Hallazgo | Evidencia | Arreglo/Esf. |
|---|---|---|---|
| J-21 [PAPER-caveat] | `avg_rounds` mezcla convergidas y fallidas; filas Timeout aportan conteo censurado (el brazo débil se ve mejor) | report.rs:127,143 | Reportar también condicionado a `passed`, o anotar censura en el paper. S |
| J-22 | `RECOGNIZED_KEYS` del error de `+ablate:` omite `no-harness-notes`, `task-list`, `tool-search-threshold` (hallado por 2 agentes). Un typo recibe ayuda engañosa | backend_spec.rs:646-649 vs :671,684-687 | Derivar el string de los parsers. S |
| J-23 | Doble `+ablate:` en un spec: `rfind` se traga el primero como parte del model name → filas `ModelBackendError` contra modelo inexistente, sin error de parseo | backend_spec.rs:73-79 | Rechazar >1 ocurrencia. S |
| J-24 | En brazo `+lead:`, el lead corre bajo el hint de familia del *worker* (un lead DeepSeek recibe instrucciones de template Qwen) — desventaja no medida del brazo | runner.rs:171-179 | Hint por-request (tocar decorator). M |
| J-25 | Skills: doc de `MAX_BODY_BYTES` dice "se trunca al cargar", el código descarta el archivo entero (>64KB → `None`); `load_body` relee sin guard si el archivo creció | lib.rs:38-41 vs :243-246; :135 | Alinear doc y comportamiento; leer con límite. S |
| J-26 | La TUI ignora `HarnessNote` (catch-all silencioso) — el usuario no ve lo que el harness le dijo al modelo; el bench sí los cuenta | app.rs:1291-1298 | Celda estilo `PlanCell`. S |
| J-27 | Snapshot de entorno: fecha en UTC sin etiquetar (usuario en UTC-4 a las 22:00 → el prompt dice "mañana") | main.rs:236-254 | Etiquetar `date (UTC):`. S |
| J-28 | `activated_deferred_tools` keyed por nombre pelado — activar `convert` de un provider resucita el `convert` de otro | engine.rs:196; tool_search.rs:79 | Keyear por (source, name). S |
| J-29 | `input_tokens` Anthropic ahora incluye cache tokens (H-18): `expect_max_tokens`/`max_turn_total_tokens`/status bar cuentan cache-reads a peso completo — salto de métrica entre sweeps sin nota | anthropic_wire.rs | Documentar en ambas cotas (o restar cache_read en el breaker). S |
| J-30 | Voseo argentino en output user-facing ("volvé a intentar" en `permissions suggest`) — el resto del CLI usa castellano neutro | permissions_report.rs | "vuelve a intentar". S |
| J-31 | Args con ruta adjunta a flag evaden el chequeo de workdir (`grep --file=/etc/passwd`, `-f/etc/shadow`) — fuga de uso, no de contenido directo | classifier.rs:88-93 | Validar valores de flags conocidos que toman ruta. S |
| J-32 | Rollout logs de sesión sin cota de tamaño ni poda (coherente con "log durable"; el caso 481K tokens/turno los agranda rápido) | file_store.rs:105-178 | `braze sessions prune` opcional. M |
| J-33 | Clasificadores de `observation_is_a_failure` por substring (`"\"exit_code\""`, `[post-edit check]`) — residual tipado de I-3, el propio doc-comment lo anota como "the proper fix" | escalation.rs:367-392 | Campo de causa tipado en `ToolResult`. M |
| J-34 | El summary-fallback arma su prompt sin addenda de skills ni task list — la ronda que salva el turno corre con menos guía que las que fallaron | engine.rs:1856-1866 vs :1266 | Usar `system_prompt_with_skills()`. S |

## Discrepancias docs-código

1. **CLAUDE.md "612 tests"** — hoy 895 verdes (894 anotaciones + doctest). Desactualizado ~46%.
2. **CLAUDE.md: split planificador/ejecutor "veredicto A/B negativo (queda opt-in)"** —
   contradice a PLAN.md y al commit `bb07363`: la degeneración era un bug de render, el
   planner se **rescata** (+10/+12pp según entrega). CLAUDE.md quedó congelado pre-rescate.
3. **CLAUDE.md § "Próximos pasos al retomar": 3 de 4 ya ejecutados** — la iteración del
   planner (hecha), el A/B del EscalatingBackend ("falta sintaxis" — existe y el 3-brazos
   está cerrado), y el circuit breaker por costo ("no diseñado" — existe como breaker de
   tokens `max_turn_total_tokens`; solo falta la variante USD).
4. **engine.rs 10.137 líneas** — ni CLAUDE.md ni PLAN.md reflejan la aceleración de P1.1.
5. Sin discrepancia: "14 crates" correcto; PLAN.md al día en planner/matriz.

## Áreas auditadas y encontradas genuinamente bien

- **Permisos**: el clasificador shell es argv puro (sin `sh -c`) — subshells, `$()`,
  backticks, `;|&&`, `env`-prefix, `xargs` no se interpretan; `bash -c` cae a
  Irreversible. `rm -rf`/`git push`/`find -fls`/`git diff -o` bien detectados.
  Aprobaciones por argv completo, denegaciones nunca cacheadas. `permissions suggest` es
  estrictamente read-only: **no puede inyectar reglas** (pregunta explícita del encargo).
- **Sesión**: append-only con flock inter-proceso + `sync_data` por append + reparación
  en disco de última línea truncada por crash. No necesita temp-write-rename.
- **MCP**: namespacing forward-only sin inversión lossy, timeouts en connect/list/call,
  colisiones logueadas, blobs reemplazados por placeholders (no vuelcan base64 al contexto).
- **TUI**: approval con safety-default a denegar si el canal muere; `/model` picker
  valida el spec y conserva el engine actual ante error; reusa el `build_engine` de
  braze-cli (cero divergencia de knobs — a diferencia del bench, ver S-6/J-nota abajo).
- **Engine core**: `TurnGuard` cubre todos los exits; `pair_aware_tail_start` nunca
  separa tool_use/result; reparación de huérfanos idempotente; F1 sigue bloqueando el
  falso rescate de definiciones de schema; best-of-n con accounting honesto de Usage.
- **Bench**: sandbox real por (tarea, repetición) sin fugas de sesión/engine/task-list
  entre corridas; seeds pareados entre brazos; `resolve_pricing` se rehúsa ante tarifas
  mixtas en vez de adivinar; fingerprint de suite + commit + digests en metadata (E6/H-17);
  un write leaked post-timeout no puede producir PASS falso (`passed` exige `converged`).
  **Salvedad de fidelidad**: el bench no aplica `with_output_budget`/`with_output_max_lines`/
  `with_formatters` de la config (runner.rs:145-147 vs main.rs:433-437 de braze-cli) —
  corre con defaults hardcodeados; decidir si es deliberado y documentarlo.
- **Skills**: dedupe determinista, truncado en char boundary, bodies nunca persistidos
  como conversación, cap por turno con evento visible.
- **Retry H-19**: solo pre-stream, 4xx≠429 terminal, Retry-After capado, Ollama sin
  retry deliberado.
- **Config plumbing**: las 6 claves nuevas llegan de env/file a ambos composition roots;
  ninguna palanca expuesta en un root y ausente del otro salvo las deliberadas
  (skills/references/environment fuera del bench por hermeticidad N-36).

## Roadmap v7 — priorizado

**Paquete 0 — higiene inmediata:**
1. Commitear el test de `gemma-diagnostic` (worktree, coordinar con la otra sesión).
2. Actualizar CLAUDE.md (tests, veredicto del planner, próximos pasos, líneas engine.rs).
3. J-30 (voseo) + J-22 (`RECOGNIZED_KEYS`) — dos strings.

**Paquete 1 — antes de citar números del bench en el manuscrito [PAPER]:**
4. J-5 (display de Wilson en la tabla) — y verificar que ninguna cifra stdout ya migrada
   al .tex esté afectada.
5. J-7 (`expect_text_contains` sobre el texto final) + re-correr las celdas apretadas.
6. J-1 + J-2 (escalación bajo compactación / mensajes del harness) — tocan la
   interpretación del brazo tasks+lead ya medido; como mínimo, anotar el confound en el
   paper si no se re-corre.
7. J-6 (warm-up) + J-10/J-21 (documentar censura de tokens/rondas en Timeout) +
   J-17 (budget pre-deferral — anotar que la Figura 3 es conservadora).

**Paquete 2 — el harness en sesiones multi-turno (chat real, invisible para el bench):**
8. J-3 (harness notes turn-scoped) + J-4 (task list reset) — par natural.
9. J-13 (`ask_user` sin timeout de 120s) + J-26 (TUI muestra HarnessNote).
10. J-12 (skills en `--resume`) + J-11 (frontmatter parser) + J-25.

**Paquete 3 — permisos/render:**
11. J-18 (`ls`/`wc` path-checked) + J-19 (sanitizar approval prompts) — ambos S.
12. J-20 (symlink): ratificar la aceptación MVP o cerrar la variante de escritura.

**Paquete 4 — resto por conveniencia:** J-8 (fence pythonic), J-9 (semántica de
deferral — decidir y documentar), J-14/J-34 (planner/summary fuera de hooks y breaker),
J-15 (causas tipadas), J-16 (abort de tasks tras timeout), J-23, J-24, J-27, J-28, J-29,
J-31, J-32, J-33, S-6 (knobs de output en bench), E1 (`no-preflight`), opencode-2
(byte-cap expuesto).

**Diferidos que siguen en pie:** P1.1 (split de engine.rs — la deuda se aceleró: hacerlo
ANTES de la próxima ronda de features), H-7, I-6, I-7, P0.2 restante (costo USD +
walltime), hooks/policies Fase 2.
