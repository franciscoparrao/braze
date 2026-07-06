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
- **N-19 · [MEDIA] OpenRouter: `resize_with` con índice del proveedor sin
  cota** (`openrouter_wire.rs:341-347`): un chunk con `{"index":4e9}` fuerza
  una alocación de decenas de GB → abort. Anthropic usa `HashMap` y es inmune.
  Doble corroboración. *Fix:* cota (p.ej. 128) o `HashMap`.
- **N-20 · [MEDIA] Sin timeouts HTTP en los tres backends** (`anthropic.rs`,
  `ollama.rs`, `openrouter.rs`: `reqwest::Client::new()` sin timeout): un
  servidor que acepta la conexión y luego se cuelga bloquea el agente para
  siempre. Doble corroboración. *Fix:* `connect_timeout` + timeout de idle por
  chunk (no total).
- **N-21 · [MEDIA] OpenRouter: tool call sin `id` → `id:""` → pairing roto**
  (`openrouter_wire.rs:373`, `unwrap_or_default()`). *Fix:* sintetizar id como
  Ollama.
- **N-22 · [MEDIA] OpenRouter: `finish_reason:"error"` se trata como parada
  normal** (`openrouter_wire.rs:332-335`) → texto parcial de una generación
  fallida persistido como respuesta final. *Fix:* setear `stream_error`.
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
- **N-29 · [MEDIA] Aprobaciones stale tras `interrupt_turn` bloquean el
  composer y pueden ejecutar una tool del turno abandonado** (`app.rs:709-721`):
  las tasks de dispatch no se abortan; una puede llamar `confirm()` tras el
  drain → overlay de aprobación de un turno ya matado; `y` corre la tool.
- **N-30 · [MEDIA] Un turno que erra mid-stream pierde la cola streameada del
  transcript y la deja congelada en el preview** (`app.rs:902-912`, no llama
  `markdown.finish()`).
- **N-31 · [MEDIA] El overlay de aprobación recorta descripciones largas sin
  indicador** (`app.rs:995-1002`, 3 filas efectivas): un `shell_exec`
  multilínea se corta —incluida la línea de hint `y`/`n`— y el usuario aprueba
  una acción irreversible que no ve completa. Safety-relevante.
- **N-32 · [MEDIA] `commit_cell` trunca la altura vía `as u16`** (`app.rs:921-928`):
  exactamente 65.536 líneas wrapeadas → altura 0 → contenido dropeado en
  silencio. Alcanzable por el gateo de fences (un bloque cercado se commitea
  atómico; `finish()` vuelca una fence sin cerrar entera).

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
  llama a `repair_session` (nuevo, factoriza `load_and_repair`) antes de
  apendear el `UserMessage` del turno en vez de después (fix de N-4).
- Los 3 tests de regresión perdieron su `#[ignore]` y pasan en verde sin
  tocar sus aserciones originales; se sumó
  `history::tests::concurrent_tool_calls_in_one_round_group_into_one_message_each_role`
  como cobertura dedicada de N-1b. Workspace completo verde (0 failed, 0
  ignored), clippy limpio.

**Pendientes de Grupo I:** `N-3, N-5, N-6, N-7, N-14`. N-3 (apagón del
summary), N-6 (compactación permanente vía budget de tokens), N-5/N-7
(corrupción de archivo de sesión) y N-14 (ids de tool_use duplicados) no son
violaciones de *forma* del mensaje — el validador de protocolo no los cubre;
necesitan tests dirigidos propios (de contenido/estado, no de shape) cuando
se aborden.

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

### Grupo K — Robustez de backends (media, esfuerzo bajo-medio)
`N-9` (arg vacío → `{}`, one-liner, rompe toda tool sin args en el backend
primario), `N-18, N-19, N-20, N-21, N-22`. La mayoría son fixes localizados en
`openrouter_wire.rs`/los constructores de `Client`.

### Grupo L — TUI Fase 2 (media, esfuerzo medio)
`N-10` (bracketed paste), `N-11` (walk symlink-aware / spawn_blocking), `N-12`
(id de sesión vivo), luego `N-28..N-32`.

### Grupo M — Validez del bench (media, esfuerzo medio)
`N-33` (kill-on-timeout), `N-34` (seed + temperatura por todos los backends),
`N-36` (espejar system prompt + budget de producción), `N-35` (bucketing de
denegaciones), `N-37` (excluir `HarnessError` del denominador + medianas).
Hasta cerrarlos, el sweep no es del todo confiable como está reportado.

### Grupo N — Deuda menor y ergonomía (baja)
El resto de medios/bajos, incluido `N-39` (redactar keys), `N-40`
(forward-compat del `PermissionKey`), `N-41` (validación de config) y los
mensajes de ayuda/config desactualizados.

---

## 7. Nota sobre la próxima acción

La verificación end-to-end contra Anthropic/Ollama reales (PLAN.md §
"Verificación end-to-end (tras Fase 5)") **no debería ejecutarse como prueba de
aceptación antes de cerrar el Grupo I**: los siete bugs de corrupción de sesión
se dispararán justo en ese escenario y darán 400s que parecerán problemas de
credenciales o de la API cuando en realidad son el orden del historial. El
primer entregable recomendado es el backend de test validador del protocolo
(precondición del Grupo I), porque convierte los siete en fallas de test
reproducibles en CI en vez de sorpresas en vivo.
