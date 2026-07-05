# Investigación TUI para braze — 2026-07-05

Investigación previa al diseño de `braze-tui` (la "Fase 2: TUI" diferida en
`PLAN.md`). Cuatro informes producidos por agentes de investigación en
paralelo: las TUIs de Gemini CLI, Codex CLI y opencode, más el estado del
ecosistema ratatui. Al final, la síntesis comparativa con las implicaciones
de diseño.

---

## Síntesis comparativa

| | Gemini CLI | Codex CLI | opencode |
|---|---|---|---|
| Stack UI | Fork propio de Ink (React terminal), TypeScript | **ratatui + crossterm** (Rust, migrado desde Ink) | OpenTUI (Zig + SolidJS), reescrito desde Go/Bubbletea en v1.0 |
| Acoplamiento core↔UI | Monorepo, core emite stream de eventos | Canal de eventos in-process (`Op`/`EventMsg`) | **Cliente/servidor: HTTP + SSE + OpenAPI** |
| Historial | `<Static>` de Ink → scrollback nativo | Escape sequences propias → scrollback nativo | Framebuffer propio con scroll interno |
| Streaming | Split semántico del mensaje en `\n\n` fuera de code blocks | Commits gateados por newline + typewriter effect | Parse incremental con "frontera de estabilidad" |

### Las tres convergencias

A pesar de tres stacks totalmente distintos, los tres proyectos llegaron a
las mismas conclusiones:

1. **Scrollback nativo del terminal para el historial finalizado, viewport
   dinámico pequeño solo para lo activo.** Gemini lo hace con `<Static>` de
   Ink, Codex con scroll regions ANSI manuales, y opencode es la excepción
   parcial (framebuffer propio) pero pagó reescribiendo la TUI dos veces.
   El costo de render debe ser O(viewport), no O(historial). El texto
   committed se escribe **una sola vez** y el usuario conserva
   scroll/selección/copy nativos del terminal.

2. **El streaming no se renderiza por token — se committea por fronteras
   semánticas.** Los tres bufferean deltas y solo "sellan" contenido en
   límites seguros: Gemini corta en el último `\n\n` fuera de un code
   block, Codex committea líneas completas (y retiene tablas markdown
   hasta que cierran), opencode parsea incrementalmente distinguiendo
   bloques estables de la cola inestable. Sin esto: flickering documentado
   en los tres proyectos.

3. **La TUI es un consumidor puro de eventos del core, nunca llama al
   engine sincrónicamente.** El corte `codex-core`↔`codex-tui`,
   `packages/core`↔`packages/cli` de Gemini, y el server SSE de opencode
   son el mismo diseño. `braze` ya tiene este corte hecho: `braze-engine` +
   `braze-events::AgentEvent` es exactamente el contrato que necesita la
   TUI.

### Convergencias de segundo orden

- **Tool calls como celdas tipadas con máquina de estados** (pending →
  confirming → executing → success/error/canceled), render distinto por
  estado, output truncado en el historial con vista completa en un
  overlay/pager.
- **Aprobaciones de permisos como un evento más del stream**, mostradas de
  a una en el bottom pane, con opciones allow-once / allow-session / deny.
- **Composer con textarea multi-línea propia** (los widgets estándar
  quedaron cortos en los tres proyectos), slash commands con popup fuzzy,
  `@archivo`, historial de input, Esc para interrumpir.
- **Snapshot tests de UI** (insta en Codex, vitest en Gemini) como red de
  seguridad para todo cambio visual.

### Implicación para braze-tui

- **Stack**: `ratatui` 0.30 (feature `scrolling-regions` — crítica para
  streaming sin flicker), `crossterm` con `event-stream`, loop
  `tokio::select!` sobre {teclado, `mpsc::Receiver<AgentEvent>`, frame
  requests}. Consistente con la desviación tokio ya asumida en el
  workspace.
- **MVP**: `Viewport::Inline` + `Terminal::insert_before` de stock (Codex
  hizo su propia inserción ANSI, pero eso es la ruta de madurez, no el
  punto de partida). Trampa conocida: el resize del viewport inline es el
  punto débil de ratatui — aceptarlo como limitación del MVP.
- **Widgets**: `tui-textarea` para el composer, `tui-markdown` para
  markdown (con plan de reemplazo si el streaming lo exige — Codex terminó
  escribiendo el suyo).
- **Arquitectura**: trait tipo `HistoryCell` (celdas tipadas: user,
  assistant-markdown, tool-call, approval) que mapea 1:1 a `AgentEvent`;
  celda activa mutable en el viewport; commit newline-gated al scrollback;
  redraws coalescidos con canal de capacidad 1.
- **Lo que braze ya tiene a favor**: `AgentEvent` como contrato,
  `braze-permissions` mapea directo al flujo de aprobación como evento, y
  el patrón cliente/servidor de opencode (HTTP+SSE) queda como opción
  futura sin costo hoy — el corte in-process ya es el correcto.

---

## Anexo: ángulo académico (OpenAlex, 2026-07-05)

Búsqueda en OpenAlex de literatura HCI/SE 2022-2026 sobre interacción con
asistentes/agentes de código, complementando la investigación de industria.
Los hallazgos que mueven el diseño de `braze-tui`:

1. **El cuello de botella es verificar y dirigir, no generar.**
   *Improving Steering and Verification in AI-Assisted Data Analysis with
   Interactive Task Decomposition* (UIST 2024, doi:10.1145/3654777.3676345):
   los usuarios verifican y dirigen significativamente mejor con
   descomposición en pasos/fases **editables** que contra un baseline
   conversacional puro. *The Metacognitive Demands and Opportunities of
   Generative AI* (CHI 2024, doi:10.1145/3613904.3642902, 208 citas):
   los sistemas GenAI imponen carga metacognitiva alta (monitorear,
   evaluar, confiar); se mitiga con explicabilidad y señales de estado.
   → Implicación: la TUI necesita visibilidad del plan/progreso del turno
   (celda de plan/todo) y un "recibo de turno" (qué archivos tocó, qué
   comandos corrió) — no solo el stream de texto.

2. **Las interrupciones proactivas solo se reciben bien en fronteras de
   tarea.** *Developer Interaction Patterns with Proactive AI: A Five-Day
   Field Study* (2026, doi:10.1145/3742413.3789148, estudio de campo con
   15 devs profesionales): la receptividad a intervenciones del asistente
   depende fuertemente del momento del workflow.
   → Implicación: las notificaciones de tareas en background
   (`TaskNotifier`) no deben robar foco ni interrumpir al usuario
   escribiendo — superficie de toast/status no modal, que se materializa
   en fronteras seguras (turno terminado, composer vacío).

3. **Los estados de interacción son medibles y revelan costos ocultos.**
   *Reading Between the Lines: Modeling User Behavior and Costs in
   AI-Assisted Programming* (CHI 2024, doi:10.1145/3613904.3641936, 73
   citas — taxonomía CUPS sobre Copilot): gran parte del tiempo del
   programador se va en estados de verificación no instrumentados.
   → Implicación: instrumentar la TUI misma con `tracing` (tiempo hasta
   primer token, tiempo con approval overlay abierto, turnos
   interrumpidos con Esc) — extensión natural del Grupo G de
   observabilidad.

4. **El humano como orquestador, no como espectador.** *Agentic Software
   Engineering: Foundational Pillars and a Research Roadmap* (2025,
   arXiv:2509.06216): propone el "Agent Command Environment" — el humano
   comanda/mentorea agentes y recibe paquetes de decisión
   (merge-readiness, consultation requests) en vez de streams crudos.
   Dirección a futuro coherente con multi-agente (`braze-agent-graph`
   diferido).

5. **Confianza calibrada requiere mostrar el porqué.** Convergencia de la
   literatura de transparencia (p.ej. *AI Transparency in the Age of
   LLMs*, 2023) con lo que braze ya tiene: `PermissionGuard` clasifica
   por nivel de riesgo — el overlay de aprobación debe **mostrar** ese
   nivel y la razón de la clasificación, no solo el comando pelado.

Nota honesta: no se encontró evidencia académica sólida sobre el efecto
typewriter/streaming en comprensión lectora de respuestas de chatbot (las
búsquedas devolvieron ruido) — la convergencia ahí es puramente de
industria.

---

# Informe 1: Arquitectura de la TUI de OpenAI Codex CLI (codex-rs)

Contexto verificado: el repo `openai/codex` hoy es ~96% Rust; toda la TUI
vive en el crate `codex-rs/tui` (paquete `codex-tui`). La versión
TypeScript/Ink está retirada.

## 1. Stack de UI

Confirmado: **ratatui + crossterm**, con features no triviales (de
`codex-rs/tui/Cargo.toml`):

- `ratatui` con features `scrolling-regions`, `unstable-backend-writer`,
  `unstable-rendered-line-info`, `unstable-widget-ref` — las dos primeras
  son la clave de su modelo de scrollback (ver §3).
- `crossterm` con `bracketed-paste` y `event-stream` (eventos async vía
  `EventStream`).
- `ratatui-macros`, `image` (pegar/adjuntar imágenes), `arboard`
  (clipboard), `pulldown-cmark` + `syntect` + `two-face` (markdown y
  syntax highlighting propios, no un widget de terceros), `textwrap`,
  `unicode-segmentation`, `tokio` (rt-multi-thread), `tracing`,
  `color-eyre`, `insta` para snapshot tests (todo cambio de UI exige
  snapshots).

Estructura del crate (`codex-rs/tui/src/`): `app.rs`, `app_event.rs`,
`app_event_sender.rs`, `tui.rs` + `tui/` (event_stream, frame_requester,
frame_rate_limiter, job_control), `chatwidget.rs` + `chatwidget/`,
`custom_terminal.rs`, `insert_history.rs`, `history_cell/` (base, exec,
mcp, messages, patches, approvals, plans...), `exec_cell/`, `bottom_pane/`
(chat_composer, textarea, approval_overlay, command_popup,
file_search_popup, footer...), `streaming/` (controller, chunking,
commit_tick, table_holdback), `markdown_stream.rs`, `markdown_render/`,
`diff_render.rs`, `pager_overlay.rs`, `status_indicator_widget.rs`,
`slash_command.rs`, `keymap.rs`.

## 2. Arquitectura: app loop, estado vs vista

**Loop de eventos** (`app.rs`): un `tokio::select!` multiplexa cuatro
fuentes:
1. `app_event_rx.recv()` — canal unbounded interno de `AppEvent` (bus de
   mensajes de la app).
2. `active_thread_rx.recv()` — eventos del agente del thread activo (el
   core emite `EventMsg` por canal; la TUI es consumidor puro de eventos,
   nunca llama al engine sincrónicamente).
3. `tui_events.next()` — stream de `TuiEvent` (Key, Paste, Resize,
   **Draw**) que envuelve el `EventStream` de crossterm.
4. Eventos del app-server (JSON-RPC) cuando aplica.

**Separación estado/vista**: `App` (estado global: threads, config,
overlays, navegación) contiene un `ChatWidget` (`chatwidget.rs`), que es la
máquina de estados de la sesión: `transcript: TranscriptState` (celdas
commiteadas + `active_cell` en vuelo), `running_commands:
HashMap<String, RunningCommand>`, `turn_lifecycle`, `stream_controller`,
`bottom_pane: BottomPane`, `input_queue`. El keypress se traduce a
intención (`Op` hacia el core, o `AppEvent` hacia la app) — nunca muta la
vista directamente.

**Redraw bajo demanda, no frame-loop fijo**: no redibujan a N fps; existe
un `FrameRequester` (`tui/frame_requester.rs` + `frame_rate_limiter.rs`) y
cualquier componente agenda repaint con
`tui.frame_requester().schedule_frame()`, que inyecta un `TuiEvent::Draw`
al stream. El render es immediate-mode: cada frame redibuja los widgets
visibles desde cero.

**Widgets principales**: `ChatWidget` (pantalla), celdas de historial
(`history_cell/`: trait base + celdas por tipo — mensajes, exec, MCP,
patches, aprobaciones, planes, sesión, separadores), `BottomPane`
(composer + popups + overlays de aprobación + footer/status line),
`StatusIndicatorWidget` (spinner de tarea corriendo), `PagerOverlay`
(transcript completo con Ctrl+T, diffs).

## 3. Streaming y scrollback (el punto clave)

Hallazgo central: **Codex NO es una TUI full-screen. Usa el scrollback
nativo del terminal para el historial finalizado, y un viewport inline de
ratatui solo para el área activa** (celda en vuelo + bottom pane).

Mecánica:

- **`custom_terminal.rs`**: un fork/reimplementación del `Terminal` de
  ratatui para viewport **inline** (no alternate screen). Trackea
  `last_known_cursor_pos` y `visible_history_rows`, tiene
  `note_history_rows_inserted()`,
  `clear_scrollback_and_visible_screen_ansi()`, un `display_width()`
  propio que ignora payloads OSC (hyperlinks), y un `diff_buffers()`
  optimizado con `ClearToEnd`.
- **`insert_history.rs`**: comentario de apertura textual: *"Codex uses
  the terminal scrollback itself for finalized chat history, so inserting
  a history cell is an escape-sequence operation rather than a normal
  ratatui render"*. No usan el `insert_before` de alto nivel de ratatui:
  emiten secuencias ANSI directamente — `SetScrollRegion(1..area.top())`
  para confinar el scroll a la zona sobre el viewport, posicionan el
  cursor, escriben las líneas (con `write_spans()` que reconstruye
  colores/modificadores ANSI mínimos), y restauran. Es el mismo mecanismo
  que habilita la feature `scrolling-regions` de ratatui, pero
  implementado a mano para control fino. Hay un modo alternativo
  **ZellijRaw** (Zellij no respeta scroll regions con líneas soft-wrapped:
  escriben directo y rellenan filas en blanco).
- **Batching**: `Tui` acumula `Vec<PendingHistoryLines>` y
  `flush_pending_history_lines()` las escribe sobre el viewport dentro de
  `stdout().sync_update()` (synchronized update, evita flicker).
- **Streaming de tokens** (`markdown_stream.rs` + `streaming/`):
  `MarkdownStreamCollector` bufferea deltas y expone commits **gateados
  por newline** (`commit_complete_source()` con `rfind('\n')`, solo
  devuelve lo nuevo vía `committed_source_len`). El
  `streaming/controller.rs` + `commit_tick.rs` animan el commit gradual
  (efecto typewriter por líneas); `table_holdback.rs` retiene tablas
  markdown hasta que estén completas (una tabla parcial se re-renderiza
  mal). Lo en vuelo se dibuja en la `active_cell` del viewport; al
  consolidarse (`AppEvent::ConsolidateAgentMessage` reemplaza la corrida
  de `AgentMessageCell` streaming por una sola `AgentMarkdownCell`), se
  emite `AppEvent::InsertHistoryCell` → escritura al scrollback.
- **Resize**: `draw_with_resize_reflow()` reconstruye el scrollback
  re-flujando el transcript (por eso guardan las celdas como modelo, no
  como texto ya wrappeado); `resize_reflow_cap.rs` limita el costo.
- Ventaja explícita del diseño: scroll, selección y copy/paste nativos del
  terminal; sin manejar scrollback propio. Costo: mucha complejidad de
  compatibilidad (Zellij, OSC, wrapping de URLs — `adaptive_wrap_line()`
  no corta URLs para que sigan siendo clickeables).

## 4. Tool calls / exec / aprobaciones / diffs

- **Celdas de historial por tipo** (`history_cell/exec.rs`, `mcp.rs`,
  `patches.rs`, `approvals.rs`): un exec cell muestra el comando, output
  en vivo mientras corre (la celda activa se actualiza vía
  `sync_active_stream_tail()`; `new_active_exec_command()` la crea,
  `finalize_active_cell_as_failed()` marca fallo), y al terminar se
  commitea truncada al scrollback. Truncación agresiva: snippets de
  comando a 80 grafemas, sufijo `[...]`, procesos background agrupados
  bajo "Background terminals" con máx. 16 visibles y `↳` para chunks de
  output. El output completo queda accesible en el overlay de transcript
  (Ctrl+T) — ese es su "colapsable": scrollback muestra resumen,
  overlay/pager muestra todo.
- **Aprobaciones** (`bottom_pane/approval_overlay.rs`): enum
  `ApprovalRequest` con 4 tipos — exec command, patch (edición de
  archivos), permissions (fs/red, con scope de turno o sesión), y MCP
  elicitation. Se renderizan como `ListSelectionView` en el bottom pane
  con header de contexto (`build_header()`: comando/razón/reglas).
  Atajos: `y` aprobar, `d` denegar, `a` aprobar por sesión, `Esc` cancelar
  (contrato duro: en MCP elicitation Esc es siempre cancel aunque el
  usuario customice keymap), `Ctrl+Shift+A` vista fullscreen. La decisión
  vuelve por `app_event_tx` como `AppEvent` → `Op` al core. Nota
  arquitectónica: el protocolo core→TUI es asíncrono submit/event (Ops:
  UserTurn/Interrupt; Events: `AgentMessageDelta`, `ExecCommandBegin/End`,
  `PatchApplied`, `TokensUsed`), y la aprobación es un evento más que se
  materializa en el bottom pane.
- **Diffs**: `diff_model.rs` + `diff_render.rs` (render unificado con
  colores) para patches propuestos y el comando `/diff`; los diffs largos
  se abren en el `pager_overlay.rs`.

## 5. Manejo de input

Todo en `bottom_pane/` — `chat_composer.rs` sobre un `textarea.rs` propio
(no tui-textarea):

- **Multi-línea**: Enter envía; Shift+Enter o Ctrl+J insertan newline
  (hint dinámico en el footer). Tab encola el mensaje si hay tarea
  corriendo (input queue — puedes escribir el siguiente prompt mientras el
  agente trabaja).
- **Historial** (`chat_composer_history.rs`): Up/Down estilo shell,
  fusionando historial persistente cross-sesión (solo texto) con el local
  de la sesión (con adjuntos); Ctrl+R búsqueda incremental reversa con
  preview en el body.
- **Slash commands** (`slash_command.rs`, `bottom_pane/command_popup.rs`):
  al tipear `/` aparece popup filtrado; Tab/Enter acepta →
  `InputResult::Command(SlashCommand)`.
- **@-menciones** (`file_search_popup.rs` + `file_search.rs`): `@` dispara
  búsqueda de archivos asíncrona (`AppEvent::StartFileSearch` /
  `FileSearchResult`); imágenes se adjuntan, rutas se insertan como texto.
- **Paste**: bracketed paste vía crossterm; pastes >1000 chars
  (`LARGE_PASTE_CHAR_THRESHOLD`) se reemplazan por placeholder; imágenes
  pegadas se adjuntan. En Windows (sin bracketed paste confiable),
  `paste_burst.rs` detecta ráfagas de chars y las bufferea.
- **Interrupciones**: Esc interrumpe el turno corriendo (limpia colas de
  stream, puede restaurar el prompt si no hubo output —
  `RestorePromptIfNoOutput`); Esc-Esc entra al modo **backtrack**
  (`app_backtrack.rs`: retroceder a un mensaje anterior y editar). Ctrl+C
  limpia el draft preservándolo en historial (`clear_for_ctrl_c()`);
  doble Ctrl+C/Ctrl+D dentro de timeout sale. Ctrl+Z suspende con job
  control real (`tui/job_control.rs` reposiciona el cursor para SIGCONT).
  Ctrl+T abre el overlay de transcript; Ctrl+G lanza editor externo
  (`external_editor.rs`, con `Tui::with_restored()` que pausa el
  EventStream y restaura modos del terminal). Keymap customizable
  (`keymap.rs`) y modo vim parcial en el composer.

## 6. Por qué migraron de Ink/TypeScript a Rust

Anunciado por Fouad Matin (co-lead de Codex) el 30-may-2025 en la
Discussion #1174 "Codex CLI is Going Native". Razones textuales:

1. **Instalación zero-dependency**: "currently Node v22+ is required,
   which is frustrating or a blocker for some users" (enterprise,
   air-gapped).
2. **Bindings de seguridad nativos**: el sandboxing (macOS Seatbelt via
   `sandbox-exec`, Linux Landlock/seccomp) ya lo tenían en Rust; en Node
   requería shims FFI.
3. **Sin GC**: "no runtime garbage collection, resulting in lower memory
   consumption" — las pausas de GC eran incompatibles con un proceso
   agéntico long-running que acumula historial y streamea.
4. **Protocolo extensible**: un wire protocol (submit/event) para extender
   el agente desde TS, Python, etc., y servir de superficie estable para
   IDEs (`codex-app-server`).

Lecciones documentadas relevantes: (a) la elección no fue "amor por Rust"
sino "best tool for the job" — reconocieron que TS itera más rápido; (b)
ratatui empuja hacia full-screen TUIs, y ellos decidieron pelear contra
eso: gestionar scrollback nativo les costó `custom_terminal.rs` +
`insert_history.rs` + casos especiales (Zellij, resize-reflow, copy/paste
— issue #1247 fue exactamente sobre copy/paste degradado en la TUI Rust
vs Ink), pero les dio UX de terminal "real"; (c) todo cambio visual se
protege con snapshot tests `insta`; (d) el desacoplamiento total core↔TUI
vía canal de eventos permitió que TUI, exec headless e IDE bridge sean
consumidores intercambiables del mismo engine (`codex-core`).

Fuentes:
- [Codex CLI is Going Native — openai/codex Discussion #1174](https://github.com/openai/codex/discussions/1174)
- [The codex-rs Architecture: How OpenAI Rewrote Codex CLI in Rust](https://codex.danielvaughan.com/2026/03/28/codex-rs-rust-rewrite-architecture/)
- [openai/codex — codex-rs/tui](https://github.com/openai/codex/tree/main/codex-rs/tui)
- [InfoQ — Another Rust Rewrite: OpenAI's Codex CLI Goes Native](https://www.infoq.com/news/2025/06/codex-cli-rust-native-rewrite/)
- [devclass — OpenAI rewrites AI coding tool in Rust](https://www.devclass.com/ai-ml/2025/06/02/nodejs-frustrating-and-inefficient-openai-rewrites-ai-coding-tool-in-rust/1619589)
- [Issue #1247 — copy/paste in the Rust TUI](https://github.com/openai/codex/issues/1247)

---

# Informe 2: Arquitectura de la TUI de Gemini CLI

Investigado directamente contra el repo (branch `main`, versión
`0.51.0-nightly.20260625`), más issues y docs. Todas las rutas son reales,
verificadas vía la API de GitHub.

## 1. Stack de UI

- **Ink confirmado, pero NO el Ink upstream**: usan un **fork propio**
  publicado como `"ink": "npm:@jrichman/ink@6.6.9"` (fork mantenido por
  Jacob Richman, ingeniero de Google). El fork agrega capacidades que Ink
  vanilla no tiene: `ResizeObserver`, `useIsScreenReaderEnabled`,
  incremental rendering, y soporte de render a "backbuffer"/alternate
  screen (ver §3).
- **React 19.2.4**, **TypeScript 5.8.3**, **Node >= 20**, ESM.
- Libs auxiliares: `ink-spinner`, `ink-gradient` + `tinygradient`, `chalk`
  4, `cli-spinners`, `string-width`, `ansi-escapes`,
  `highlight.js`/`lowlight` (syntax highlight), `diff` 8, `fzf` 0.5
  (autocompletado fuzzy), `clipboardy`, `@xterm/headless` (tests de shell
  embebido), `yargs`.
- Monorepo npm workspaces: `packages/cli` (toda la TUI) sobre
  `packages/core` (orquestación del modelo, tools, scheduler — sin UI).
  Tests de UI con `vitest` + snapshots.

## 2. Arquitectura de componentes

Todo vive en `packages/cli/src/ui/`: `components/` (~100 componentes),
`components/messages/`, `components/shared/`, `components/views/`,
`layouts/`, `hooks/` (~80 hooks), `contexts/`, `state/`, `commands/`,
`key/`, `themes/`, `editors/`, `auth/`.

**Jerarquía raíz** (entry: `packages/cli/src/gemini.tsx` →
`startInteractiveUI`):

- `ui/AppContainer.tsx` — bootstrap: monta la pirámide de providers
  (`ConfigContext`, `SettingsContext`, `UIStateContext`,
  `UIActionsContext`, `InputContext`, `KeypressContext`, `VimModeContext`,
  `StreamingContext`, `ToolActionsContext`, `OverflowContext`,
  `ShellFocusContext`…). El estado global viaja por `UIStateContext`, no
  por prop drilling.
- `ui/App.tsx` — trivial: elige entre `QuittingDisplay` y el layout; hace
  switch por screen reader.
- `ui/layouts/DefaultAppLayout.tsx` (y `ScreenReaderAppLayout.tsx` —
  layout alternativo completo para accesibilidad). El layout default:
  1. `MainContent.tsx` — historial de chat (ver §3),
  2. `BackgroundTaskDisplay.tsx` — panel de shells en background,
  3. bloque de controles inferiores: `Notifications` → `DialogManager`
     **o** `Composer` (mutuamente excluyentes) → `ExitWarning`.
- `Composer.tsx` — zona de input compuesta: `QueuedMessageDisplay`
  (mensajes encolados con Tab), `ToastDisplay`, `StatusRow`,
  `DetailedMessagesDisplay` (consola de errores, F12), `ShortcutsHelp`,
  `TodoTray`, `InputPrompt`, `Footer` (status bar: branch git, modelo,
  quota, memoria, sandbox).
- **Mensajes** (`components/messages/`): `UserMessage`,
  `GeminiMessage`/`GeminiMessageContent`, `ThinkingMessage`,
  `ToolGroupMessage` + `ToolMessage` + `DenseToolMessage`,
  `ToolConfirmationMessage`, `ToolResultDisplay`, `DiffRenderer`,
  `ErrorMessage`/`WarningMessage`/`InfoMessage`, `CompressionMessage`,
  `SubagentProgressDisplay`/`SubagentHistoryMessage`, `Todo`. Dispatcher:
  `components/HistoryItemDisplay.tsx`, switch sobre `item.type`.
- **Primitivas propias** (`components/shared/`): `VirtualizedList` y
  `ScrollableList`, `MaxSizedBox`/`SlicingMaxSizedBox` (recorte por altura
  con "+N more lines"), `RadioButtonSelect`, `TextInput`,
  `text-buffer.ts` (motor de edición, 4.285 líneas). Ink no trae nada de
  esto.
- Spinners: `GeminiRespondingSpinner`, `CliSpinner`, `usePhraseCycler`
  (frases "Thinking…" rotativas).

**Modelo de datos central**: historial = array de `HistoryItem` inmutables
(`hooks/useHistoryManager.ts`), más `pendingHistoryItems` (lo que aún está
streameando/ejecutándose). Esta separación committed vs pending es LA
decisión arquitectónica clave.

## 3. Streaming de tokens y performance de render

Sí es re-render de React por chunk, pero con tres mecanismos:

**a) `<Static>` de Ink + split de mensajes** (el truco principal). En
`MainContent.tsx` el historial committed se renderiza dentro de `<Static>`
(se escribe al scrollback una sola vez); solo `pendingItems` se
re-renderiza. En `hooks/useGeminiStream.ts` (`handleContentEvent`) el
mensaje en streaming se **parte activamente** — comentario literal:

> "Splitting a message is primarily a performance consideration…
> Everything but the last message is treated as static in order to
> prevent re-rendering an entire message history multiple times per-second
> (as streaming occurs). **Prior to this change you'd see heavy flickering
> of the terminal.**"

El punto de corte lo decide `findLastSafeSplitPoint`
(`ui/utils/markdownUtilities.ts`): busca el último `\n\n` que **no esté
dentro de un code block**; lo anterior se committea como item histórico
(→ `<Static>`), la cola queda como `pendingHistoryItem` dinámico.

**b) Tres modos de render** (config `useAlternateBuffer` /
`useTerminalBuffer`): *Legacy* (`<Static>` + región dinámica al fondo del
scrollback nativo); *Alternate buffer* (tipo vim/htop, con
`ScrollableList`/`VirtualizedList` propias, mouse scroll); *Terminal
buffer* (híbrido — lista virtualizada con `overflowToBackbuffer` que
desborda ítems al scrollback nativo).

**c) Defensas adicionales**: memoización agresiva
(`MemoizedHistoryItemDisplay`); `MAX_GEMINI_MESSAGE_LINES` capea el alto
de un mensaje; `useFlickerDetector.ts` (telemetría de flicker);
`historyRemountKey` fuerza remount del `<Static>` tras resize/clear.
Framerate de Ink capeado a 30fps (issue #8050). El fork de Ink añade
*incremental rendering* (issue #14415).

## 4. Tool calls en curso

Pipeline: `packages/core` (scheduler) emite estados →
`hooks/useToolScheduler.ts` + `toolMapping.ts` → grupo de llamadas
contiguas como un solo `HistoryItem` tipo `tool_group` →
`ToolGroupMessage` renderiza el borde compartido, adentro un `ToolMessage`
por call.

- **Estados** (`ToolCallStatus`): `Pending`, `Confirming`, `Executing`,
  `Success`, `Error`, `Canceled`. `ToolStatusIndicator` renderiza glifo
  coloreado por estado; `Executing` muestra spinner; `Canceled` en
  strikethrough; `Executing` con progreso muestra output parcial en vivo
  (shell: `AnsiOutput.tsx` con PTY embebido vía `@xterm/headless`, focus
  conmutable Tab/Shift+Tab).
- **Confirmación de permisos**: `ToolConfirmationMessage.tsx` — según
  `confirmationDetails.type` (`edit` | `exec` | `info` | `mcp` | plan):
  para edits un **diff previo** (`DiffRenderer`), para exec el comando
  (con detección de URLs engañosas), y `RadioButtonSelect` (allow once /
  allow always / modify with editor / no). **Cola de confirmaciones**
  (`ToolConfirmationQueue.tsx`): de a una, al final del área pending, con
  auto-scroll. Modos: `ApprovalMode` (default/auto-edit/yolo), ciclable
  con Shift+Tab.
- **Resultado colapsable**: `ToolResultDisplay` + `MaxSizedBox` truncan
  con "show more" (Ctrl+O); ítems anteriores al último prompt dejan de ser
  expandibles. Modo denso (`DenseToolMessage.tsx`).
- **Diffs**: `DiffRenderer.tsx` — unified diff coloreado, números de
  línea, colapsa hunks largos, archivos nuevos con syntax highlighting.

## 5. Manejo de input

- **Editor multi-línea propio**: `components/shared/text-buffer.ts` (4.285
  líneas). Buffer lógico con líneas + cursor en code points, operaciones
  word-wise, undo/redo, viewport propio, Unicode correcto, LRU cache de
  anchos, **placeholders de paste** (>5 líneas o >500 chars →
  `[Pasted Text: N lines]`), pegado de imágenes, acciones vim
  (`vim-buffer-actions.ts`, opt-in).
- **Submit vs newline**: keybindings declarativos en `ui/key/keyBindings.ts`
  (enum `Command` + `KeyBinding`, ~100 comandos, personalizables). `SUBMIT`
  = `enter`; `NEWLINE` = `ctrl+enter`/`cmd+enter`/`alt+enter`/
  `shift+enter`/`ctrl+j`. Shift+Enter funciona vía **Kitty Keyboard
  Protocol** (`useKittyKeyboardProtocol.ts`). También `\` final como
  continuación.
- **Historial de input**: `useInputHistory` (flechas/ctrl+p/n), historial
  de shell separado, **reverse search** ctrl+r.
- **Slash commands + autocompletado**: comandos declarados en
  `ui/commands/` (builtin + TOML + MCP prompts); matching **fuzzy con
  `AsyncFzf`** en `SuggestionsDisplay.tsx`. Completions: `@archivo`,
  argumentos, shell.
- **Otros atajos**: Tab = encolar mensaje mientras streamea, Ctrl+G editor
  externo, Ctrl+L clear, Ctrl+Y yolo, Shift+Tab cicla approval mode, F12
  errores, Alt+M markdown crudo, F9 copy mode, doble-Esc rewind.

## 6. Decisiones de diseño documentadas

- **Fork de Ink como decisión asumida**: issues #14415 (incremental
  rendering), #21924 (flicker-free resize), #10677 (alternate buffer),
  #8050 (cap de 30fps).
- **`<Static>` + split semántico de mensajes** como la solución de
  performance del streaming (comentario largo en `useGeminiStream.ts`).
- **Accesibilidad como layout paralelo**, no como parche.
- **Testing de TUI**: snapshots vitest por componente,
  `useFlickerDetector`, y un subagente `tui-tester` que automatiza el
  binario real vía [pproenca/agent-tui](https://github.com/pproenca/agent-tui)
  (daemon PTY en Rust) — "observe→act→wait→verify". Issue #9176.
- **UI-state en contexts, lógica en `packages/core`** — el mismo corte que
  `braze-engine` vs `braze-cli`.

**Lecciones para una TUI Rust**: (1) separar historial committed (emitir
al scrollback una vez) de la ventana pending re-pintable, con cortes en
límites de párrafo/code-block; (2) tool calls = máquina de 6 estados con
render por estado y cola de confirmaciones de a una; (3) el text buffer
multi-línea es el componente individual más caro (~4K líneas — en Rust,
`tui-textarea` cubre parte); (4) Kitty protocol para Shift+Enter
(crossterm: `PushKeyboardEnhancementFlags`); (5) keybindings como tabla
declarativa desde el día uno.

Fuentes: [gemini-cli repo](https://github.com/google-gemini/gemini-cli),
[packages/cli/src/ui](https://github.com/google-gemini/gemini-cli/tree/main/packages/cli/src/ui),
issues [#14415](https://github.com/google-gemini/gemini-cli/issues/14415),
[#21924](https://github.com/google-gemini/gemini-cli/issues/21924),
[#8050](https://github.com/google-gemini/gemini-cli/issues/8050),
[#10677](https://github.com/google-gemini/gemini-cli/issues/10677),
[#9176](https://github.com/google-gemini/gemini-cli/issues/9176),
[DeepWiki architecture](https://deepwiki.com/google-gemini/gemini-cli/1.1-architecture-overview)

---

# Informe 3: Arquitectura de la TUI de opencode (estado 2025–2026)

**Aviso previo**: (a) El repo `sst/opencode` hoy redirige a
**`anomalyco/opencode`** — el equipo SST se rebautizó "Anomaly". (b) Con
**v1.0.0 (31-oct-2025)** la TUI en Go/Bubbletea fue **reescrita completa**
sobre un framework propio, **OpenTUI** (core en Zig + bindings TypeScript
+ SolidJS). No confundir con `opencode-ai/opencode`, el proyecto original
de Kujtim Hoxha (100% Go/Bubbletea), que tras el split de 2025 se
convirtió en `charmbracelet/crush`.

## 1. Stack de UI: estado actual

| Era | Stack | Evidencia |
|---|---|---|
| Original (opencode-ai) | 100% Go: Bubbletea + Lipgloss, monolito | Hoy es `charmbracelet/crush` |
| sst/opencode ≤ v0.x (2025) | Core TypeScript/Bun + TUI en **Go/Bubbletea v2** | `packages/tui/go.mod` en tag `v0.6.4`: `bubbletea v2.0.0-beta.4`, `glamour v0.10.0`, `chroma/v2`, fork vendoreado de `charmbracelet/x/input` |
| **v1.0.0+ (oct 2025 → hoy, v1.17.x)** | **Todo TypeScript/Bun. TUI = OpenTUI (Zig + @opentui/solid + SolidJS)** | Release notes v1.0.0: reescritura completa desde Go/Bubbletea "which had performance and capability issues" |

**OpenTUI** ([anomalyco/opentui](https://github.com/anomalyco/opentui)):
"native terminal UI core written in Zig with TypeScript bindings… exposes
a C ABI". Layout flexbox, arquitectura de "Renderables", framebuffer
nativo. Anomaly lo patrocinó para evitar tanto Bubbletea como Ink (Ink:
cap de 30 FPS y >50MB de memoria para apps básicas).

## 2. Arquitectura cliente/servidor

**La** decisión arquitectónica central; sobrevivió intacta a la
reescritura de la TUI.

- **Servidor**: proceso headless en Bun, HTTP server con Hono (migrando a
  Effect en V2). `opencode serve` lo expone standalone (puerto 4096). Ahí
  vive TODO lo agéntico: loop LLM, ejecución de tools, permisos, sesiones.
- **Protocolo**: **REST/OpenAPI 3.1 + Server-Sent Events**. El cliente
  abre `EventSource('/event')` y recibe el bus de eventos completo:
  text-deltas, tool calls, cambios de estado, pedidos de permiso.
- **SDKs generados**: el spec OpenAPI alimenta generación automática de
  clientes con **Stainless**.
- **Lanzamiento**: `opencode` arranca el server Bun y spawnea el frontend
  con `OPENCODE_SERVER=<url>` por env var.
- **Ventaja declarada**: "run on your computer, drive it remotely from a
  mobile app". En la práctica: TUI + web UI + desktop + VS Code contra el
  mismo server.

**Lección**: el server es dueño de la verdad (mensajes/parts persistidos a
disco); los clientes son proyecciones. Cada mutación se persiste y se
broadcastea por SSE — la TUI nunca mantiene estado agéntico propio, solo
estado de UI.

## 3. Arquitectura de la TUI misma

### Era Go/Bubbletea (la más relevante para ratatui)

Modelo **Elm puro** (Model/Update/View): un `Model` raíz cuyo
`Update(msg)` despacha mensajes (teclas, eventos SSE convertidos a
`tea.Msg`, ticks) y un `View()` que re-renderiza. Los eventos SSE se leen
en una goroutine y se inyectan como mensajes — el mismo patrón que
necesita ratatui con canal mpsc + crossterm events.

Estructura de `packages/tui/internal/` (tag `v0.6.4`): `app/` (estado),
`api/` (comunicación con el server), `tui/` (Model raíz), `components/` —
`chat/` (viewport de mensajes, editor), `dialog/` y `modal/` (selector de
modelos, temas, sesiones, permisos), `diff/`, `status/`, `textarea/`,
`list/`, `completions/`, `toast/`, `commands/`, `qr/` — `theme/` (JSON
cargables), `layout/`, `styles/`, `viewport/`, `clipboard/`,
`attachment/`. Fork local de `charmbracelet/x/input` (el input handling
estándar les quedó corto).

### Era OpenTUI (actual)

- Pipeline: componentes SolidJS → reconciler `@opentui/solid` → árbol de
  "Renderables" en `@opentui/core` → core Zig (layout flexbox,
  framebuffer, diff de frames, ANSI). Reactividad fina de SolidJS
  (signals, sin virtual DOM).
- Componentes de fábrica: `Box`, `Text`, `Code`, `Markdown`, `Input`,
  `Textarea`, `Select`, `TabSelect`, `ScrollBox`, `Diff`, `FrameBuffer`,
  QR, notificaciones.
- UI 1.0: historial "comprimido" (solo detalle de tools edit/bash),
  command bar (Ctrl+P), sidebar opcional, sistema de **leader key**
  (`ctrl+x` + tecla). Theming: `/themes`, config en `tui.json`.

## 4. Streaming de tokens y markdown

- **Era Go**: server emite `text-delta` por SSE; la TUI re-renderiza el
  mensaje en curso con **glamour** + **chroma/v2**. Costo: re-render de
  markdown completo por delta — una de las "performance issues" que mató
  esta TUI.
- **Era OpenTUI**: el componente `Markdown` tiene **modo streaming
  nativo**: `parseMarkdownIncremental` reporta cuántos tokens del head del
  stream son "estables" (no cambiarán al apendear más), y solo se
  re-parsea/re-renderiza la cola inestable. Syntax highlighting vía
  Tree-sitter.

**Para braze/ratatui**: el patrón "parse incremental con frontera de
estabilidad" es la idea más copiable. Equivalentes Rust:
`pulldown-cmark`/`comrak` + `syntect` o `tree-sitter`, con buffer que
distinga bloques sellados vs bloque en curso.

## 5. Tool calls: permisos, output, diffs

- **Permisos**: modelo server-side. El tool consulta la config y si
  corresponde ejecuta `Permission.ask({type, pattern, callID})` → evento
  SSE → la TUI muestra el diálogo → respuesta por HTTP. Un rechazo produce
  `tool-error` y detiene el loop. El prompt del sistema instruye al modelo
  a *no* pedir permiso conversacionalmente — el permission layer es
  infraestructura.
- **Estados**: eventos `tool-call`/`tool-result`/`tool-error` como "parts"
  del mensaje; pending/running/completed por part. En 1.0 el historial
  comprime: solo edit y bash muestran detalle expandido por defecto.
- **Diffs**: era Go = componente propio + `sergi/go-diff`. Actual = `Diff`
  renderable de OpenTUI: unified o split (con `syncScroll`), Tree-sitter,
  line numbers, wrap modes. Estilo configurable (`auto` | `stacked`).
- **Seguridad complementaria**: snapshots Git por paso (`git write-tree`
  sin tocar el índice) para revertir cambios (`messages_undo`).

## 6. Decisiones de diseño notables

1. **Cliente/servidor desde el día uno** — habilitó TUI/web/desktop/VS
   Code sin duplicar lógica; hizo *barata* la reescritura total de la TUI
   (el frontend era desechable porque el contrato era HTTP+SSE+OpenAPI).
2. **Por qué Go para la TUI (era sst)**: Charm/Bubbletea era lo mejor
   disponible para TUIs; el core en TypeScript por el ecosistema AI. Costo
   aceptado: dos lenguajes + SDK generado como pegamento.
3. **Por qué la abandonaron (v1.0.0)**: "performance and capability
   issues" — el re-render string-based de Bubbletea + glamour por delta
   escala mal; además querían una sola base TypeScript/SolidJS para TUI,
   web y desktop. Rechazaron Ink explícitamente.
4. **SDK generado desde OpenAPI (Stainless)** — el contrato es un
   artefacto generado, no mantenido a mano.
5. **Event bus persistente**: cada mutación se persiste y broadcastea —
   múltiples frontends observan la misma sesión en vivo.
6. **Arquitectura V2 en curso**: re-modularización sobre Effect, TUI como
   un cliente más.

### Implicaciones para braze

- Valida el split engine/CLI existente; exponer `braze-engine` tras
  HTTP+SSE (axum) haría la TUI un cliente reemplazable — opción futura.
- ratatui es immediate-mode con buffer diffing nativo — estructuralmente
  más cercano a OpenTUI que a Bubbletea, así que el problema que mató a la
  TUI Go afecta menos; el punto débil equivalente es re-parsear markdown
  por delta → copiar el parse incremental con frontera de estabilidad.
- Taxonomía de componentes probada dos veces: chat viewport, textarea con
  completions, stack de modals (permisos, selectores), status bar, toasts,
  diff viewer, temas como data (JSON), leader-key.
- El flujo de permisos como evento del stream mapea directo a
  `braze-permissions` + `AgentEvent`.

**Fuentes**: [anomalyco/opencode](https://github.com/anomalyco/opencode)
(README v0.6.4, `packages/tui/go.mod` v0.6.4, `packages/tui/package.json`
en `dev`, [release v1.0.0](https://github.com/anomalyco/opencode/releases/tag/v1.0.0)),
[anomalyco/opentui](https://github.com/anomalyco/opentui),
[opentui.com docs](https://opentui.com/docs/getting-started/),
[opencode.ai/docs/tui](https://opencode.ai/docs/tui/),
[deep dive de Moncef Abboud](https://cefboud.com/posts/coding-agents-internals-opencode-deepdive/),
[DeepWiki](https://deepwiki.com/anomalyco/opencode/1.2-architecture-overview),
[HN rename sst→anomalyco](https://news.ycombinator.com/item?id=46552218)

---

# Informe 4: ecosistema Rust/ratatui para una TUI de chat agéntico (2025–2026)

## 1. Estado actual de ratatui

**Versión estable: `ratatui` 0.30.2** (junio 2026), MSRV 1.88, edición
2024. La serie 0.30 fue "el release más grande" del proyecto:

- **Modularización**: `ratatui-core` (traits/buffer/layout — lo que deben
  targetear autores de widgets), `ratatui-widgets`, `ratatui-macros`,
  backends separados `ratatui-crossterm`, `ratatui-termion`,
  `ratatui-termwiz`, `ratatui-termina`. Soporte `no_std`. Helper
  `ratatui::run()`. Feature `layout-cache` en defaults.
- **Modelo de render: immediate mode con doble buffer.** Cada frame la app
  redibuja todos los widgets a un `Buffer`; ratatui diffea contra el
  buffer anterior y solo emite las celdas que cambiaron. Implicación:
  redibujar "todo" cada frame es barato en celdas, pero *construir* el
  contenido (parsear markdown, wrappear) cada frame no lo es — cachear la
  representación, no el render.
- **Integración con tokio**: ratatui es sync; el patrón async oficial es
  `crossterm` feature `event-stream` → `EventStream` + `tokio::select!`
  sobre {input, tick, canal de eventos}, con `CancellationToken`.
  Documentado en [Async Counter App](https://ratatui.rs/tutorials/counter-async-app/).

## 2. El problema del scrollback (el punto de diseño más importante)

Por defecto ratatui usa **alternate screen**: al salir, el historial
desaparece. Para un chat estilo Claude Code/Codex el patrón es:

**`Viewport::Inline(height)` + `Terminal::insert_before`**: la TUI ocupa
solo N filas al fondo, y el contenido finalizado se "imprime" hacia
arriba, quedando en el scrollback nativo ([ejemplo oficial
Inline](https://ratatui.rs/examples/apps/inline/)).

**Limitaciones conocidas (verificadas en issues):**

- [#1426](https://github.com/ratatui/ratatui/issues/1426): `insert_before`
  limitado por `u16::MAX` en altura; no existe `delete_before`
  (append-only, coherente con un chat).
- [#584](https://github.com/ratatui/ratatui/issues/584): flickering con
  alto throughput de `insert_before`. Solucionado con la feature
  **`scrolling-regions`** ([PR #1341](https://github.com/ratatui/ratatui/pull/1341)),
  que usa scroll regions del terminal (DECSTBM). **Activarla sí o sí para
  streaming.**
- [#984](https://github.com/ratatui/ratatui/issues/984) /
  [#2086](https://github.com/ratatui/ratatui/issues/2086) /
  [PR #1964](https://github.com/ratatui/ratatui/pull/1964): el **resize es
  el talón de Aquiles del viewport inline** — resize horizontal corrompe
  el render; el texto ya insertado en el scrollback no re-wrappea (es
  texto plano del terminal).

**Cómo lo resuelve Codex CLI**: no usa `insert_before` de stock — tiene
`custom_terminal.rs` + `insert_history.rs` (escapes ANSI propios, scroll
region sobre el viewport, reverse-index, wrapping propio con
`live_wrap.rs`, modo `ZellijRaw` para multiplexores). Además exponen
`--no-alt-screen` porque Zellij no tiene scrollback utilizable en pantalla
alternativa.

**Conclusión para braze-tui**: empezar con `Viewport::Inline` +
`insert_before` + feature `scrolling-regions` (suficiente para MVP),
sabiendo que la ruta de madurez es el patrón Codex (inserción ANSI propia)
si el resize/wrapping duele.

## 3. Widgets y crates útiles para chat

| Necesidad | Crate | Versión | Notas |
|---|---|---|---|
| Markdown → `Text` de ratatui | [`tui-markdown`](https://crates.io/crates/tui-markdown) | 0.3.8 (jun 2026) | pulldown-cmark 0.13; feature `highlight-code` = syntect + `ansi-to-tui`. Mantenido (joshka). |
| Markdown con wrapping a ancho | [`ratskin`](https://lib.rs/crates/ratskin) | 0.3.1 (ene 2026) | Wrapper de `termimad`. **Ya no mantenido** (sucesor: `mdfrier`). |
| Syntax highlighting | `syntect` + `ansi-to-tui` | — | Lo que usa tui-markdown por debajo. |
| Input multi-línea | [`tui-textarea`](https://github.com/rhysd/tui-textarea) | 0.7.0 (rhysd) | Estándar de facto; fork mantenido: [`ratatui-textarea`](https://github.com/ratatui/ratatui-textarea). Alternativa vim-like: `edtui`. |
| Scroll de listas largas | `tui-scrollview`, `tui-widget-list` | — | Solo relevantes con alternate screen; con inline+scrollback el scroll lo hace el terminal. |

Advertencia: Codex **no** usa tui-markdown — tiene markdown propio, porque
el markdown streaming + wrapping estable requiere control que los crates
genéricos no dan.

## 4. Streaming token a token y throttling

1. **Coalescing de redraws con canal de capacidad 1**: `FrameRequester`
   con `broadcast::Sender<()>` de capacidad 1; cada evento llama
   `schedule_frame()`, los requests intermedios se dropean, el loop dibuja
   máximo una vez por wakeup. Nunca dibujar "en cada token"
   ([issue #1338](https://github.com/ratatui/ratatui/issues/1338): CPU
   altísimo sin dirty-checking).
2. **Celda activa mutable + commit al scrollback**: el texto en streaming
   vive en una "active cell" del viewport que muta in-place; al
   finalizarse un bloque (gated por newlines completos), sus líneas se
   mueven al scrollback y salen del ciclo de render para siempre. Costo de
   render O(viewport), no O(historial).
3. **Synchronized output**: envolver cada draw en BSU/ESU (`sync_update`
   de crossterm) — elimina tearing.
4. **Cache del layout del bloque activo**: re-parsear/re-wrappear solo la
   cola del markdown en streaming.
5. **Batching de historia pendiente**: acumular `pending_history_lines` y
   flushear en el próximo draw.

## 5. Proyectos de referencia

- **[openai/codex — codex-rs/tui](https://github.com/openai/codex/tree/main/codex-rs/tui)** —
  la referencia número uno: exactamente este dominio, ratatui + tokio,
  Apache-2.0. Estructura clave: `App` (coordinador + event bus),
  `ChatWidget` (transcript + `active_cell`), **trait `HistoryCell`**
  (mapea 1:1 a `AgentEvent`), `BottomPane` (`ChatComposer` + stack de
  overlays).
- **[oatmeal](https://github.com/dustinblackman/oatmeal)** — chat con
  burbujas, traits `Backend`/`Editor`, sesiones, slash commands. Útil como
  referencia de UX pero **dormante** (última release mar 2024) y alternate
  screen clásico.
- **[tenere](https://github.com/pythops/tenere)** — TUI para LLMs con
  keybindings vim, backends ChatGPT/llama.cpp/Ollama. Buena referencia de
  event loop tokio pequeño.
- Más en [awesome-ratatui](https://github.com/ratatui/awesome-ratatui).

Lección transversal: oatmeal/tenere (alternate screen + scroll propio) son
la generación anterior; Codex (inline + scrollback nativo) es el patrón
que los usuarios de coding agents esperan hoy.

## 6. Alternativas a ratatui

- **[iocraft](https://github.com/ccbrown/iocraft)** — declarativo estilo
  React. Elegante pero ecosistema chico y sin el control fino de
  viewport/ANSI que necesita el truco del scrollback.
- **[rooibos](https://github.com/aschey/rooibos)** — reactivo (señales de
  Leptos) sobre ratatui. **Explícitamente pre-alpha**.
- **[cursive](https://github.com/gyscos/cursive)** — retained mode, event
  loop propio. Maduro pero el modelo invertido encaja mal con
  `tokio::select!` sobre streams de un motor externo.
- Capas sobre ratatui: `tui-realm`, `ratatui-reactive` — añaden estructura
  sin resolver el scrollback.

**Veredicto**: ratatui sigue siendo el default correcto en 2026 —
mantenimiento muy activo, ecosistema de widgets más grande, tutoriales
async oficiales, y la validación de producción más fuerte posible para
este caso de uso exacto: Codex CLI.

Fuentes principales:
[ratatui releases](https://github.com/ratatui/ratatui/releases) ·
[highlights v0.30](https://ratatui.rs/highlights/v030/) ·
[tutorial async](https://ratatui.rs/tutorials/counter-async-app/full-async-events/) ·
[issue #1426](https://github.com/ratatui/ratatui/issues/1426) ·
[PR #1341 scrolling-regions](https://github.com/ratatui/ratatui/pull/1341) ·
[issue #584](https://github.com/ratatui/ratatui/issues/584) ·
[issue #2086](https://github.com/ratatui/ratatui/issues/2086) ·
[issue #1338](https://github.com/ratatui/ratatui/issues/1338) ·
[codex-rs insert_history.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/insert_history.rs) ·
[codex-rs TUI deep dive](https://deepwiki.com/openai/codex/4.1-terminal-user-interface-(tui)) ·
[tui-markdown](https://crates.io/crates/tui-markdown) ·
[tui-textarea](https://github.com/rhysd/tui-textarea) ·
[oatmeal](https://github.com/dustinblackman/oatmeal) ·
[tenere](https://github.com/pythops/tenere) ·
[awesome-ratatui](https://github.com/ratatui/awesome-ratatui)
