# Auditoría exhaustiva de `braze` — julio 2026 (v2, post-Fase TUI 2 / G10 / OpenRouter)

> **Objetivo:** re-auditar `braze` en su totalidad tras la primera auditoría
> (`AUDITORIA-2026-07.md`, grupos A–H remediados) y la superficie nueva
> agregada desde entonces — backend OpenRouter, técnica G10 (best-of-n) y
> Fase TUI 2 (slash commands, @-menciones, Ctrl+T, temas, backtrack). Dos
> preguntas guía: (1) ¿los fixes A–H se sostienen? (2) ¿qué defectos nuevos
> introdujo el código posterior?
>
> **Fecha:** 2026-07-05 · **Commit base:** `c3084a2` · **Cobertura:** los 13
> crates del workspace, ~20k líneas de Rust.
> **Método:** 6 agentes adversariales en paralelo (uno por dominio), más una
> corrida paralela independiente que corroboró de forma redundante los
> hallazgos críticos del engine, los backends y la sesión.

---

## 1. Resumen ejecutivo

**El veredicto en una frase:** la infraestructura sigue siendo sólida y los
fixes de seguridad y de robustez del loop (grupos B, D, F) se sostienen, pero
**la tubería de contexto (grupos A/C) tiene múltiples huecos que reintroducen
exactamente la clase de corrupción permanente de sesión — el 400 de Anthropic
que no se cura con reintentos — que esos grupos habían cerrado.** Todos se
manifiestan contra Anthropic real en sesiones de longitud media/larga, es
decir, precisamente en la verificación end-to-end de Fase 6 que aún está
pendiente. Ninguno lo detectan los tests actuales porque usan `ScriptedModel`
(no valida el orden de mensajes) y el sweep promedia 2.4 rondas (<20 eventos,
por debajo de todos los umbrales de compactación).

### Por qué los tests verdes no los atrapan

- **`ScriptedModel` no valida el protocolo de mensajes.** Los tests de
  regresión de C4/A1/A2 pasan porque el modelo simulado acepta cualquier
  secuencia; Anthropic no.
- **El sweep es demasiado corto.** 2.4 rondas ≈ 10-12 eventos; los umbrales
  de compactación (`window=20`, `threshold=40`) y la banda de reordenamiento
  (21-40 eventos) nunca se cruzan en el bench.
- **Los caminos nuevos (TUI, budget de Ollama, OpenRouter heterogéneo) no
  tienen cobertura de integración.**

### Conteo de hallazgos (deduplicado, tras fusionar las dos corridas)

| Severidad | Nuevos | Comentario |
|-----------|:---:|---|
| **CRÍTICA / ALTA** | 12 | 7 son corrupción permanente de sesión (400 Anthropic); 2 de seguridad; 1 de backends; 2 de TUI |
| **MEDIA** | ~18 | robustez del loop, validez del bench, paridad OpenRouter, UX de la TUI |
| **BAJA** | ~15 | deuda menor, mensajes de ayuda desactualizados, cosméticos |

### Lo que hay que arreglar primero (bloqueantes de Fase 6)

1. ~~**N-2 · Reordenamiento durable/táctico → 400 en toda sesión media**~~ —
   ✅ resuelto 2026-07-05.
2. ~~**N-1 · Corte `KEEP_RAW_TAIL` parte pares tool_use/result → 400 al
   compactar**~~ — ✅ resuelto 2026-07-05 (junto con N-1b, un hallazgo más
   profundo descubierto al implementar este fix).
3. ~~**N-4 · La reparación de huérfanos apendea el resultado *después* del
   `UserMessage` nuevo → orden inválido persistido → 400 permanente.**~~ —
   ✅ resuelto 2026-07-05.
4. **N-3 · "Apagón" del summary: el `CompactionOccurred` recién creado es
   invisible ~20 eventos → desaparece la pregunta original del usuario.**
   Pendiente.
5. **N-6 · Modo compactación permanente reintroducido por el budget de
   tokens** (Ollama, activo por default). Pendiente.
6. **N-9 · Tool calls sin argumentos se dropean en silencio** (Anthropic +
   OpenRouter) → el agente "converge" sin ejecutar la llamada. Pendiente.

---

## 2. Hallazgos críticos y altos

### Tubería de contexto — la corrupción permanente reintroducida

Estos siete comparten la misma consecuencia: un log de sesión que, una vez
escrito, hace que **todo request futuro** contra Anthropic falle con 400
(orden de mensajes inválido o pares `tool_use`/`tool_result` rotos). Como el
log es append-only, el error no se cura solo. En OpenRouter/Ollama (formato
OpenAI) el mismo defecto degrada la conversación sin necesariamente 400,
porque esos backends no validan el orden — pero el contexto queda revuelto.

#### N-1 · [ALTA] El corte ciego de `KEEP_RAW_TAIL` parte pares tool_use/tool_result
- **Ubicación:** `crates/braze-engine/src/engine.rs:956-958` (+ `history.rs:110-117`).
- **Corroborado por:** ambas corridas del engine (E-1 en las dos).
- **Escenario:** una ronda con 3 tool calls termina el log en
  `[ATC1,TCS1,ATC2,TCS2,ATC3,TCS3,TCC1,TCC2,TCC3]`. Si el `load_messages`
  post-dispatch dispara compactación, `live_tail = &tactical[len-6..]` deja
  `TCC1`/`TCC2` como `tool_result` cuyo `tool_use` quedó plegado en el resumen
  (solo prosa) → Anthropic rechaza el `tool_result` huérfano con 400. Se
  repite en cada compactación que caiga en medio de una ronda multi-tool.
- **Fix:** hacer el corte del tail *pair-aware* — extender el slice hacia
  atrás hasta incluir el `AssistantToolCall` de todo `ToolCallCompleted`
  presente en el tail, o excluir del tail el resultado huérfano.
- **✅ RESUELTO 2026-07-05.** `pair_aware_tail_start` (`engine.rs`, cerca de
  `merge_summary`) extiende `start` hacia atrás en un loop de punto fijo
  hasta que ningún `AssistantToolCall` anterior al corte tenga su
  `ToolCallCompleted` dentro de la cola — ver su doc comment para el
  análisis de por qué una sola pasada no basta (la extensión puede arrastrar
  más pares). Combinado con el fix de N-1b (abajo). Test
  `engine::tests::compaction_tail_cut_can_orphan_a_tool_result` pasó de rojo
  a verde sin tocar su aserción.

#### N-1b · [ALTA, hallazgo nuevo descubierto al implementar el fix de N-1] Cualquier ronda con 2+ tool calls concurrentes ya renderiza `tool_use`/`tool_result` no-adyacentes, con o sin compactación
- **Ubicación:** `crates/braze-engine/src/engine.rs` (`dispatch_tool_calls`,
  el loop que apendea `AssistantToolCall`/`ToolCallStarted` de **todas** las
  llamadas del round antes de despachar ninguna) + `history.rs` (mapeo
  1-evento-1-mensaje original).
- **Cómo se descubrió:** al implementar el fix de N-1 y correr el test de
  regresión contra el validador de protocolo (`protocol_check.rs`, la
  precondición del Grupo I), el pair-aware tail cut por sí solo dejó de
  producir `OrphanedToolResult` pero empezó a fallar con
  `ToolResultNotAdjacent` — el `tool_result` de `call-1` no era el mensaje
  inmediatamente siguiente a su `tool_use`, porque entre medio había OTROS
  `tool_use` (call-2, call-3) del mismo round.
- **Escenario:** `dispatch_tool_calls` persiste, para un round con 3 tool
  calls, `[ATC1,TCS1,ATC2,TCS2,ATC3,TCS3]` (todas las llamadas) **antes** de
  despachar ninguna, y recién después las `ToolCallCompleted` llegan en
  orden de finalización — este shape existe siempre, no solo cuando
  compacta. Con el mapeo histórico (un evento → un mensaje), esto renderiza
  como 3 mensajes `Assistant` separados (uno por `tool_use`) seguidos de 3
  mensajes `User` separados (uno por `tool_result`) — el mensaje
  inmediatamente después del primer `tool_use` es *otro* `tool_use`, no su
  respuesta, y Anthropic rechaza eso. El propio comentario de diseño de
  `history.rs` (Fase 5) ya reconocía esto como una asunción sin verificar
  ("MVP simplification... still a valid sequence... revisit if strict
  role-alternation becomes a real constraint") — la auditoría de tres
  agentes independientes convergiendo en "el tool_result debe ser el
  mensaje inmediatamente siguiente" indica que la asunción era optimista.
- **Fix — ✅ RESUELTO 2026-07-05:** `history.rs` agrupa ahora eventos
  `ToolUse`/`ToolResult` consecutivos del mismo tipo en un solo `Message`
  con múltiples `ContentBlock`s (`push_grouped`/`event_to_block`), en vez de
  un mensaje por evento — exactamente como el propio comentario de diseño
  describía el formato real de Anthropic. Esto elimina la ambigüedad de raíz
  para cualquier round concurrente, con o sin compactación de por medio. Los
  mensajes de texto plano (`UserMessage`/`AssistantText`) NO se agrupan —
  conservan su semántica 1:1 existente, sin afectar tests previos salvo
  ajustes de índice donde corresponde. Nuevo test de regresión:
  `history::tests::concurrent_tool_calls_in_one_round_group_into_one_message_each_role`.

#### N-2 · [ALTA→CRÍTICA] `split`+`build_messages` reordenan: durables por encima de tácticos más viejos → primer mensaje `assistant` → 400 en la banda 21-40 eventos
- **Ubicación:** `crates/braze-session/src/simple_compactor.rs:165-191` +
  `crates/braze-engine/src/history.rs:51-73`.
- **Escenario:** sesión con defaults, aún sin compactar (`summary` vacío),
  con >20 eventos. Los eventos más viejos que la ventana se reparten: los
  "settled" (`AssistantToolCall`/`ToolCallCompleted`/`PermissionDecided`) van
  a `durable_events`; los huérfanos (`UserMessage`, `AssistantText`) se quedan
  en `tactical`. Pero `build_messages` renderiza **todos los durables antes de
  todos los tácticos**, aunque en el log los huérfanos los precedan. En cuanto
  el primer par tool sale de la ventana, el primer mensaje del request pasa a
  ser `assistant[tool_use]` → Anthropic 400 ("first message must use the user
  role" / tool_result huérfano). **Toda sesión con tool calls cruza esta banda
  (~eventos 21-40) obligatoriamente** antes de que la primera compactación
  (que antepone el summary como mensaje `user`) la "arregle". No se detectó en
  vivo porque el sweep promedia <20 eventos.
- **Impacto:** el más amplio de todos — no requiere condiciones de carrera ni
  crashes, solo que la sesión llegue a tamaño medio.
- **Fix:** renderizar en orden de log (anotar eventos con su índice original y
  hacer merge posicional en `build_messages`, o tratar como durables-in-place
  los huérfanos previos al primer durable). Agregar un test que construya el
  request de una sesión de 25 eventos con tool call temprana y asserte que el
  primer mensaje es `user`.
- **✅ RESUELTO 2026-07-05, con un fix más acotado que el propuesto.**
  Reordenar `durable_events` vs `tactical` en orden de log real requeriría
  que `ContextCompactor::split` — un trait congelado — expusiera índices
  posicionales que hoy descarta; en vez de tocar ese contrato,
  `build_messages_with_never_clear` antepone un mensaje `User` placeholder
  ("[Contexto previo] ...") siempre que `durable_events` sea no-vacío,
  incluso con `summary` vacío — garantiza la única regla que este hallazgo
  realmente violaba (primer mensaje `role: user`) sin reordenar nada. Test
  `history::tests::build_messages_keeps_log_order_even_when_summary_is_still_empty`
  pasó de rojo a verde; dos tests preexistentes
  (`durable_tool_result_is_cleared_but_tool_use_is_preserved`,
  `never_clear_list_exempts_only_the_named_tool`) se actualizaron para
  reflejar el mensaje adicional (índices +1).

#### N-2b · [ALTA→CRÍTICA, hallazgo nuevo, encontrado en vivo contra Anthropic real] Un par tool_use/tool_result puede quedar partido entre `durable_events` y `tactical` cuando la ventana cae justo entre ambos
- **Ubicación:** `crates/braze-session/src/simple_compactor.rs` (`split`,
  clasificación de `AssistantToolCall` como "settled" sin mirar dónde cae
  su `ToolCallCompleted`).
- **Cómo se descubrió:** verificación end-to-end 2026-07-05 contra
  **Anthropic real** (`claude-haiku-4-5-20251001`, la única prueba de esta
  serie con validación estricta de protocolo) — pedir al modelo leer dos
  archivos "de una vez" (tool calls concurrentes) con
  `tactical_window`/`tactical_compaction_threshold` bajos produjo un 400
  real: `messages.4: tool_use ids were found without tool_result blocks
  immediately after`. Ninguno de los tests unitarios previos (incluidos los
  de N-1b) lo cubría porque siempre probaban el par completo *fuera* de la
  ventana o completo *dentro* — nunca el caso borde de un `AssistantToolCall`
  viejo cuyo `ToolCallCompleted` seguía dentro de la ventana.
- **Escenario:** `split` decide "settled" (→ `durable_events`) para todo
  `AssistantToolCall` con índice `< window_start`, sin verificar si su
  `ToolCallCompleted` sigue con índice `>= window_start` (aún dentro de la
  ventana). Con una ronda de 2+ tool calls concurrentes (`dispatch_tool_calls`
  persiste todas las `AssistantToolCall` antes de despachar ninguna — el
  mismo patrón de N-1b), el índice del `tool_use` puede quedar *antes* del
  corte de ventana mientras su `tool_result` queda *después* — el `tool_use`
  se renderiza en el bloque `durable_events` (temprano), su `tool_result`
  se renderiza más tarde en `tactical`, con contenido no relacionado (el
  siguiente `UserMessage`) entre medio. Reproducible incluso con la ventana
  default (20) en una ronda con suficientes tool calls concurrentes o
  suficiente distancia entre eventos — el test de esta sesión solo lo hizo
  trivial de disparar con una ventana artificialmente angosta.
- **Fix — ✅ RESUELTO 2026-07-05.** `split` calcula ahora
  `completed_ids_in_window` (ids de todo `ToolCallCompleted` con índice
  `>= window_start`); un `AssistantToolCall` cuyo id está en ese conjunto
  ya NO se clasifica "settled" aunque su propio índice sea viejo — cae al
  mismo camino que cualquier evento huérfano no cubierto (se queda en
  `tactical`, en orden original, junto a su resultado). Test
  `simple_compactor::tests::split_keeps_a_tool_use_with_its_result_even_when_the_call_alone_would_be_old_enough_for_durable`
  — verificado revirtiendo el fix para confirmar que sin él el test falla
  exactamente como se predijo. **Re-ejecutada la sesión exacta que había
  dado el 400 real contra Anthropic tras el fix: completa sin error.**

#### N-3 · [ALTA] "Apagón" del summary: el `CompactionOccurred` recién persistido es invisible mientras está en la ventana táctica
- **Ubicación:** `crates/braze-session/src/simple_compactor.rs:165-177` +
  `crates/braze-engine/src/history.rs:122-127`.
- **Corroborado por:** ambas corridas del engine (E-2).
- **Escenario:** `load_messages` compacta y arma *ese* request con el summary
  correcto. Pero el `CompactionOccurred` queda como el evento más nuevo del
  log: en los siguientes `split()` cae *dentro* de la ventana de 20 → se
  mapea a `None` (history.rs:122) y `summary_parts` solo recoge compactions
  con índice `< window_start`. Durante ~20 eventos (varias rondas),
  `durable.summary` está vacío y **todo el contexto plegado —incluida la
  pregunta original del usuario— desaparece del prompt.** Es la falla A1
  reintroducida como ventana temporal, silenciosa, sin error.
- **Fix:** en `split()`, recoger también en `summary_parts` los
  `CompactionOccurred` dentro de la ventana (manteniendo su render como
  `None`), o renderizar un `CompactionOccurred` táctico como su texto de
  resumen.
- **✅ RESUELTO 2026-07-05.** `SimpleContextCompactor::split` ahora cosecha
  el summary de todo `CompactionOccurred`, esté o no dentro de la ventana
  (rama `i >= window_start`) — sin duplicar nada, porque un
  `CompactionOccurred` nunca se renderiza como bloque de mensaje de todas
  formas. Test `simple_compactor::tests::a_compaction_still_inside_the_window_still_contributes_its_summary`.

#### N-4 · [ALTA→CRÍTICA] La reparación de huérfanos (fix C4) persiste el resultado *después* del `UserMessage` nuevo → orden inválido permanente
- **Ubicación:** `crates/braze-engine/src/engine.rs:397-406` (orden en
  `run_turn`) + `engine.rs:979-1022` (repair al final del log).
- **Corroborado por:** ambas corridas del engine (E-4) y la sesión (S-1) —
  triple corroboración.
- **Escenario:** el proceso muere (o el usuario interrumpe con Esc en la TUI,
  `app.rs:703-707` hace `handle.abort()`) entre `AssistantToolCall` y su
  `ToolCallCompleted`. Al resumir, `run_turn` apendea **primero** el
  `UserMessage` nuevo y **recién después** `repair_orphaned_tool_calls`
  apendea el `ToolCallCompleted` sintético al final. El log queda
  `[..., ATC(huérfano), UserMessage, TCC(repair)]` → se renderiza
  `assistant[tool_use] → user[text] → user[tool_result]`. Anthropic exige que
  el `tool_result` esté en el mensaje inmediatamente siguiente al `tool_use`:
  400 permanente en disco. Los tests de C4 usan `ScriptedModel` (no valida
  orden), por eso pasan.
- **Fix:** ejecutar la reparación **antes** de apendear el `UserMessage` del
  turno (cargar+reparar, luego apendear el mensaje del usuario).
- **✅ RESUELTO 2026-07-05.** `run_turn` ahora llama a `Engine::repair_session`
  (nuevo método, factoriza el load+repair que antes vivía inline en
  `load_messages` en un helper compartido `load_and_repair`) **antes** de
  apendear el `UserMessage` del turno; `load_messages` sigue reparando
  también (idempotente) para cualquier otro caller directo. Test
  `engine::tests::resuming_after_a_crash_with_an_orphaned_tool_call_stays_protocol_valid`
  pasó de rojo (`ToolResultNotAdjacent{expected:#2, actual:#3}`) a verde.

#### N-5 · [ALTA] El fix C5 tolera la línea truncada al leer pero nunca repara el archivo → el siguiente `append` la pega al fragmento → corrupción dura permanente
- **Ubicación:** `crates/braze-session/src/file_store.rs:149-175` (tolerancia
  solo en lectura) + `file_store.rs:96-109` (`append` con `O_APPEND`, sin
  verificar que el archivo termine en `\n`).
- **Escenario:** (1) crash mid-`write_all` deja `{"type":"assist` sin newline.
  (2) `--resume` descarta esa línea con warning — la sesión funciona. (3) El
  primer append nuevo pega el evento al fragmento →
  `{"type":"assist{"type":"user_message",...}\n`, una sola línea malformada.
  (4) Siguiente reinicio: esa línea ya no es la última → `load` falla duro →
  `SessionError::Read` para siempre. C5 convirtió una corrupción permanente
  inmediata en una diferida un turno.
- **Fix:** al descartar la línea truncada en `load`, truncar el archivo al
  final de la última línea válida (`set_len`); o en `append` verificar que el
  archivo termine en `\n` (anteponer `\n` defensivo si el último byte no lo
  es).
- **✅ RESUELTO 2026-07-05.** Al descartar la línea final truncada, `load`
  ahora reescribe el archivo (`tokio::fs::write`) con solo las líneas
  válidas — el siguiente `append` empieza limpio. Corre bajo el mismo
  `write_lock` que N-7 introduce en `load` (ver abajo), así que la
  reparación queda serializada frente a cualquier escritura concurrente.
  Test `file_store::tests::load_repairs_the_file_after_a_truncated_final_line_not_just_tolerates_it`
  (verifica el archivo en disco, no solo la vista en memoria, y que una
  segunda reanudación con un tercer evento siga funcionando).

#### N-6 · [ALTA] Modo compactación permanente reintroducido por el budget de tokens: el estimador cuenta payloads que el renderer limpia y la compactación no puede reducir `durable_events`
- **Ubicación:** `crates/braze-engine/src/engine.rs:1109-1114` (estimador) vs
  `history.rs:146-175` (clearing); disparo en `engine.rs:921-953`; producción
  en `braze-cli/src/main.rs:414-420`.
- **Corroborado por:** ambas corridas del engine (E-3) y la sesión (S-5).
- **Escenario:** backend Ollama (budget = `num_ctx − max_tokens − margen`,
  **activo por default**). Un `read_file` de 200KB se asienta en
  `durable_events`; `estimate_prompt_tokens` lo cuenta con el `Debug` repr del
  evento crudo (~50K tokens) aunque `event_to_message_cleared` lo enviará como
  placeholder de una línea. `compact_tactical` solo pliega la táctica, nunca
  reduce `durable_events`, así que `over_token_budget` queda `true` para
  siempre: cada `load_messages` (≥2 por ronda) dispara compactación, persiste
  un `CompactionOccurred` nuevo, el log crece sin límite y el prompt nunca baja
  del budget. Es la patología A2/C2 reintroducida por la vía del presupuesto.
- **Fix:** estimar `durable_events` sobre la forma *renderizada* (resultado
  limpiado), y no disparar compactación cuando `tactical.len() <=
  KEEP_RAW_TAIL` o cuando plegar la táctica no puede reducir el estimado.
- **✅ RESUELTO 2026-07-05.** Nuevo `history::render_durable_events`
  (`pub(crate)`) renderiza `durable_events` a través de la misma lógica de
  clearing que usa `build_messages`; `estimate_prompt_tokens` mide esa
  forma renderizada en vez del `Debug` crudo. Además, `load_messages` ahora
  exige `compaction_would_help = tactical.len() > KEEP_RAW_TAIL` junto a
  los umbrales existentes — si `tactical` ya cabe entero en la cola cruda
  que se conserva siempre, compactar no reduce nada y solo añadiría un
  `CompactionOccurred` redundante en cada llamada. Tests:
  `engine::tests::a_large_settled_tool_result_does_not_permanently_blow_the_token_budget`
  (el caso de N-6) y `a_single_oversized_event_triggers_compaction_via_the_token_budget`
  actualizado para seguir siendo significativo bajo el nuevo gate.

#### N-7 · [ALTA] Carrera `load` (cache fría) vs `append` vía Ctrl+T → cache sembrada con lectura rasgada → repair duplica el `tool_result` → 400 permanente
- **Ubicación:** `crates/braze-session/src/file_store.rs:126-181` (`load` no
  toma `write_lock`) + `crates/braze-tui/src/app.rs:259-264` (Ctrl+T permitido
  con `turn_running`, mismo `Arc` de store que el engine).
- **Escenario:** turno en curso, engine mid-`append` de `ToolCallCompleted` X;
  el usuario presiona Ctrl+T con cache fría; `read_to_string` observa la línea
  de X a medio escribir → la tolerancia la descarta silenciosamente →
  `cache.insert` deja la cache **sin X** aunque X sí terminó en disco. El
  siguiente `load_messages` ve el `AssistantToolCall` de X sin resultado y
  **persiste un segundo `ToolCallCompleted`** para el mismo id → dos
  `tool_result` en disco → 400 permanente.
- **Fix:** que el camino frío de `load` tome `write_lock` durante la lectura
  (y re-chequee la cache al adquirirlo).
- **✅ RESUELTO 2026-07-05.** El camino frío de `load` ahora adquiere el
  mismo `write_lock` que `append` sostiene durante toda su escritura, y
  re-chequea la cache al adquirirlo (por si otro `load` la calentó
  mientras esperaba). Costo cero para el camino caliente (la mayoría de
  las llamadas retornan antes de tocar el lock). Test
  `file_store::tests::cold_load_waits_for_an_in_flight_append_instead_of_racing_it`
  — verificado que falla (`NotFound`) sin el fix revirtiéndolo
  temporalmente, y pasa con él.

### Seguridad — capacidades destructivas enmascaradas de "read-only"

#### N-8a · [ALTA] Comandos allowlisted "read-only" que igual escriben archivos: `find -fls` y `git diff --output=`
- **Ubicación:** `crates/braze-permissions/src/classifier.rs:170-177`
  (`is_safe_find`) y `:183-189` (`is_safe_git`).
- **Corroborado por:** seguridad (Finding 1) y bench (Finding 1) — doble.
- **Escenario:** `MUTATING_PRIMARIES` bloquea `-delete/-exec/-fprint*` pero
  **omite `-fls`**; GNU `find … -fls FILE` escribe (truncando) `FILE`.
  `is_safe_git` admite `git diff` "con cualquier argumento", pero
  `git diff --output=FILE` escribe a un archivo arbitrario. Ambos se
  clasifican `Reversible` → **sin prompt**. Exploit:
  `shell_exec ["find",".","-fls","/home/user/.../.git/config"]` trunca
  `.git/config`; `shell_exec ["git","diff","--output=/home/user/.bashrc","HEAD"]`
  sobrescribe `~/.bashrc`. Es la misma clase que E2 supuestamente cerró.
- **Fix:** agregar `-fls` a `MUTATING_PRIMARIES`; rechazar `git diff|log|show`
  con `-O`/`--output` (y considerar `--ext-diff`, que ejecuta un driver de
  config).

#### N-8b · [ALTA] `shell_exec` lee cualquier ruta del filesystem sin prompt, saltándose el read-gate D7
- **Ubicación:** `classifier.rs:130-131` (`cat|head|tail|grep|file|diff`
  incondicionalmente safe) vs `braze-tools-local/src/provider.rs:107-123`
  (`invoke_shell_exec` no hace check de ruta), en contraste con `check_read`.
- **Escenario:** el fix D7 gateó `read_file`/`grep`/`glob` por `ReadPath` +
  `WorkdirAllowlist` (leer `~/.ssh/id_rsa` prompta), pero la lectura idéntica
  vía `shell_exec ["cat","/etc/shadow"]` / `["grep","-r","AWS","/home/user"]`
  / `["find","/home","-name","*.pem"]` devuelve el secreto al contexto del
  modelo con cero confirmación. Combinado con descripciones de tools MCP no
  sanitizadas (D6), es un primitivo de exfiltración funcional. Las dos capas
  son inconsistentes: las file tools imponen el límite del workdir, el
  allowlist de shell no.
- **Fix:** path-gate los comandos read-capable del allowlist, o un denylist de
  rutas sensibles aplicado al argv de shell.

### Backends

#### N-9 · [ALTA] Tool call con argumentos vacíos se dropea en silencio (Anthropic y OpenRouter)
- **Ubicación:** `anthropic_wire.rs:430-445` (`finalize_tool_call`) y
  `openrouter_wire.rs:411-426`.
- **Corroborado por:** ambas corridas de backends — doble.
- **Escenario:** para un `tool_use` sin argumentos, Anthropic streamea
  `content_block_start {input:{}}` y **cero** `input_json_delta` (o uno con
  `partial_json:""`); los SDKs oficiales tratan el buffer vacío como `{}`.
  Aquí `serde_json::from_str("")` falla → el tool call se dropea con solo un
  `tracing::error!`, el stream termina con `Done` limpio y `stop_reason:
  "tool_use"`, y el engine persiste el texto parcial como respuesta final: el
  turno "converge" sin haber ejecutado la llamada. Ollama es inmune (default a
  `{}`). Gatillo real: cualquier tool MCP sin parámetros requeridos; no lo vio
  el sweep porque las tools locales todas exigen args.
- **Fix:** en ambos `finalize_tool_call`, `if buf.trim().is_empty() {
  arguments = json!({}) }`.
- **✅ RESUELTO 2026-07-05 (Grupo K).** Ambos `finalize_tool_call` (Anthropic
  y OpenRouter) tratan el buffer vacío/solo-whitespace como `{}` antes de
  intentar parsearlo — un JSON genuinamente inválido (no vacío) sigue
  fallando como antes. Tests
  `anthropic_wire::tests::finalize_tool_call_treats_{an_empty,a_whitespace_only}_buffer_as_an_empty_object`,
  `openrouter_wire::tests::finalize_tool_call_treats_empty_arguments_as_an_empty_object`.

### TUI (Fase 2) — código nuevo, no auditado antes

#### N-10 · [ALTA] Paste multilínea dispara submits prematuros — bracketed paste nunca se habilita, `Event::Paste` se descarta
- **Ubicación:** `crates/braze-tui/src/terminal.rs:47-57` (sin
  `EnableBracketedPaste`) + `app.rs:232` (`Some(Ok(_)) => {}` descarta
  `Event::Paste`).
- **Escenario:** en el composer idle, pegar 3 líneas → la línea 1 se envía
  instantáneamente como turno (cada `\r` es `KeyCode::Enter`); el resto se
  teclea mientras `turn_running`. Amplificador: si el texto pegado contiene
  `@` o empieza con `/`, el popup se abre a mitad de paste y un Tab/Enter
  pegado *acepta una completación de ruta aleatoria*. Silencioso, común, y
  manda contenido no intencionado a una API remota. La feature flag
  `bracketed-paste` está compilada pero nunca cableada — peor de dos mundos:
  si el terminal fuerza paste-bracketing, el paste se descarta entero.
- **Fix:** `EnableBracketedPaste` en el setup + manejar `Event::Paste` como
  inserción literal.
- **✅ RESUELTO 2026-07-06.** `terminal::setup`/`TerminalGuard::drop`
  ejecutan `EnableBracketedPaste`/`DisableBracketedPaste`; el loop principal
  maneja `Event::Paste(text)` con `App::on_paste`, que hace
  `composer.insert_str(&text)` en un solo edit atómico (`insert_str` ya
  parte `\n`/`\r\n` en líneas reales de composer, nunca un submit) y respeta
  el mismo gate que el tipeo normal (ignorado con una aprobación pendiente).
  Sin test de `App` completo (ver nota de alcance al final de este grupo).

#### N-11 · [ALTA] El walk de @-menciones cuelga la TUI para siempre en un ciclo de symlinks; sin cota de profundidad; corre síncrono en el event-loop
- **Ubicación:** `crates/braze-tui/src/mentions.rs:36-67`.
- **Escenario:** `path.is_dir()` sigue symlinks; no hay visited-set ni límite
  de profundidad; el cap `MAX_FILES=5000` solo cuenta archivos. Un ciclo de
  symlinks de directorios sin archivos regulares dentro (`mkdir a; ln -s ../a
  a/self`) → teclear un `@` entra en loop infinito con `stack` creciendo sin
  cota, en el hilo del event-loop → la UI se congela y **Ctrl+C queda muerto**
  (el select loop nunca vuelve a correr); solo SIGKILL recupera.
- **Fix:** walk symlink-aware con visited-set/cota de profundidad, o moverlo a
  una `spawn_blocking`.
- **✅ RESUELTO 2026-07-06.** `list_files` usa `entry.file_type()`
  (equivalente a `lstat`, nunca sigue symlinks) en vez de `path.is_dir()`
  (equivalente a `stat`, sí los sigue) — un symlink nunca se desciende,
  garantizando terminación sin necesitar visited-set ni cota de profundidad
  (mismo default que ripgrep/fd). Test
  `mentions::tests::a_directory_symlink_cycle_does_not_hang_the_walk`
  (corre en un thread aparte con `recv_timeout` para fallar en vez de
  colgar el binario de test si regresiona) — verificado revirtiendo el fix
  para confirmar que el test lo detecta. La preocupación secundaria del
  hallazgo (el walk corre síncrono en el hilo del event-loop incluso para
  un árbol grande sin ciclos) queda sin resolver — mitigada solo por el cap
  `MAX_FILES=5000` ya existente, no por mover el walk a `spawn_blocking`.

#### N-12 · [ALTA] Backtrack desincroniza la persistencia de permisos: los eventos de aprobación se apendean a la sesión pre-backtrack para siempre
- **Ubicación:** `braze-cli/src/main.rs:118` (construye
  `ChannelConfirmationPrompt` una vez con el `SessionId` inicial) +
  `braze-tui/src/approval.rs:55-99` + `app.rs:592-604` (`backtrack_to` cambia
  `App::session` a un id fresco).
- **Escenario:** aprobar un `write_file`, Esc-Esc-backtrack, continuar,
  aprobar otra acción, salir → (a) `--resume <sesión-nueva>` no encuentra
  ningún `PermissionDecided`, re-pregunta todo, y el log nuevo es un audit
  trail incompleto; (b) la sesión *original* — que el diseño promete "queda
  intacta y `--resume`-able" — queda contaminada con eventos de permiso de
  turnos que no están en su historia. Viola el principio "el rollout log es la
  única fuente de verdad".
- **Fix:** threading del id de sesión vivo (`Arc<Mutex<SessionId>>`) al
  `ChannelConfirmationPrompt`, o reconstruir los prompts en el backtrack.
- **✅ RESUELTO 2026-07-06,** exactamente como se propuso. `ChannelConfirmationPrompt::session`
  pasó de `SessionId` a `Arc<Mutex<SessionId>>`, leído fresco en cada
  `confirm()` (`current_session()`); `App` gana el campo `live_session`
  (la misma instancia del `Arc`) y `backtrack_to` escribe en él junto con
  `self.session`; `braze-cli::build_permission_guard` construye el `Arc`
  una vez y lo pasa a cada `PermissionGuard` (local + uno por servidor MCP)
  y a `braze_tui::run`. El camino plano (`TerminalConfirmationPrompt`, sin
  backtrack) sigue recibiendo un `SessionId` plano, leído una sola vez del
  mismo `Arc`. Test
  `approval::tests::persists_against_whatever_session_the_shared_handle_points_to_now`
  (muta el handle compartido entre la creación del prompt y la llamada a
  `confirm()`, confirma que persiste en la sesión nueva y que la original
  queda con `SessionError::NotFound`).

---

## 3. Hallazgos medios

### Loop agéntico / engine
- **N-13 · [MEDIA] Best-of-n (G10) es todo-o-nada ante el error de un
  candidato** (`engine.rs:326-334`, `?` dentro del loop): un `StreamError`
  transitorio en el candidato 3 aborta el turno y descarta los 2 candidatos
  válidos ya pagados — multiplica ~N× la probabilidad de perder el turno,
  contradiciendo el propósito de G10. Corroborado por ambas corridas. *Fix:*
  votar entre los exitosos, fallar solo si fallan todos.
- **N-14 · [MEDIA] Sin guarda de unicidad de `tool_use` ids** (`engine.rs:604-614`):
  ids duplicados (OpenRouter con `id:""`, u Ollama con contador de proceso tras
  `--resume`) entran al log append-only → dos `tool_use`/`tool_result` con el
  mismo id → 400 permanente. Corroborado por engine (×2), backends y sesión.
  *Fix:* verificar unicidad en `complete_once` y re-sufijar con nonce.
  **✅ RESUELTO 2026-07-05.** `ensure_unique_tool_call_id` (en
  `dispatch_tool_calls`, no en `complete_once` — más simple: es donde el
  `ToolCall` ya se clona antes de persistir/despachar) re-sufija con
  `-dupN` cualquier id que colisione contra `known_tool_call_ids`, sembrado
  desde el historial de la sesión (`Engine::existing_tool_call_ids`) y
  actualizado con cada id nuevo del turno. El estado per-turno se agrupó en
  `TurnDispatchState` para no exceder el límite de argumentos de clippy.
  Test `engine::tests::duplicate_tool_use_ids_in_one_round_are_renamed_to_stay_unique`.
- **N-15 · [MEDIA] El rescate textual de tool calls (B5) ejecuta una tool que
  el modelo solo *mostraba* como ejemplo** (`engine.rs:274-283`): pedir
  "muéstrame el JSON para invocar write_file" hace que el modelo emita ese
  JSON, `try_parse_textual_tool_call` lo convierte en llamada real y la
  ejecuta (las tools de lectura ni pasan por confirmación). *Fix:* gatear tras
  flag de config y/o no limpiar `text_buffer` cuando el nombre no resuelve.
- **N-16 · [MEDIA] `all_stubs()` sigue fail-fast (A8 abierto)**
  (`engine.rs:423` + `tools-core/registry.rs:44`): un provider MCP caído a
  mitad de sesión brickea *todos* los turnos, incluso los que solo usan tools
  locales. *Fix:* `all_stubs_lossy()` que degrade con warn.
- **N-17 · [MEDIA] Dos `run_turn` concurrentes sobre el mismo `Engine` se
  roban las completions** por el canal compartido del notifier
  (`engine.rs:813-835`): el turno A descarta la completion de B como "stale",
  B espera el timeout y persiste un error falso. Latente (CLI/TUI secuencial)
  pero sin guarda ni doc. Corroborado por ambas corridas. *Fix:* canal por
  ronda, o `&mut self`.

### Backends
- **N-18 · [MEDIA] OpenRouter: `[DONE]` sin `finish_reason` previo descarta
  tool calls acumulados en silencio** (`openrouter_wire.rs:395-405`). Doble
  corroboración. *Fix:* drenar `finalize_tool_calls()` en `handle_done_sentinel`.
  **✅ RESUELTO (Grupo K).** `handle_done_sentinel` drena `finalize_tool_calls()`
  antes de emitir `Usage`/`Done`. Test
  `done_sentinel_without_prior_finish_reason_still_emits_accumulated_tool_calls`.
- **N-19 · [MEDIA] OpenRouter: `resize_with` con índice del proveedor sin
  cota** (`openrouter_wire.rs:341-347`): un chunk con `{"index":4e9}` fuerza
  una alocación de decenas de GB → abort. Anthropic usa `HashMap` y es inmune.
  Doble corroboración. *Fix:* cota (p.ej. 128) o `HashMap`.
  **✅ RESUELTO (Grupo K).** `MAX_TOOL_CALL_INDEX = 128`; un fragmento con
  índice mayor se ignora con `warn!` en vez de redimensionar. Test
  `accumulate_tool_call_fragment_ignores_an_implausibly_large_index`.
- **N-20 · [MEDIA] Sin timeouts HTTP en los tres backends** (`anthropic.rs`,
  `ollama.rs`, `openrouter.rs`: `reqwest::Client::new()` sin timeout): un
  servidor que acepta la conexión y luego se cuelga bloquea el agente para
  siempre. Doble corroboración. *Fix:* `connect_timeout` + timeout de idle por
  chunk (no total).
  **✅ RESUELTO (Grupo K).** Nuevo `http_client.rs` compartido:
  `connect_timeout=10s`, `read_timeout=600s` (se resetea en cada lectura
  exitosa — no es un timeout total, no interrumpe una generación lenta pero
  sana; 600s da margen sobre el peor caso documentado de Ollama CPU-only,
  180-400s por turno). Los tres backends usan `crate::http_client::build_client()`
  en vez de `reqwest::Client::new()`. Test
  `a_connection_that_never_responds_is_cut_off_by_the_read_timeout` (con
  duraciones cortas inyectables para no esperar los 600s reales).
- **N-21 · [MEDIA] OpenRouter: tool call sin `id` → `id:""` → pairing roto**
  (`openrouter_wire.rs:373`, `unwrap_or_default()`). *Fix:* sintetizar id como
  Ollama.
  **✅ RESUELTO (Grupo K).** `finalize_tool_calls` sintetiza
  `openrouter-tool-call-{n}` (mismo patrón que `ollama_wire.rs`) cuando el id
  es `None` o vacío. Test
  `finalize_tool_calls_synthesizes_an_id_when_the_provider_never_sent_one`.
- **N-22 · [MEDIA] OpenRouter: `finish_reason:"error"` se trata como parada
  normal** (`openrouter_wire.rs:332-335`) → texto parcial de una generación
  fallida persistido como respuesta final. *Fix:* setear `stream_error`.
  **✅ RESUELTO (Grupo K).** `finish_reason == "error"` setea `stream_error`
  en vez de `stop_reason` normal, y no finaliza tool calls como si la ronda
  hubiese terminado con éxito. Test
  `stream_state_finish_reason_error_sets_stream_error_not_a_normal_stop`.
- **N-23 · [MEDIA] Ollama: tool result sin correlación en el caso no-error**
  (`ollama_wire.rs:181-193`): el `tool_use_id` solo se incrusta cuando
  `is_error`; con 2+ tool calls exitosas, el modelo recibe dos mensajes `tool`
  sin marca de cuál es cuál → atribución cruzada (era el modo de falla B10).
- **N-24 · [MEDIA] Truncamiento por `max_tokens`/`length` persiste el turno
  truncado como respuesta final** (`engine.rs:227-234`, solo `warn!`): residuo
  de A3/B3 por el canal de budget de tokens (no de stream muerto). *Fix:*
  convertir en error o retry-con-budget-mayor cuando `stop_reason` es
  truncación y hay 0 tool calls.

### Sesión
- **N-25 · [MEDIA] `durable.summary` crece linealmente sin cota**
  (`simple_compactor.rs:174-176,194`): N compactaciones concatenan N digests
  (cada uno con el trailer de instrucciones repetido) en *cada* request. El
  fix sugerido en la auditoría previa ("conservar solo el último summary") no
  se implementó. Corroborado por engine (E-11) y sesión (S-6). *Fix:*
  conservar los últimos K, o meta-summary con cap.
- **N-26 · [MEDIA] Backtrack replica un `tool_use` huérfano sin su reparación**
  (`app.rs:573-604`): backtrack a un `UserMessage` cuyo prefijo incluye un
  `ATC` huérfano pero no su `TCC-repair` (posterior al corte) → sesión nueva
  envenenada. Se vuelve casi gratis de arreglar con N-4. *Fix:* sintetizar el
  `ToolCallCompleted` de error tras cada `ATC` huérfano al replicar.
- **N-27 · [MEDIA] Single-writer asumido pero no forzado** (`file_store.rs:27-39`):
  doble `--resume` de la misma sesión → reparaciones duplicadas → 400
  permanente. *Fix:* lockfile advisory (`flock`) por sesión.

### TUI
- **N-28 · [MEDIA] Popup de backtrack secuestra Enter y destruye el draft del
  composer** (`app.rs:281-366`): Esc-Esc por hábito vim, seguir tecleando,
  Enter → rewind + draft descartado + cambio silencioso de sesión.
  `KeyEventKind::Repeat` no se filtra, así que *mantener* Esc para interrumpir
  auto-repite y abre el popup.
  **✅ RESUELTO 2026-07-06.** Dos cambios: (1) el brazo de Esc idle ahora
  exige `key.kind == KeyEventKind::Press` — un `Repeat` (de mantener Esc
  presionado, p.ej. para interrumpir) ya no puede armar ni completar el
  doble-tap; (2) a diferencia de Slash/Mention (que sí tienen una query que
  la tipeada sigue afinando), el popup de Backtrack no tiene query alguna —
  cualquier tecla que no sea Up/Down/Tab/Enter/Esc ahora lo cierra en vez de
  dejarlo abierto en silencio bajo lo que el usuario sigue tecleando, así
  que un Enter posterior envía el draft normalmente en vez de ser
  secuestrado como "aceptar esta selección de backtrack".
- **N-29 · [MEDIA] Aprobaciones stale tras `interrupt_turn` bloquean el
  composer y pueden ejecutar una tool del turno abandonado** (`app.rs:709-721`):
  las tasks de dispatch no se abortan; una puede llamar `confirm()` tras el
  drain → overlay de aprobación de un turno ya matado; `y` corre la tool.
  **✅ RESUELTO 2026-07-06.** Una request de aprobación legítima solo llega
  mientras su propio turno sigue `turn_running`; el brazo
  `self.approval_rx.recv()` del loop principal ahora deniega inmediatamente
  (`respond.send(false)`) cualquier request que llegue estando idle, en vez
  de encolarla — sin necesidad de un contador de generación ni de cancelar
  las tasks de dispatch (fuera de alcance, requeriría cambios en
  `Engine`/`TaskNotifier`).
- **N-30 · [MEDIA] Un turno que erra mid-stream pierde la cola streameada del
  transcript y la deja congelada en el preview** (`app.rs:902-912`, no llama
  `markdown.finish()`).
  **✅ RESUELTO 2026-07-06.** El brazo `TurnFinished(Err(..))` llama
  `self.markdown.finish()` y commitea el tail (si hay) antes del `ErrorCell`
  — mismo patrón que `interrupt_turn` ya usaba.
- **N-31 · [MEDIA] El overlay de aprobación recorta descripciones largas sin
  indicador** (`app.rs:995-1002`, 3 filas efectivas): un `shell_exec`
  multilínea se corta —incluida la línea de hint `y`/`n`— y el usuario aprueba
  una acción irreversible que no ve completa. Safety-relevante.
  **✅ RESUELTO 2026-07-06.** Nueva `truncate_for_display` (función libre,
  testeada de forma aislada): reserva siempre la última fila para el hint
  y/n, y acota la descripción a un presupuesto de caracteres derivado de
  `composer_area.height/width` — el wrap por palabras solo puede usar
  *menos* filas que esa cota nunca más, así que el hint jamás se empuja
  fuera de pantalla — con un marcador "…" visible cuando corta algo.
- **N-32 · [MEDIA] `commit_cell` trunca la altura vía `as u16`** (`app.rs:921-928`):
  exactamente 65.536 líneas wrapeadas → altura 0 → contenido dropeado en
  silencio. Alcanzable por el gateo de fences (un bloque cercado se commitea
  atómico; `finish()` vuelca una fence sin cerrar entera).
  **✅ RESUELTO 2026-07-06.** Nueva `clamp_height` (función libre, testeada):
  `line_count.clamp(1, u16::MAX as usize) as u16` — el clamp corre en
  `usize` antes del cast, así que nunca es lossy. Test confirma
  `clamp_height(65_536) == u16::MAX` (antes: `0`).

Los 8 hallazgos del Grupo L (`N-10, N-11, N-12, N-28, N-29, N-30, N-31,
N-32`) — ✅ **CERRADOS 2026-07-06**. Workspace completo verde, clippy
limpio, `cargo fmt` aplicado a `braze-tui`/`braze-cli`. **Nota de alcance:**
`braze-tui` no tenía (y sigue sin tener) ningún test a nivel de `App`
completo — solo funciones libres puras (`backtrack_preview`,
`truncate_for_display`, `clamp_height`, `mentions::list_files`, etc.) y
snapshot tests de `HistoryCell` individuales. Construir un harness de `App`
real requeriría agregar `braze-model`/`braze-tools-core` como dependencias
de desarrollo (`braze-tui` hoy solo recibe un `Arc<Engine>` ya construido
desde `braze-cli`, no los conoce) — cambio de alcance mayor al de estos
fixes puntuales, no realizado en esta sesión. Cada fix se verificó por
lectura cuidadosa del código + test unitario de su lógica pura extraíble
(y, para N-11, revirtiendo el fix para confirmar que el test lo detecta);
ninguno se verificó manejando el binario real en una terminal interactiva.

### Bench / config
- **N-33 · [ALTA en el contexto del bench] El timeout por tarea abandona pero
  no mata** (`runner.rs:112-124` + `engine.rs:804,1236-1239` + `shell_exec.rs`
  sin `kill_on_drop`): el proceso colgado sigue consumiendo CPU/RAM el resto
  del sweep → reintroduce el confounder de contención que `df36b92` eliminó.
  *Fix:* `kill_on_drop(true)` + cancelación de las tool-tasks.
- **N-34 · [MEDIA-ALTA] Sin seed y sin paridad de temperatura entre backends**
  comparados: Ollama a 0.2 fijo, Anthropic/OpenRouter a default del proveedor
  (~1.0). La tabla compara regímenes de sampling distintos y ningún run es
  reproducible. El titular `deepseek 49/50` se midió a temperatura del
  proveedor vs. Ollama a 0.2 — no es columna apples-to-apples.
- **N-35 · [MEDIA] La métrica `permission_denials` es siempre 0**: el
  `DenyAll` del bench no persiste `PermissionDecided`, y una denegación se
  cuenta como `tool_execution_failures` (el engine apendea `ToolCallStarted`
  antes del dispatch). La columna "denegaciones" del sweep está muerta y "0
  denegaciones" en los docs es vacío.
- **N-36 · [MEDIA] El bench no mide el sistema de producción**: usa un system
  prompt de una línea (no el anti-loop de `main.rs:47-66`) y no aplica
  `with_context_budget` (Ollama en producción sí). Los pass rates no
  transfieren a `braze chat`.
- **N-37 · [MEDIA] Matemática de agregación floja** (`report.rs:53-76`): el
  intervalo de Wilson trata las repeticiones de la *misma* tarea como i.i.d.
  (correlacionadas → "±5pp" demasiado angosto); las filas `HarnessError`
  entran al denominador de pass rate y a los promedios con `wall_time:0`; no
  hay mediana/percentiles (F9 abierto), y F10 sigue con matches de substring
  (`"3"` matchea "13").
- **N-38 · [MEDIA] Escape del sandbox vía archivo de suite** (`sandbox.rs:25-38`):
  un `setup_files` con clave `"../../x"` o absoluta escribe fuera del sandbox
  antes del run; `Drop` corre `remove_dir_all` sobre esa ruta.
- **N-39 · [MEDIA] Las API keys son `String` plano en `Config`/`ConfigOverrides`
  con `derive(Debug, Serialize)`** (E6 abierto, `config.rs:29-36`): cualquier
  `debug!(?config)` futuro filtra `sk-...` a logs. *Fix:* newtype con `Debug`
  redactado.

---

## 4. Hallazgos bajos (resumen)

- **Backends:** Ollama emite `arguments` como string JSON no manejado (B14);
  ids de tool call con contador global de proceso (A13, colisión tras resume);
  OpenRouter `"error":null` en un chunk mata el stream contra gateways
  LiteLLM/vLLM; B1 nunca implementó la señal de truncamiento dura
  (`prompt_eval_count >= num_ctx`).
- **Engine:** una completion vacía termina el turno como éxito silencioso
  (`engine.rs:473-483`; bajo best-of-n, 3 vacíos "ganan"); `dropped_tokens_estimate`
  cuenta como perdido el tail que se conserva; `attempt_final_summary_round`
  traga el error real y muestra texto que luego dropea.
- **Sesión:** `flush()` sin `sync_data()` (C14, productor del insumo de N-5);
  cache en memoria sin evicción; estimador de tokens sobre `Debug` repr
  (infla 30-50%); `AgentEvent::Unknown` pierde el payload al replicarse en
  backtrack.
- **N-40 · [BAJA] Forward-compat parcial (grupo G):** `#[serde(other)]` solo
  rescata tags de `"type"` desconocidos; una variante nueva de `PermissionKey`
  (o un campo no-defaultable dentro de un evento conocido) escrita por un
  binario más nuevo rompe el parse de esa línea intermedia → `load` aborta la
  sesión entera en el binario viejo, justo el modo de falla que C9 pretendía
  cerrar, un nivel más adentro (`braze-types/src/permission.rs:15-38`,
  `file_store.rs:169-172`). *Fix:* `#[serde(default)]`+`Option` o
  deserialización tolerante por-campo del key.
- **N-41 · [BAJA] Sin validación `tactical_window < tactical_compaction_threshold`**
  (`braze-config/src/config.rs:224-229`): una config con ventana ≥ umbral entra
  en compactación permanente (un `CompactionOccurred` por `load_messages`).
  *Fix:* `warn!`/clamp en startup.
- **TUI:** ANSI/tabs en tool output se ven como basura literal
  (`[0m[32m…`); slash command con argumentos (`/quit ahora`) se manda al
  modelo; `replace_trigger_token` deja residuo con el cursor a mitad de token;
  `last_esc_at` no se limpia cuando otro handler consume el Esc; truncación
  por char-count vs. ancho de display (CJK); replay de backtrack fallido deja
  archivo de sesión huérfano; heurística de fences falla en fences
  anidadas/citadas.
- **Config/CLI:** `--repetitions 0` acepta silenciosamente (sweep vacío exit
  0); `--output` se valida recién tras el sweep entero; claves de config
  desconocidas se ignoran sin warning; sin validación de rango numérico
  (`num_ctx=0`); `--resume <uuid-inexistente>` arranca sesión vacía en
  silencio; el help de `--tui` miente (dice que las acciones irreversibles se
  deniegan "hasta la oleada 4", pero el overlay ya se implementó).

---

## 5. Fortalezas confirmadas (no tocar)

- **El canal de error del stream (A3/B4)** está implementado consistentemente
  en los tres backends, con los 4 modos de falla cubiertos y tests espejo; el
  engine no persiste parciales.
- **Los fixes de seguridad nombrados (D1, E1, E2-`env`, D5 namespacing MCP)
  están genuinamente cerrados y no son bypasseables** por su vía original. El
  SSE framing hecho a mano es correcto (maneja `\r\n`, multi-línea,
  keep-alives, fragmentación arbitraria, UTF-8 partido).
- **La paridad de OpenRouter en lo crítico es sustancial**: no repite los bugs
  ya corregidos (canal de error, `max_tokens`, tool calls fragmentados, HTTP
  no-200); sus gaps son de segunda línea y específicos de la heterogeneidad de
  upstreams.
- **G10 (best-of-n)**: voto por pluralidad correcto, desempate determinista,
  usage sumado con `stop_reason` del ganador, `best_of_n:0` no paniquea — todo
  con tests (salvo el todo-o-nada de N-13).
- **El diseño de backtrack** (sesión nueva por replay en vez de mutar in-place)
  es la decisión correcta frente al contrato append-only y la cache; evita una
  familia entera de bugs de reconciliación (los huecos son N-12/N-26, no el
  diseño).
- **Temas de la TUI**: sin indexing, sin OOB, fail-fast en nombre desconocido
  al startup, structs de color completos — limpio.
- **La idempotencia de compactación (A2/C2)** vía `last_compaction_index` se
  sostiene en el caso base con test de regresión; el O(n²) de I/O se cerró con
  la cache C11 (salvo la carrera N-7).
- **El versionado forward-compat (C9)** con `#[serde(other)]` + `#[serde(default)]`
  es correcto y aditivo.

---

## 6. Roadmap de remediación priorizado

### Grupo I — Corrupción permanente de sesión (BLOQUEANTE de Fase 6, esfuerzo medio)
`N-2, N-1, N-4, N-3, N-6, N-5, N-7, N-14`. Todos producen 400 permanente o
pérdida de contexto contra Anthropic real, y todos se manifiestan en la
verificación end-to-end pendiente.

**Precondición — ✅ CERRADA 2026-07-05.** Se agregó
`crates/braze-engine/src/protocol_check.rs` (`#[cfg(test)]`, `pub(crate)`): un
validador puro (`check_anthropic_message_protocol`) que aplica las cuatro
reglas de orden que la API real de Anthropic exige — primer mensaje `user`,
ids de `tool_use` únicos, todo `tool_result` referencia un `tool_use` previo,
y ese `tool_result` debe ser el mensaje *inmediatamente siguiente* al de su
`tool_use`. `crate::protocol_check::ProtocolViolation` reporta la violación
exacta (con índice de mensaje) en vez de solo "Err".

También se agregó `ProtocolValidatingModel<M>` (decorador de `ModelBackend`,
en `engine.rs`'s `mod tests`) que envuelve cualquier backend de test
(`ScriptedModel` u otro) y valida `req.messages` en cada `complete()` antes de
delegar — convierte lo que sería un 400 en producción en un panic inmediato y
preciso justo en el sitio donde se construyó la secuencia inválida.

Con esta infraestructura se escribieron 3 tests de regresión que
reprodujeron N-1, N-2 y N-4 como fallas rojas antes de arreglarlos.

**N-1, N-1b, N-2 y N-4 — ✅ RESUELTOS 2026-07-05.** Ver el detalle del fix en
cada hallazgo arriba. Resumen de los cambios:
- `history.rs`: `build_messages` agrupa `tool_use`/`tool_result` consecutivos
  del mismo tipo en un solo `Message` multi-bloque (fix de N-1b, el hallazgo
  más profundo que surgió al perseguir N-1 — cualquier round con 2+ tool
  calls concurrentes producía mensajes no-adyacentes con o sin
  compactación); y antepone un `User` placeholder cuando `durable_events` es
  no-vacío con `summary` todavía vacío (fix de N-2).
- `engine.rs`: `pair_aware_tail_start` hace el corte de `KEEP_RAW_TAIL`
  consciente de los pares (fix de N-1, complementario al de N-1b); `run_turn`
  llama a `load_and_repair` (nuevo, factoriza el load+repair que antes vivía
  inline en `load_messages`) antes de apendear el `UserMessage` del turno en
  vez de después (fix de N-4), y su resultado siembra `known_tool_call_ids`
  (fix de N-14).
- Los 3 tests de regresión perdieron su `#[ignore]` y pasan en verde sin
  tocar sus aserciones originales; se sumó
  `history::tests::concurrent_tool_calls_in_one_round_group_into_one_message_each_role`
  como cobertura dedicada de N-1b. Workspace completo verde (0 failed, 0
  ignored), clippy limpio.

**N-3, N-5, N-6, N-7 y N-14 — ✅ RESUELTOS 2026-07-05.** Ver el detalle del
fix en cada hallazgo arriba. Ninguno era una violación de *forma* del
mensaje (el validador de protocolo no los cubre) — cada uno se verificó con
un test dirigido de contenido/estado propio:
- N-3: `SimpleContextCompactor::split` cosecha el summary de un
  `CompactionOccurred` aunque siga dentro de la ventana.
- N-6: `estimate_prompt_tokens` mide la forma renderizada/limpiada de
  `durable_events` (`history::render_durable_events`, nuevo) en vez del
  `Debug` crudo, y `load_messages` no recompacta cuando no puede ayudar
  (`tactical.len() <= KEEP_RAW_TAIL`).
- N-5: `load` repara el archivo en disco (no solo la vista en memoria) al
  descartar una línea final truncada.
- N-7: el camino frío de `load` toma el mismo `write_lock` que `append`,
  serializando la lectura contra una escritura concurrente en vuelo.
- N-14: `ensure_unique_tool_call_id` re-sufija cualquier id colisionante
  antes de persistirlo/despacharlo.

**Grupo I completo — ✅ CERRADO 2026-07-05.** Los 8 hallazgos originales
(`N-1, N-1b, N-2, N-3, N-4, N-5, N-6, N-7, N-14` — 9 contando N-1b) están
resueltos con test de regresión propio cada uno. Workspace completo verde,
clippy limpio.

**Actualización — verificación end-to-end contra Anthropic real (2026-07-05):
un hallazgo más, N-2b, encontrado y cerrado.** La suite de tests unitarios
(`ScriptedModel`, sin validación de protocolo real) no podía atrapar esto por
diseño; hizo falta el key real de Anthropic para que apareciera. Ver el
detalle en el hallazgo N-2b arriba (junto a N-2, ya que es la misma familia
de bug: un par tool_use/tool_result partido entre `durable_events` y
`tactical`, esta vez por el lado de la ventana en vez del lado del orden de
renderizado). Con N-2b cerrado, la sesión exacta que había producido el 400
real se re-ejecutó limpia. **Grupo I ahora tiene 10 hallazgos, todos
cerrados y verificados tanto por test unitario como en vivo contra
Anthropic real.**

### Grupo J — Seguridad (máxima prioridad, esfuerzo bajo) — ✅ CERRADO 2026-07-05
`N-8a, N-8b`. Cerrados los dos escapes del allowlist en
`crates/braze-permissions/src/classifier.rs`:
- `N-8a`: `-fls` agregado a `MUTATING_PRIMARIES` de `is_safe_find`;
  `is_safe_git` ahora rechaza `-o`/`--output`/`--output=FILE`/`--ext-diff` en
  `diff`/`log`/`show`.
- `N-8b`: `is_safe_shell_command` pasó a método de `DefaultClassifier` y
  gatea `cat`/`head`/`tail`/`file`/`diff`/`grep`/`find` con
  `all_path_like_args_allowed` — todo argumento no-flag debe resolver dentro
  del `WorkdirAllowlist` del proceso, si no, `Irreversible` (prompt).

6 tests de regresión nuevos (`find_fls_is_irreversible`,
`git_diff_log_show_with_output_or_ext_diff_are_irreversible`,
`shell_read_commands_outside_workdir_are_irreversible`,
`shell_read_commands_inside_workdir_are_still_reversible`, más los dos casos
`-o`/`--output` separados dentro del test de git). Suite completa del
workspace verde (`cargo test --workspace`, 0 failed) y
`cargo clippy --workspace --all-targets -- -D warnings` limpio.

### Grupo K — Robustez de backends (media, esfuerzo bajo-medio) — ✅ CERRADO 2026-07-05
`N-9, N-18, N-19, N-20, N-21, N-22`. Cerrados los 6:
- `N-9`: buffer de argumentos vacío/whitespace → `{}` en vez de dropear el
  tool call (Anthropic y OpenRouter).
- `N-18`: `handle_done_sentinel` de OpenRouter drena los tool calls
  acumulados antes de cerrar el stream.
- `N-19`: `MAX_TOOL_CALL_INDEX=128` — un índice de fragmento implausible se
  ignora en vez de forzar un `resize_with` de gigabytes.
- `N-20`: nuevo `crates/braze-model/src/http_client.rs` compartido —
  `connect_timeout=10s` + `read_timeout=600s` (se resetea por lectura, no es
  un timeout total) en los tres backends, reemplazando `reqwest::Client::new()`.
- `N-21`: OpenRouter sintetiza `openrouter-tool-call-{n}` cuando el
  proveedor nunca manda un id.
- `N-22`: `finish_reason:"error"` de OpenRouter setea `stream_error` en vez
  de tratarse como parada normal.

Los 6 con test de regresión propio (el de N-20 usa duraciones cortas
inyectables para no esperar los 600s reales de producción). Workspace
completo verde, clippy limpio, `cargo fmt` aplicado a los crates tocados
(`braze-model`, `braze-engine`, `braze-session`) — el resto del workspace
(`braze-tui`, `braze-cli`, `braze-mcp-client`) tiene drift de formato
preexistente, no tocado por estar fuera de alcance de esta sesión.

### Grupo L — TUI Fase 2 (media, esfuerzo medio) — ✅ CERRADO 2026-07-06
`N-10, N-11, N-12, N-28, N-29, N-30, N-31, N-32`. Ver el detalle de cada fix
en la sección "TUI (Fase 2)" de los Hallazgos críticos y altos, y en "TUI"
dentro de Hallazgos medios, más arriba.

### Grupo M — Validez del bench (media, esfuerzo medio) — ✅ CERRADO 2026-07-06
`N-33, N-34, N-35, N-36, N-37`. Cerrados los 5:
- `N-33`: `Command::kill_on_drop(true)` en `braze-tools-local::shell_exec::run`
  (el proceso hijo ya no sobrevive a un `abort()` de su tarea propietaria) +
  `TaskNotifier::abort` nuevo en el trait (`braze-events`), implementado por
  `ChannelTaskNotifier` (rastrea `JoinHandle` por tarea, con `Drop` que aborta
  todo lo pendiente) y por el `TestNotifier` de `braze-engine`.
  `Engine::dispatch_tool_calls` ahora llama `abort()` de verdad al vencer el
  timeout por tarea, en vez de solo "olvidarla" corriendo en background.
- `N-34`: `temperature`/`seed` agregados a los tres `ModelBackend`
  (`AnthropicBackend::with_temperature`, `OllamaBackend::with_seed`,
  `OpenRouterBackend::with_temperature`/`with_seed`; Anthropic no tiene
  parámetro de seed en su API — documentado como límite conocido). El bench
  ahora expone `--temperature`/`--seed` y aplica el mismo sampling a los tres
  backends de un sweep, con offset por repetición (`seed + repetition`) para
  no colapsar `--repetitions` en copias idénticas.
- `N-35`: `DenyAll` (el `ConfirmationPrompt` del bench) ahora persiste el par
  `PermissionRequested`/`PermissionDecided` igual que `TerminalConfirmationPrompt`
  y `ChannelConfirmationPrompt` — antes una denegación nunca entraba al log de
  sesión, así que `permission_denials` quedaba en 0 y la denegación se contaba
  como `tool_execution_failures`.
- `N-36`: `default_system_prompt`/`ollama_context_budget_tokens` se movieron a
  `braze-config` (compartidos por `braze-cli` y `braze-bench`, en vez de
  duplicados) — el bench ahora mide con el mismo system prompt anti-loop y el
  mismo budget de contexto de Ollama que usa `braze chat`/`braze run` de verdad.
- `N-37`: `report.rs::summarize` excluye las filas `HarnessError` del
  denominador de pass rate y de los promedios (antes diluían ambos con
  `wall_time_ms:0`), reportando el conteo excluido en su propia columna
  (`harness_err`) en vez de descartarlo en silencio; se agregó mediana de
  wall-time junto al promedio. Wilson tratando repeticiones como i.i.d. y
  percentiles completos (p90/p99) quedan documentados como límite conocido,
  no resueltos en este grupo (serían un cambio estadístico mayor,
  desproporcionado al resto del fix).

15 tests de regresión nuevos entre `braze-tools-local`, `braze-events`,
`braze-engine`, `braze-model` y `braze-bench` (incluye, para N-33 y N-35,
revertir el fix y confirmar que el test correspondiente falla). Workspace
completo verde (`cargo test --workspace`, 473 tests) y `cargo clippy
--workspace --all-targets -- -D warnings` limpio.

### Grupo N — Deuda menor y ergonomía (baja) — ✅ CERRADO 2026-07-06
Alcance completo: los 10 hallazgos MEDIA sueltos (`N-13, N-15, N-16, N-17,
N-23, N-24, N-25, N-26, N-27, N-38`), los 3 BAJA nombrados (`N-39, N-40,
N-41`), y los ~20 hallazgos bajos sin numerar de los bloques Engine,
Backends, Sesión, TUI y Config/CLI. Organizado en 5 paquetes por área:

- **Paquete 1 — Engine loop** (`braze-engine`): `N-13` (best-of-n vota
  entre los candidatos exitosos en vez de abortar todo el round si uno
  falla), `N-15` (`Engine::with_textual_rescue_enabled` — flag para
  desactivar el rescate textual, gateado también vía
  `Config::disable_textual_tool_call_rescue`), `N-16`
  (`ToolRegistry::all_stubs_lossy` — un provider caído degrada en vez de
  abortar todos los turnos), `N-17` (`TurnGuard`: guarda con `AtomicBool` +
  `EngineError::ConcurrentTurn` — rechaza explícitamente una segunda
  llamada concurrente a `run_turn` en vez de dejarlas robarse
  completions), `N-24` (`EngineError::TruncatedFinalResponse` — una
  respuesta final truncada por `max_tokens`/`length` se reporta como error
  en vez de persistirse como respuesta convergida). Bajos: completion
  vacía → `EngineError::EmptyModelResponse` en vez de éxito silencioso;
  `estimate_dropped_tokens` cuenta texto visible en vez de `Debug` repr (y
  ya no cuenta el tail retenido); `attempt_final_summary_round` ahora
  loguea el error real en sus dos puntos de fallo en vez de tragarlo.
- **Paquete 2 — Backends** (`braze-model`): `N-23` (Ollama: el
  `tool_use_id` se incrusta en el contenido también en el caso
  exitoso, no solo en error — corrige atribución cruzada con 2+ tool
  calls concurrentes). Bajos: `arguments` como string JSON parseado antes
  de pasar (no solo objeto nativo); `TOOL_CALL_COUNTER` de Ollama/OpenRouter
  mezclado con un nonce de proceso (`synth_id::process_nonce`) para no
  colisionar tras `--resume`; OpenRouter `"error":null` ya no mata streams
  sanos (`.filter(|e| !e.is_null())`); Ollama detecta truncamiento duro
  (`prompt_eval_count >= num_ctx`) y lo reporta como `stream_error`.
- **Paquete 3 — Sesión** (`braze-session`, `braze-tui`, `braze-events`):
  `N-25` (`MAX_SUMMARIES_KEPT = 5` — `durable.summary` ya no crece sin
  cota), `N-26` (`braze_engine::synthesize_orphan_repairs`, función libre
  reusada por `Engine::repair_orphaned_tool_calls` y por
  `App::backtrack_to` — el prefijo replicado en un backtrack ahora repara
  sus propios huérfanos), `N-27` (`FileSessionStore` adquiere un lock
  advisory por sesión vía `fs2`, `session_locks: Mutex<HashMap<SessionId,
  File>>` — un segundo proceso escribiendo la misma sesión falla alto y
  claro en vez de correr reparaciones duplicadas). Bajos: `sync_data()`
  tras `flush()`; `estimate_dropped_tokens` ya no usa `Debug` repr (mismo
  fix que Paquete 1). `AgentEvent::Unknown` perdiendo payload en
  replicación de backtrack queda documentado como límite aceptado
  (`#[serde(other)]` no soporta payload en enums con tag externo — el
  wire format real cambiaría, riesgo mayor al problema).
- **Paquete 4 — TUI** (`braze-tui`): `sanitize_tool_output` (strip ANSI +
  expand tabs) aplicado en `summarize_tool_output`/
  `ExpandedToolOutputCell`; `slash_commands::parse_slash_command` — un
  comando reconocido con argumentos (`/quit ahora`) ya no se manda al
  modelo; `composer_trigger::token_suffix_len` + `TextArea::delete_str` —
  `replace_trigger_token` ya no deja residuo con el cursor a mitad de
  token; `last_esc_at` se limpia en los 3 handlers que consumen un Esc sin
  pasar por `handle_idle_escape`; `truncate_for_display` budgetea por
  ancho de display (`unicode-width`) en vez de char-count (CJK/emoji);
  `safe_commit_boundary` reescrito con matching de largo de backticks
  (`fence_marker_len`) — fences citadas/anidadas ya no rompen el gateo.
  Orphaned session file tras un backtrack fallido queda documentado como
  límite aceptado (`SessionStore` no tiene método de borrado; agregar uno
  para este caso raro y benigno no es proporcional).
- **Paquete 5 — Config/CLI + Bench** (`braze-config`, `braze-cli`,
  `braze-bench`, `braze-types`): `N-38` (`TaskSandbox::new` rechaza
  `setup_files` con `..` o rutas absolutas), `N-39` (`braze_config::ApiKey`
  — newtype con `Debug`/`Serialize` redactados, `Deserialize` transparente),
  `N-40` (`braze_types::deserialize_permission_key_lossy` —
  `deserialize_with` por-campo en `key`, cae a `None` en vez de abortar
  toda la línea/sesión ante una variante de `PermissionKey` desconocida),
  `N-41` (`Config::validate` — rechaza `tactical_window >=
  tactical_compaction_threshold`). Bajos: `--repetitions 0` rechazado por
  `clap` (`value_parser().range(1..)`); `--output` validado antes del
  sweep, no después; claves de config/env desconocidas loguean `warn!`
  (antes silenciosas); `ollama_num_ctx`/`max_tokens` en 0 rechazados por
  `Config::validate`; `--resume <uuid-inexistente>` avisa por stderr en
  vez de arrancar sesión vacía en silencio; help de `--tui` actualizado
  (la aprobación real ya está implementada, no "hasta la oleada 4").

~45 tests de regresión nuevos a través de 8 crates (incluye, para varios
hallazgos — el guard de N-13/N-17, el lock de N-27, el kill_on_drop
heredado de Grupo M —, revertir el fix y confirmar que el test
correspondiente falla). Dos dependencias nuevas, ambas acotadas a un solo
crate: `fs2` (`braze-session`, lock advisory) y `unicode-width`
(`braze-tui`, ya transitiva vía ratatui/crossterm). Workspace completo
verde (`cargo test --workspace`, 519 tests) y `cargo clippy --workspace
--all-targets -- -D warnings` limpio.

---

## 7. Nota sobre la próxima acción

**Actualización 2026-07-05: verificación end-to-end contra Anthropic/Ollama
reales EJECUTADA (PLAN.md § "Verificación end-to-end (tras Fase 5)").**

- **Ollama** (`qwen2.5:3b`): sesión multi-turno con `tactical_window`/
  `tactical_compaction_threshold` bajos forzando compactación real (4
  compactaciones sobre 51 eventos, sin thrashing); `kill -9` a mitad de
  sesión + `--resume` reprodujo en vivo la colisión de ids de N-14
  (`ollama-tool-call-0` → renombrado a `ollama-tool-call-0-dup1`,
  correlación correcta); simulación de huérfano + resume confirmó el orden
  de reparación de N-4 exactamente como se diseñó. Sin crashes, sin
  procesos colgados.
- **Anthropic real** (`claude-haiku-4-5-20251001`, con API key provista por
  el usuario): el paso 3 del checklist, pendiente desde el MVP original por
  costo. **Encontró un bug real en el primer intento** — un 400 genuino
  (`tool_use ids were found without tool_result blocks immediately after`)
  al pedir dos tool calls concurrentes con ventana angosta — precisamente
  el tipo de violación que ningún test unitario con `ScriptedModel` podía
  atrapar. Diagnosticado, corregido (N-2b, ver arriba) y **re-verificado en
  vivo**: la misma sesión exacta corre limpia tras el fix.

**Grupo I ahora tiene 10 hallazgos** (`N-1, N-1b, N-2, N-2b, N-3, N-4, N-5,
N-6, N-7, N-14`), todos cerrados y verificados tanto por test unitario como
en vivo contra Anthropic real. El checklist completo de PLAN.md § "Verificación
end-to-end" (pasos 1-6, incluido el paso 3 que quedaba pendiente por costo)
está satisfecho.

Quedan abiertos, fuera del alcance de esta sesión: Grupo K (robustez de
backends — el más barato es N-9, argumentos vacíos dropeados en silencio),
Grupo L (TUI Fase 2 — bracketed paste, walk de @-menciones, id de sesión en
backtrack), y Grupo M (validez del bench). Y dentro de lo ya cubierto,
`N-25` (crecimiento sin cota de `durable.summary`) sigue explícitamente fuera
de alcance — mencionada como antecedente de N-6 pero no cerrada.

**Lección metodológica para futuras verificaciones:** N-2b sobrevivió
completo al Grupo I (incluida su precondición, el validador de protocolo
`protocol_check.rs`, y sus tests con `ProtocolValidatingModel`) porque todos
los tests unitarios probaban el par tool_use/tool_result *completo* a un
lado de la ventana (todo durable o todo táctico), nunca el caso borde de la
ventana cayendo *entre* ambos. Una verificación end-to-end contra el
proveedor real que sí valida estrictamente el protocolo — no solo tests
unitarios con un modelo simulado — sigue siendo necesaria antes de dar por
cerrada esta clase de hallazgo.
