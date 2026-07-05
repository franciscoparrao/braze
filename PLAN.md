# Plan de Proyecto: braze

## Context

Este proyecto nace de una conversación exploratoria sobre si construir un software agéntico personalizado a partir de `google-gemini/gemini-cli`. Se investigó también `openai/codex` (CLI de codificación agéntica escrita en Rust, ~90 crates, Apache-2.0) como referencia arquitectónica directa, y se decidió no forkear ninguno de los dos sino diseñar un **motor híbrido propio en Rust**: la infraestructura "dura" que Codex ya resuelve bien (sandboxing, cliente MCP, abstracción de proveedor de modelo) combinada con principios cualitativos de cómo opera Claude Code que no están explícitos en la estructura de crates de Codex (carga diferida de herramientas, compactación diferencial de contexto, ejecución en background con notificación push, y un modelo de permisos de dos capas).

Es explícitamente un **ejercicio de experimentación**, no un compromiso de producto — el alcance del MVP favorece un sistema pequeño y funcional sobre cobertura exhaustiva.

**Nombre del proyecto**: `braze` (verificado disponible en crates.io). Carpeta ya creada: `/home/franciscoparrao/proyectos/braze` (vacía).

**Decisiones ya tomadas con el usuario en esta sesión:**
1. Backends de modelo para el MVP: **Anthropic** (proveedor principal, ya usado a diario) + **Ollama local** (segundo implementador del trait `ModelBackend`, gratis, sin costo de API durante la experimentación).
2. Concurrencia: **todo asíncrono (tokio) en todo el workspace** — el usuario prefirió esto explícitamente sobre la alternativa híbrida (async aislado solo en 2 crates de borde). Esto es una **desviación deliberada** de la convención del resto de sus proyectos Rust (datacube-rs, geostat-rs, swarm-abm son 100% sync + rayon), justificada porque `braze` es un sistema fundamentalmente de I/O concurrente (streaming SSE del modelo, cliente MCP, tareas en background), un dominio distinto al de sus librerías numéricas/geoespaciales.

## Convenciones heredadas (verificadas contra datacube-rs, geostat-rs, swarm-abm)

- Workspace: `[workspace]` raíz, crates en `crates/braze-<rol>`, `[workspace.package]` + `[workspace.dependencies]`, `resolver = "3"`, `edition = "2024"`.
- Errores: `thiserror` v2, un enum `<Crate>Error` por crate en `error.rs`.
- CLI: `clap` v4 con `#[derive(Parser)]`.
- Serde: `serde` + `serde_json` (sin yaml/toml).
- Tests: módulos inline `#[cfg(test)]` (con `#[tokio::test]` donde aplique, dado el workspace async).
- Licencia dual `MIT OR Apache-2.0` vía `license.workspace = true`.
- CI: `.github/workflows/ci.yml` con `fmt --check` → `clippy --workspace --all-targets -- -D warnings` → `test --workspace`.
- Estilo de módulos: archivo-por-módulo (sin `mod.rs`).
- `CLAUDE.md` en la raíz del repo (español), documentando alcance/estado — para `braze` debe indicar explícitamente que es un proyecto de experimentación.

**Desviaciones explícitas de la convención (y por qué):**
- **`tokio` en todo el workspace** (ver arriba) — dominio distinto al resto de sus proyectos.
- **`tracing` + `tracing-subscriber`** (con `EnvFilter`, salida a stderr, sin exportador OTLP en el MVP) — se adopta porque el modo de falla central de un motor agéntico ("la secuencia de tool-calls hizo algo inesperado") es invisible sin trazas estructuradas por turno/tool-call, a diferencia de sus librerías numéricas donde la corrección se valida con tests. Se limita a los crates `braze-engine`, `braze-model`, `braze-mcp-client`, `braze-tools-core`; las librerías nunca instalan el subscriber, solo `braze-cli` lo hace en `main.rs`.

## Arquitectura

### Módulos (crates del workspace, MVP)

| Crate | Responsabilidad | Nivel | Dependencias |
|---|---|---|---|
| `braze-types` | Vocabulario compartido sin lógica: `Message`, `Role`, `ContentBlock`, `ToolCall`, `ToolResult`, `SessionId`, `ToolStub` | 0 | ninguna |
| `braze-events` | `AgentEvent` enum + `TaskNotifier` (dispatch de tareas en background vía `tokio::spawn` + `tokio::sync::mpsc`, notificación push no polling) | 0 | ninguna |
| `braze-config` | Carga/merge de config (env vars, `~/.config/braze/config.json`, overrides de CLI) | 0 | ninguna |
| `braze-permissions` | Modelo de permisos de dos capas: allowlist de directorio de trabajo (MVP, sin Landlock aún) + capa de confirmación de intención (clasificador de acciones irreversibles + callback de confirmación) | 1 | types |
| `braze-session` | Persistencia de sesión en disco (rollout log JSON-lines) + compactación diferencial de contexto (`ContextCompactor`: separa estado durable de ventana táctica) | 1 | types, events |
| `braze-tools-core` | Trait `ToolProvider` + `ToolRegistry` + mecanismo de **carga diferida de herramientas** (índice de nombres, resolución de schema bajo demanda) | 1 | types |
| `braze-model` | Trait `ModelBackend` async + implementaciones `AnthropicBackend` y `OllamaBackend` (streaming SSE/NDJSON) | 1 | types |
| `braze-tools-local` | Herramientas locales built-in implementando `ToolProvider`: leer/escribir/editar archivo, shell exec, grep/glob | 2 | tools-core, types, permissions |
| `braze-mcp-client` | Cliente MCP sobre `rmcp` (SDK oficial), implementa `ToolProvider`, expone nombres primero y schemas bajo demanda | 2 | tools-core, types, permissions |
| `braze-engine` | Loop agéntico: orquesta llamadas al modelo, dispatch de tools, tareas en background + notificación, trigger de compactación, checks de permisos. Raíz de composición. | 3 | permissions, session, tools-core, tools-local, mcp-client, model, events, config |
| `braze-cli` | Binario `clap` v4: `braze chat` (interactivo), `braze run <prompt>` (one-shot), subcomandos de sesión/config | 4 | engine, config, session |

**Total: 11 crates** — deliberadamente pequeño frente a los ~90 de Codex.

### Diferido a Fase 2 (explícitamente fuera del MVP)

`braze-sandbox-linux` (Landlock/seccomp), sandboxing multi-OS, `braze-otel` (export OTLP), `braze-skills` (paquetes de capacidades cargables), `braze-agent-graph` (multi-agente/grafo de threads), `braze-tui` (interfaz de pantalla completa — MVP es CLI de línea), `braze-hooks` (sistema de hooks plugueable — los puntos de hook del MVP van hardcodeados en `braze-engine`).

### Grafo de dependencias

```
Nivel 0 (paralelizable):  braze-types, braze-events, braze-config
Nivel 1:                  braze-permissions, braze-session, braze-tools-core, braze-model
Nivel 2:                  braze-tools-local, braze-mcp-client
Nivel 3 (composición):    braze-engine
Nivel 4 (binario):        braze-cli
```

Sin ciclos: `braze-engine` es el único crate que conoce simultáneamente `tools-local` y `mcp-client` (hermanos, nunca dependen entre sí — eso es lo que mantiene `ToolProvider` como un seam válido). `braze-model` y `braze-tools-core` nunca dependen entre sí. Desde el trabajo de gating de permisos en MCP (ver entrada "Grupo 2 del roadmap SOTA" más abajo), `braze-mcp-client` (Nivel 2) también depende de `braze-permissions` (Nivel 1) — igual que `braze-tools-local` ya hacía desde Fase 4 — sin introducir ningún ciclo ni acoplar `tools-local`/`mcp-client` entre sí.

### Contratos entre módulos (async, dado tokio workspace-wide)

Los traits usados como objetos dinámicos (`Box<dyn ToolProvider>`, `Box<dyn ModelBackend>`) usan el crate `async-trait` para mantener dyn-compatibility.

#### `ToolProvider` (`braze-tools-core/src/provider.rs`) — implementado por `braze-tools-local` y `braze-mcp-client`

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolStub { pub name: String, pub summary: String, pub source: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolSchema { pub name: String, pub description: String, pub input_schema: serde_json::Value }

#[async_trait::async_trait]
pub trait ToolProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    async fn list_stubs(&self) -> Result<Vec<ToolStub>, ToolError>;
    async fn resolve_schema(&self, name: &str) -> Result<Option<ToolSchema>, ToolError>;
    async fn invoke(&self, call: &ToolCall) -> Result<ToolResult, ToolError>;
}

pub struct ToolRegistry { providers: Vec<Box<dyn ToolProvider>> }
impl ToolRegistry {
    pub async fn all_stubs(&self) -> Result<Vec<ToolStub>, ToolError>;
    pub async fn resolve(&self, name: &str) -> Result<ToolSchema, ToolError>;   // el "mecanismo de búsqueda"
    pub async fn dispatch(&self, call: &ToolCall) -> Result<ToolResult, ToolError>;
}
```

#### `ModelBackend` (`braze-model/src/backend.rs`) — implementado por `AnthropicBackend` y `OllamaBackend`

```rust
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub tool_stubs: Vec<ToolStub>,   // solo nombres+resumen, nunca schemas completos por adelantado
    pub system_prompt: String,
    pub max_tokens: u32,
}

pub enum CompletionEvent {
    TextDelta(String),
    ToolCallRequested { id: String, name: String, arguments: serde_json::Value },
    Usage { input_tokens: u32, output_tokens: u32 },
    Done,
}

#[async_trait::async_trait]
pub trait ModelBackend: Send + Sync {
    fn name(&self) -> &str;
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = CompletionEvent> + Send>>, ModelError>;
}
```

`OllamaBackend` habla contra `http://localhost:11434` (API nativa `/api/chat` con streaming NDJSON) — sin API key, sin costo.

#### `SessionStore` / `ContextCompactor` (`braze-session/src/store.rs`)

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AgentEvent {
    UserMessage { text: String },
    AssistantText { text: String },
    ToolCallStarted { id: String, name: String, background: bool },
    ToolCallCompleted { id: String, result: ToolResult },
    CompactionOccurred { summary: String, dropped_tokens_estimate: u32 },
    PermissionRequested { action: String, reversible: bool },
    PermissionDecided { action: String, allowed: bool },
}

#[async_trait::async_trait]
pub trait SessionStore: Send + Sync {
    async fn append(&self, session: &SessionId, event: &AgentEvent) -> Result<(), SessionError>;
    async fn load(&self, session: &SessionId) -> Result<Vec<AgentEvent>, SessionError>;
    async fn list_sessions(&self) -> Result<Vec<SessionId>, SessionError>;
}

pub trait ContextCompactor: Send + Sync {
    fn split(&self, events: &[AgentEvent]) -> (DurableState, Vec<AgentEvent>);
    fn compact_tactical(&self, tactical: &[AgentEvent]) -> Result<String, SessionError>;
}
```

MVP: la compactación diferencial ship con un split real pero simple (durable = resultados de tools + decisiones ya resumidas; táctico = últimos N turnos crudos), no un summarizer afinado.

#### `TaskNotifier` (`braze-events/src/notify.rs`) — background + push notification

```rust
#[async_trait::async_trait]
pub trait TaskNotifier: Send + Sync {
    fn spawn(&self, task: BackgroundTask) -> TaskHandle;   // no bloqueante, tokio::spawn interno
    async fn next_completed(&self, timeout: std::time::Duration) -> Option<(TaskHandle, ToolResult)>;
    // el loop principal espera en el canal (tokio::sync::mpsc + tokio::time::timeout),
    // nunca hace polling activo de estado
}
```

## Alcance del MVP

**Incluido:**
- Loop de tool-calling funcionando contra Anthropic real y Ollama local.
- `braze-tools-local` + `braze-mcp-client` (con al menos un servidor MCP real por stdio) ambos implementando `ToolProvider`.
- Carga diferida de herramientas end-to-end (`ToolRegistry::list_stubs`/`resolve`), verificada con un servidor MCP sintético con suficientes tools para que cargar schemas completos infle visiblemente el prompt.
- Persistencia de sesión a disco (JSON-lines) + compactación diferencial (split real pero simple).
- Capa mínima de confirmación de permisos: tabla fija de "siempre confirmar" (git push/force-push, `rm -rf`, escrituras fuera del cwd) + prompt y/n en terminal — sin enforcement de sandboxing a nivel de SO todavía.
- Dispatch de tareas en background + notificación push vía `braze-events` (tokio nativo).
- `braze-cli` con `chat` y `run <prompt>`.

**Diferido a Fase 2:** ver sección anterior (sandboxing SO, multi-agente, TUI, otel, skills-packs, hooks plugueables).

## Fases de Implementación

### Fase 1: Scaffold (orquestador) — COMPLETA (2026-07-03)
- [x] Escribir este PLAN.md en `/home/franciscoparrao/proyectos/braze/PLAN.md`
- [x] Crear `Cargo.toml` workspace raíz + `crates/braze-*` (solo manifiestos + `lib.rs`/`main.rs` esqueleto, sin lógica)
- [x] Escribir los traits/tipos compartidos completos (`braze-types`, `braze-events`, y las firmas de `ToolProvider`/`ModelBackend`/`SessionStore` arriba) — estos son el contrato congelado
- [x] `.github/workflows/ci.yml`, `CLAUDE.md`, `LICENSE-MIT`/`LICENSE-APACHE`, `.gitignore`
- [x] `cargo build --workspace` y `cargo test --workspace` verdes (1 test, roundtrip de `SessionId`)

**Nota de implementación**: `ToolStub` se movió de `braze-tools-core` a `braze-types` (no estaba en el borrador original de esta sección) para evitar que `braze-model` dependiera de `braze-tools-core` — ambos son crates hermanos de Nivel 1 y no deben depender entre sí (ver grafo de dependencias). `rmcp` fijado en `"2"` (no `"0.1"`, versión real verificada en crates.io: 2.1.0). `braze-tools-core::ToolRegistry` quedó con la struct y las firmas de método completas pero cuerpos `todo!()` — su implementación real es trabajo de Fase 3 (Agente C), igual que el resto de la lógica de negocio de los crates de Nivel 1+.

### Fase 2: Implementación — Nivel 0 — COMPLETA (2026-07-03)
- [x] `braze-types` + `braze-events` — ya completos desde Fase 1 (el orquestador los escribió como parte del contrato congelado, no hizo falta un agente aparte)
- [x] `braze-config` → 1 subagente (sin worktree — no había paralelismo que aislar). `Config::load()` con merge defaults→archivo JSON XDG-aware→env `BRAZE_*`→overrides explícitos (`ConfigOverrides`, el seam que usará `braze-cli` en Fase 5). 22 tests + 1 doctest, `cargo build --workspace`/`cargo test --workspace` verificados independientemente por el orquestador tras el reporte del agente.
- [x] (scaffolding de repo ya cubierto en Fase 1 por el orquestador)

**Decisiones de diseño del agente, aceptadas sin cambios**: una sola struct `ConfigOverrides` (sparse, todo `Option`) reutilizada para archivo/env/CLI en vez de tres tipos distintos; resolución de env vía iteradores inyectables (no `std::env::set_var` en tests, unsafe en edition 2024); XDG resuelto a mano (`$XDG_CONFIG_HOME`/`$XDG_DATA_HOME`/`$HOME`) sin agregar la dependencia `dirs` (MVP es Linux-only por PLAN.md); variables `BRAZE_*` desconocidas se ignoran silenciosamente (forward-compat, no fail-fast).

### Fase 3: Implementación paralela — Nivel 1 (4 subagentes) — COMPLETA (2026-07-03)
- [x] `braze-permissions` → Agente A. Contrato nuevo (diseñado en esta sesión vía Plan Mode, no existía antes): `WorkdirAllowlist` (allowlist léxica, sin canonicalize), `DefaultClassifier`/`Reversibility` (tabla fija git push/rm -rf/escrituras fuera del cwd), `ConfirmationPrompt` (async, default seguro = denegar ante fallo de lectura), `PermissionGuard` (punto de entrada único). 35 tests, clippy limpio.
- [x] `braze-session` (`ContextCompactor` — pieza más novedosa) → Agente B. `FileSessionStore` (JSON-lines, mutex coarse para single-writer) + `SimpleContextCompactor` (ventana táctica de 20 eventos por defecto, invariante de no-pérdida verificada con property test barriendo tamaños de ventana). 11 tests.
- [x] `braze-tools-core` (`ToolRegistry` — cuerpos `todo!()` rellenados) → Agente C. `all_stubs` concurrente (`join_all`), `resolve`/`dispatch` secuenciales por orden de registro, `tracing::debug!` en la resolución bajo demanda (verificable con `RUST_LOG=braze=debug`). 6 tests.
- [x] `braze-model` (`AnthropicBackend` + `OllamaBackend`) → Agente D. Parser SSE manual (Anthropic, acumula `input_json_delta` por bloque hasta `content_block_stop`) y NDJSON manual (Ollama, nativo `/api/chat`). Cero dependencias nuevas (ni siquiera de test — servidor TCP falso hecho a mano). 36 tests.
- [x] Verificación independiente del orquestador: `cargo build --workspace`, `cargo test --workspace` (110 tests + 2 doctests, todos verdes), `cargo clippy --workspace --all-targets -- -D warnings` (limpio, incluido el warning de `dead_code` que quedaba de Fase 1).

**Gap resuelto en esta fase**: `CompletionRequest.tool_stubs` (solo nombre+resumen) vs. el `input_schema` que exige la API de Anthropic — `AnthropicBackend`/`OllamaBackend` envían un schema permisivo genérico (`{"type":"object","additionalProperties":true}`) por stub; la validación real del schema queda como deuda explícita y documentada para `braze-engine` en Fase 5 (comentario en `anthropic_wire.rs::build_tools`).

### Fase 4: Implementación paralela — Nivel 2 (2 subagentes, requiere `braze-tools-core` congelado)
- [ ] `braze-tools-local` → Agente A
- [ ] `braze-mcp-client` (sobre `rmcp`) → Agente B

### Fase 5: Integración (orquestador, secuencial) — COMPLETA (2026-07-03)
- [x] Dos enmiendas aditivas previas: `braze-events::AgentEvent` gana `AssistantToolCall{id,name,arguments}` (necesaria para reconstruir el historial de mensajes — Anthropic exige el bloque `tool_use` del asistente en el historial antes del `tool_result` correspondiente); `braze-config::Config` gana `anthropic_model: Option<String>` (sin default) y `ollama_model: String` (default `"llama3.1"`), con su `ConfigOverrides`/env (`BRAZE_ANTHROPIC_MODEL`/`BRAZE_OLLAMA_MODEL`) siguiendo el mismo patrón que los campos existentes. La nueva variante de `AgentEvent` rompió dos `match` exhaustivos (sin wildcard) en `braze-session::simple_compactor.rs` (`approx_char_len` y el conteo de `compact_tactical`) que no estaban cubiertos por el `matches!` de `is_settled_durable` — no es un archivo congelado (solo `compactor.rs`/`store.rs`/`provider.rs`/`backend.rs` lo son), así que se agregó un brazo explícito a cada match en vez de tocar el contrato.
- [x] `braze-engine`: `Engine` (composición de `ModelBackend` + `ToolRegistry` (envuelto en `Arc` para compartirse dentro de las tasks en background) + `SessionStore` + `ContextCompactor` + `TaskNotifier`), `run_turn` implementado siguiendo el algoritmo especificado (loop con tope de seguridad de 20 iteraciones, streaming real vía `on_text`, compactación diferencial disparada a 40 eventos tácticos). `history::build_messages` reconstruye `Vec<Message>` desde `DurableState`+eventos tácticos (documentado como simplificación MVP: 1 evento → como máximo 1 `Message`, en vez de agrupar varios `ToolUse`/`ToolResult` consecutivos del mismo rol). 7 tests (5 de `history::build_messages`, 2 de `Engine::run_turn` end-to-end con mocks: turno sin tool call, turno con tool call completo incluyendo dispatch en background + notificación).
- [x] `braze-cli`: subcomandos `chat`/`run` (clap v4 derive), carga de config + overrides de `--backend`/`--model` vía el seam `ConfigOverrides` ya expuesto por `braze-config`, construcción de `AnthropicBackend`/`OllamaBackend` según backend resuelto, capa de permisos (`WorkdirAllowlist` + `DefaultClassifier` + `TerminalConfirmationPrompt` nuevo), `LocalToolsProvider` + conexión best-effort a los `McpServerConfigStub` configurados, `ChannelTaskNotifier` nuevo (implementación concreta de `TaskNotifier`, ver nota de diseño abajo), `FileSessionStore`+`SimpleContextCompactor::default()`, resolución de `SessionId` (nueva o `--resume`).
- [x] Verificación independiente del orquestador: `cargo build --workspace` limpio, `cargo test --workspace` (170 tests + 1 doctest, todos verdes), `cargo clippy --workspace --all-targets -- -D warnings` (limpio).

**Nota de diseño — `TaskNotifier`**: PLAN.md declara esta responsabilidad de `braze-events`, pero no había ninguna implementación concreta (solo el trait) hasta esta fase. `ChannelTaskNotifier` (tokio::spawn + mpsc + `AtomicU64` para handles) se implementó originalmente en `braze-cli/src/channel_notifier.rs` por simplicidad de la Fase 5. **Movido a `braze-events` en la limpieza de deuda técnica del 2026-07-04** (su hogar declarado desde el principio) — `braze-cli` ahora solo importa `braze_events::ChannelTaskNotifier`. De paso se le agregaron 2 tests unitarios que no tenía.

### Fase 6: Verificación (subagentes)
- [ ] Agente de tests: `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Agente de review de código
- [ ] Verificación manual end-to-end (ver sección siguiente)

### Fase 4: Implementación paralela — Nivel 2 (2 subagentes) — COMPLETA (2026-07-03)
- [x] `braze-tools-local` → `LocalToolsProvider`, un único `ToolProvider` que fronta 6 tools (read_file, write_file, edit_file, shell_exec, grep, glob). Escrituras/shell pasan por `PermissionGuard`; lecturas no. grep/glob implementados shelleando a `grep`/`find` del sistema (sin dependencias nuevas). 33 tests.
- [x] `braze-mcp-client` → `McpToolProvider` sobre `rmcp` real (API verificada contra el source vendored, no inventada: `TokioChildProcess`, `ServiceExt::serve()`, `Peer<RoleClient>::{list_all_tools, call_tool}`). Probado con un servidor MCP de juguete real (`src/bin/toy_mcp_server.rs`) spawneado como subproceso — 11 tests de integración + 8 unitarios de truncado de summary.
- [x] Verificación independiente del orquestador: `cargo build/test --workspace` (162 tests + 2 doctests, verdes), `cargo clippy --workspace --all-targets -- -D warnings` (limpio).

**Deuda técnica — investigada (2026-07-04), no tiene arreglo simple en Cargo estable**: en `braze-mcp-client/Cargo.toml`, `rmcp` con features de servidor (`server`, `transport-io`) sigue en `[dependencies]` normales, no en `[dev-dependencies]`, porque el `[[bin]]` `toy_mcp_server` (usado solo por los tests de integración) no puede ver dev-dependencies en un `cargo build` plano. Se intentó el arreglo "correcto" (mover `toy_mcp_server` a un crate nuevo `braze-mcp-toy-server`, agregado como `[dev-dependencies]` de path) y **no funciona en Cargo estable**: `CARGO_BIN_EXE_<bin>` solo se define para binarios del propio paquete bajo test, no para binarios de crates en `[dev-dependencies]` (ni con un target `lib` presente) — se necesitaría la feature de "artifact dependencies" (`-Z bindeps`), que sigue siendo nightly-only. Se revirtió el intento (build+tests verificados de vuelta en verde) y se deja la solución actual (documentada in-line en el Cargo.toml) como definitiva salvo que Rust estabilice bindeps o que aparezca una razón real de peso (ej. publicar el crate a crates.io, donde el tamaño de dependencias sí importa).

## Verificación end-to-end (tras Fase 5)

1. `cargo build --workspace` compila sin warnings.
2. `braze run "lista los archivos en /tmp" --backend ollama` — corre contra Ollama local (gratis), confirma que el loop de tool-calling + `braze-tools-local` funcionan sin tocar la API de pago.
3. `braze run "..." --backend anthropic` — mismo prompt contra Anthropic real, confirma que `ModelBackend` no es un one-off atado a un solo proveedor.
4. Conectar un servidor MCP de prueba (p. ej. `@modelcontextprotocol/server-filesystem` vía stdio) y verificar en logs (`RUST_LOG=braze=debug`) que solo se listan `ToolStub` (nombres) hasta que el modelo intenta invocar una tool específica — ahí debe verse la resolución de schema bajo demanda.
5. Disparar una acción de la tabla de confirmación (ej. pedirle que borre un archivo fuera del cwd) y verificar que el prompt y/n intercepta antes de ejecutar.
6. Matar el proceso a mitad de sesión y verificar que `braze chat --resume <session-id>` recupera el historial desde el rollout log en disco.

### Resultados de la validación manual (2026-07-03/04) — pasos 1, 2, 4, 6 confirmados

- **Paso 1**: `cargo build --workspace` limpio.
- **Paso 2 (Ollama)**: `braze run "..." ` contra `llama3.2:1b`, `qwen2.5:3b` y `qwen2.5:7b` reales. El loop completo funciona de punta a punta: tool call → error de argumentos → recuperación → persistencia en JSONL.
- **Paso 4 (carga diferida)**: **confirmado en logs reales** — `RUST_LOG=braze=debug` muestra `resolved full tool schema on demand tool="read_file" provider="local"` exactamente en el momento en que el modelo pide invocar esa tool, nunca antes.
- **Paso 6 (resume)**: confirmado — `braze chat --resume <id>` recupera el historial y el modelo mantiene contexto de la sesión anterior.
- **Hallazgo importante (no un bug, evidencia de la deuda ya documentada)**: con `llama3.2:1b` el modelo invocó `read_file` con argumentos inventados (`{"fn":"...", "mode":"r"}` en vez de `{"path":"..."}`) porque el `input_schema` que le llega es el genérico permisivo (`additionalProperties: true`), no el real — exactamente el gap flageado en Fase 3/5 para resolver en una futura validación real de schema. Con `qwen2.5:3b`/`qwen2.5:7b` (mejor soporte de tool-calling nativo) el argumento salió correcto (`{"path":"Cargo.toml"}`) en ambos intentos, y sí trajo el contenido real del archivo. Conclusión: el bug de diseño es real pero su impacto depende fuertemente de la capacidad del modelo — no bloquea el MVP, pero es la prioridad más clara para una próxima iteración de `braze-engine`.
- **Limitación de entorno detectada (no de braze)**: la máquina de pruebas es CPU-only (sin GPU) y en el momento de la prueba tenía load average 12-15 y swap casi lleno por procesos ajenos al proyecto — los turnos con `qwen2.5:7b`/`qwen2.5:3b` no alcanzaron a completar la respuesta final dentro de 180-400s en esas condiciones. No se investigó más a fondo por decisión explícita (prioridad baja frente a la evidencia de corrección ya obtenida).
- **Paso 5 (confirmación y/n) — verificado (2026-07-04)**: `braze run 'Use the write_file tool with path "/tmp/.../outside.txt" ...'` contra `llama3.2:1b`, respondiendo "y" por stdin. El prompt real apareció exactamente como se diseñó (`write file /tmp/.../outside.txt\n¿Permitir? [y/N]: `), la escritura fuera del `cwd` se clasificó `Irreversible` correctamente, y el archivo se creó tras la confirmación. La rama de rechazo ("n") no se logró disparar en vivo tras 3 intentos — el modelo formó mal el JSON del tool call antes de llegar al chequeo de permisos en los 3 casos (mismo problema de siempre, no uno nuevo) — pero se verificó por inspección directa de `TerminalConfirmationPrompt::confirm`: es la misma lectura de stdin ya probada en vivo, comparada contra un literal distinto (`"y"`/`"yes"` → cualquier otra respuesta cae al mismo `else` → `false`/denegado), y la semántica de denegación de `PermissionGuard::check` ya tiene cobertura unitaria dedicada desde la Fase 3.
- **Pendiente**: paso 3 (Anthropic real, implica costo de API).

## MVP cerrado (2026-07-04) — tag `v0.1.0`

5 de 6 pasos de verificación end-to-end confirmados en vivo (el único pendiente, Anthropic real, se difiere a criterio del usuario por su costo, no por duda técnica). Las 6 fases del plan original están completas, verificadas independientemente en cada una, y pusheadas a `github.com/franciscoparrao/braze` (rama `main`, tag `v0.1.0`).

**Próximo incremento más claro si se retoma el proyecto** (implementado el 2026-07-04, ver § "Grupo 1 del roadmap SOTA" más abajo): validación real de tool schema en `braze-engine` antes del dispatch final — hasta esta fecha solo hacía un `resolve()` best-effort sin validar; era la deuda con más evidencia empírica detrás, confirmada en vivo múltiples veces durante esta sesión de validación manual (modelos chicos arman argumentos inventados contra el schema genérico permisivo que usa `braze-model`). Ver también la lista de items diferidos a "Fase 2" en la sección de Arquitectura (sandboxing SO, multi-agente, TUI, otel, skills-packs, hooks plugueables).

**Investigación de estado del arte (2026-07-04)**: ver `docs/SOTA-2026-07.md` — dos estudios profundos (práctica de industria + literatura académica) con roadmap priorizado combinado. Resumen: (1) validación real de schema + reintento acotado, (2) `PermissionGuard` por niveles de riesgo + patrón de dos pasadas cobertura-luego-auditoría (evidencia de AuthBench, mayo 2026), (3) compactor con limpieza quirúrgica de `tool_result` antes de una arquitectura de memoria de 3 capas, (4) TTL/caché de catálogo MCP. La literatura confirma que el compactor 100% determinístico de `braze-session` (sin LLM/RL) es una decisión de diseño defendible, no una simplificación pobre.

## Grupo 2 del roadmap SOTA — PermissionGuard por niveles de riesgo (2026-07-04)

Implementa el punto 2 de `docs/SOTA-2026-07.md` § "Roadmap priorizado" tal como fue diseñado: reemplazo del clasificador de shell (allow-por-defecto, dos patrones prohibidos) por un allowlist explícito default-deny, mecanismo de "recordar por sesión" en memoria, y extensión del gating de `PermissionGuard` a las tool calls de `braze-mcp-client` (que hasta ahora no tenía ningún control de permisos).

**Qué se hizo**:
- `braze-permissions::classifier`: `ShellCommand` pasó de "Reversible salvo `git push`/`rm -rf`" a "Irreversible salvo que coincida con `is_safe_shell_command`" — un allowlist explícito de utilitarios de solo lectura (`ls`, `cat`, `pwd`, `echo`, `wc`, `diff`, `whoami`, `date`, `env`, `which`, `true`, `false`, `head`, `tail`, `file`, `grep`) más un subconjunto no-mutante de `find` (rechaza `-delete`/`-exec`/`-execdir`/`-ok`/`-okdir`/`-fprint*`) y `git` (`status`/`diff`/`log`/`show`, o `branch` sin argumentos). `mv`, `dd`, `curl`, `chmod -R`, y un `rm` sin flags — que antes pasaban sin confirmar — ahora son `Irreversible`.
- `braze-permissions::action`: nueva variante `ActionDescriptor::McpToolCall { server, tool }`, siempre clasificada `Irreversible` (un servidor MCP es código arbitrario sin subconjunto seguro por construcción).
- `braze-permissions::allowlist`: extraído `WorkdirAllowlist::resolve` (antes lógica inline duplicada en `is_allowed`) para que `guard.rs` pueda resolver rutas a la misma forma canónica al construir su clave de sesión.
- `braze-permissions::guard`: `PermissionGuard` gana una caché en memoria (`Mutex<HashSet<RememberKey>>`, nunca persistida a disco) de acciones irreversibles ya confirmadas en la sesión — una repetición de la misma clave (programa+subcomando de shell, ruta resuelta de write/delete, servidor+tool de MCP) no vuelve a preguntar; una denegación nunca se recuerda.
- `braze-mcp-client`: `McpToolProvider` gana un campo `guard: PermissionGuard` (cuarto parámetro nuevo de `connect`) y chequea `ActionDescriptor::McpToolCall` antes de despachar cualquier `invoke` — cierra el gap de que cualquier servidor MCP conectado ejecutaba sin restricción alguna.
- `braze-cli`: la construcción del guard se extrajo a `build_permission_guard(cwd)`, reusada para el `LocalToolsProvider` y para un guard nuevo e independiente por cada servidor MCP conectado en el loop existente.

**Tests agregados/cambiados**: `braze-permissions` pasó de 35 a 49 tests (clasificador: comandos seguros nuevos, `find`/`git` con y sin flags mutantes, regresión explícita de `mv`/`curl`/`chmod -R`/`dd`, `McpToolCall` siempre irreversible, `Display` de `McpToolCall`; guard: recordar-por-sesión con claves repetidas/distintas, denegación nunca recordada). `braze-mcp-client` ganó un test de integración (`invoke_is_denied_by_a_guard_that_always_refuses`) y su helper `connect()` de test ahora construye y pasa un guard.

**Verificación**: `cargo build --workspace`, `cargo test --workspace` (187 tests + 1 doctest, todos verdes), `cargo clippy --workspace --all-targets -- -D warnings` (limpio), `cargo fmt --all --check` (limpio).

**Decisión de diseño no completamente especificada de antemano**: `McpToolProvider` guarda el nombre "pelado" del servidor en un campo nuevo `server_name` (en vez de derivarlo quitando el prefijo `"mcp:"` de `provider_id`) — más simple y no depende de que el formato de `provider_id` no cambie nunca.

**Diferido a propósito** (resuelto 2026-07-04, ver § "Persistencia y replay de decisiones de permisos entre `--resume`" más abajo): persistencia de la sesión de "recordar" entre corridas (`--resume`) — ver `docs/SOTA-2026-07.md` § 2 para la razón original del diferimiento.

## Grupo 1 del roadmap SOTA — validación real de schema + reintento acotado (2026-07-04)

Implementa el punto 1 de `docs/SOTA-2026-07.md` § "Roadmap priorizado" tal como fue diseñado: `Engine::dispatch_tool_calls` valida los argumentos que produjo el modelo contra el schema real de la tool antes de despachar, en vez de descartar el resultado de `ToolRegistry::resolve` como hacía el MVP (ver nota de Fase 3 "Gap resuelto en esta fase" y la nota de "MVP cerrado" más arriba — ambas ya actualizadas, ya no describen el gap como pendiente).

**Qué se hizo**:
- Dependencia nueva `jsonschema = "0.46"` (workspace, solo en `braze-engine`) con `default-features = false` — se excluyen deliberadamente `resolve-http`/`resolve-file`/`tls-aws-lc-rs` (traerían una segunda pila TLS junto a la de `reqwest`/`rustls-tls` que ya usa `braze-model`, y los schemas de `braze` son documentos JSON autocontenidos sin `$ref` externos). Verificado (build + ejecución real de `jsonschema::validate` con un schema `required`/`properties`) que ningún feature adicional hace falta.
- `Engine::dispatch_tool_calls` gana un parámetro `retry_counts: &mut HashMap<String, u32>` (vive y muere con el `run_turn` que lo llama, no es un campo de `Engine`). Al resolver el schema de cada tool call vía `self.tools.resolve(&call.name)`, si viene `Ok(schema)` se valida `call.arguments` contra `schema.input_schema` con `jsonschema::validate`. Si la validación falla, se incrementa el contador por nombre de tool y se produce un `ToolCallCompleted { is_error: true, .. }` con: el schema completo + el error de validación en la primera falla de esa tool en el turno (contexto de reparación para que el modelo se corrija), o solo el error (sin el schema) en fallas subsiguientes de la misma tool en el mismo turno — y en ambos casos se hace `continue`, sin `ToolCallStarted` ni dispatch real. `ToolRegistry::resolve` devuelve `Result<ToolSchema, ToolError>` (no `Result<Option<ToolSchema>, ToolError>`): `Err(ToolError::NotFound(_))` es exactamente el caso "ningún proveedor conoce esta tool" (cada `ToolProvider::resolve_schema` ya devuelve `Ok(None)` para nombres desconocidos; el registro solo convierte eso en `NotFound` cuando *ningún* proveedor la reclama), así que se trata igual que "sin schema para validar" — se loggea y se deja caer al dispatch normal, igual que cualquier otro error de resolución.
- **Limitación aceptada y documentada en el código**: el contador de reintentos es por nombre de tool, no por llamada específica — si el modelo invoca la misma tool varias veces en un turno con argumentos distintos, no distingue "primera falla real de esta llamada" de "otra llamada distinta que también falla". Heurística simple y acotada a propósito, no un sistema de correlación preciso por llamada.
- El schema que **ve el modelo** (`braze-model`, el genérico permisivo `{"type":"object","additionalProperties":true}`) no cambió — esta validación es del lado del motor, antes del dispatch; no resuelve el problema de raíz para modelos chicos no especializados que documenta la literatura citada en `docs/SOTA-2026-07.md` § 1.

**Tests agregados/cambiados**: `EchoToolProvider` (fixture de test de `braze-engine`) pasó de un schema genérico `{"type":"object"}` a uno real con `required: ["text"]`, y ganó un contador de invocaciones (`Arc<AtomicU32>`) para poder afirmar que `invoke` nunca corrió para una llamada rechazada por validación. `braze-engine` pasó de 7 a 10 tests: los 2 tests de `Engine::run_turn` existentes se mantienen (uno de ellos ahora también verifica que el conteo de invocaciones es exactamente 1 para argumentos válidos) y se agregaron 3 nuevos — argumentos inválidos seguidos de argumentos corregidos en la ronda siguiente (el primer `ToolCallCompleted` incluye el schema y `invoke` no corrió para esa llamada, el segundo sí corrió con el resultado real), dos fallas seguidas de la misma tool en el mismo turno (el primer mensaje incluye el schema, el segundo no y es más corto, `invoke` nunca corrió), y un test unitario liviano que confirma que el schema permisivo genérico de `braze-model` no rechazaría argumentos arbitrarios si alguna vez se validara contra él.

**Verificación**: `cargo build --workspace`, `cargo test --workspace` (191 tests + 1 doctest, todos verdes), `cargo clippy --workspace --all-targets -- -D warnings` (limpio), `cargo fmt --all --check` (limpio).

**Decisiones de diseño no completamente especificadas de antemano**: (1) el mensaje de contexto de reparación en la primera falla incluye el nombre de la tool, el texto de error de `jsonschema` (`Display` de `ValidationError`), y el `input_schema` completo serializado compacto (`Display` de `serde_json::Value`) con una instrucción explícita de reintentar con argumentos corregidos; el mensaje de fallas subsiguientes es deliberadamente más corto (sin el schema) e incluye una nota de que no se darán más pistas automáticas para esa tool en el turno. (2) La distinción "el schema fue incluido" en los tests se verifica con la substring `"properties"` (aparece en el JSON del schema serializado, nunca en el texto de error de `jsonschema`, que dice `"text" is a required property` en singular) en vez de `"required"`/`"text"` sugeridos en la especificación original — esos dos también aparecen dentro del propio texto de error de `jsonschema`, lo que hubiera dado falsos positivos al verificar que el segundo mensaje *no* incluye el schema.

## Grupo 3 del roadmap SOTA — `durable_events` llega a los mensajes + limpieza quirúrgica de `tool_result` (2026-07-04)

Implementa el "paso barato inmediato" del punto 3 de `docs/SOTA-2026-07.md` § "Roadmap priorizado" ("limpiar `tool_result`, conservar `tool_use`", mirror de `clear_tool_uses_20250919` de Anthropic) sobre `SimpleContextCompactor`/`braze-engine::history`.

**Bug de plomería encontrado y arreglado como parte necesaria de este trabajo, no como tarea aparte**: `DurableState.durable_events` (calculado por `ContextCompactor::split` desde la Fase 3) se computaba pero nunca se leía en código de producción — `braze-engine::history::build_messages` solo usaba `durable.summary`, ignorando por completo el `Vec<AgentEvent>` de `durable_events`. En la práctica, eventos que envejecían fuera de la ventana táctica desaparecían del contexto que ve el modelo salvo por lo que ya hubiera quedado resumido en `summary`. No tenía sentido implementar limpieza quirúrgica sobre datos que ni siquiera llegaban a renderizarse, así que el arreglo de plomería y la limpieza quirúrgica se hicieron en el mismo cambio.

**Qué se hizo**:
- `braze-session::simple_compactor::is_settled_durable` gana `AgentEvent::AssistantToolCall` como cuarto tipo "asentado". Razón: un `tool_use` viejo debe migrar a `durable_events` junto con su `ToolCallCompleted` correspondiente, en el mismo orden relativo — de lo contrario el `tool_use` quedaría huérfano en `tactical` mientras su `tool_result` va a `durable_events`, inconsistente ahora que ambos se renderizan.
- `braze-engine::history::build_messages` reconstruye el orden: resumen (si hay) → `durable.durable_events` (limpiados vía la función nueva `event_to_message_cleared`) → `tactical` (íntegro, sin cambios de comportamiento).
- `event_to_message_cleared` se comporta idéntico a `event_to_message` para todo tipo de evento excepto `ToolCallCompleted`: si el nombre de la tool asociada (resuelto vía un mapa `id -> nombre` construido una sola vez por llamada a `build_messages` desde los `AssistantToolCall` presentes en `durable_events`, no reconstruido por evento) está en la lista de exclusión, el contenido pasa íntegro; si no, `result.content` se reemplaza por un placeholder (`"[tool result cleared: N chars removed to keep context small; the tool call above is preserved]"`), preservando `is_error`/`tool_use_id`. `CompactionOccurred` sigue mapeando a `None` en esta función también (ya representado en `durable.summary`). Si el id no se resuelve (no debería pasar dado el cambio anterior), se trata como no exento — más seguro limpiar de más que dejar pasar contenido pesado por un caso no cubierto.
- La lista de exclusión (`NEVER_CLEAR_TOOLS`, vacía por defecto — MVP no exime ninguna tool todavía) se pasa como parámetro a una función interna (`build_messages_with_never_clear`) en vez de leerse directo de la const dentro de la función de limpieza, para que los tests puedan ejercer ambas ramas sin mutar estado global.
- Ningún cambio a `AgentEvent`, `ContentBlock`, ni a las firmas de `ContextCompactor`/`DurableState` — son contratos congelados. `compact_tactical()` y el disparador de compactación en `Engine::load_messages` no se tocaron.

**Tests agregados**: `braze-session` pasó de 11 a 13 tests (`is_settled_durable_now_includes_assistant_tool_call`; `split_moves_a_tool_use_and_its_result_together_in_order`, que confirma que un par `AssistantToolCall`+`ToolCallCompleted` migra junto y en orden a `durable_events`). `braze-engine` pasó de 10 a 14 tests: `durable_tool_result_is_cleared_but_tool_use_is_preserved` (contenido largo en `durable_events` → placeholder, `tool_use` intacto), `never_clear_list_exempts_only_the_named_tool` (dos pares de tools distintas, una exenta vía parámetro → solo la no exenta se limpia), `tactical_tool_result_is_never_cleared_regardless_of_length` (evento en `tactical`, nunca limpiado sin importar el largo), `round_trip_orders_summary_then_durable_then_tactical` (orden final: resumen → asentados limpiados en orden → tácticos íntegros en orden). El property test de no-pérdida de `simple_compactor` (`split_never_drops_events_across_a_range_of_window_sizes`) siguió pasando sin modificación — es agnóstico a qué tipos van a cada lado, solo verifica que la suma cuadra.

**Verificación**: `cargo build --workspace`, `cargo test --workspace` (197 tests + 1 doctest, todos verdes), `cargo clippy --workspace --all-targets -- -D warnings` (limpio), `cargo fmt --all --check` (limpio).

**Decisiones de diseño no completamente especificadas de antemano**: (1) el mapa `id -> nombre de tool` se construye una vez por llamada a `build_messages` (función `tool_names_by_id`, un `HashMap<&str, &str>` sobre referencias prestadas de `durable_events`) en vez de recorrer `durable_events` por cada `ToolCallCompleted` — evita ser cuadrático en el número de eventos asentados. (2) `NEVER_CLEAR_TOOLS` quedó como `const` de producción, pero la implementación real toma la lista como parámetro (`&[&str]`) vía una función interna no pública `build_messages_with_never_clear`, de forma que los tests de ambas ramas (exenta / no exenta) no necesitan mutar la constante global.

## Grupo 4 del roadmap SOTA — TTL de cache del catálogo MCP del lado del cliente (2026-07-04)

Implementa el punto 4 de `docs/SOTA-2026-07.md` § "Roadmap priorizado" ("Ecosistema MCP"), pero no tal como estaba escrito: el mecanismo citado en el roadmap original no existe todavía.

**Hallazgo al ir a implementar**: `docs/SOTA-2026-07.md` citaba "adoptar el mecanismo TTL/`cacheScope` (SEP-2549) en `braze-mcp-client`". Verificado (2026-07-04) que **SEP-2549 no existe todavía**: es parte de un release candidate del spec de MCP fechado 2026-07-28 (futuro respecto a la fecha de esta verificación), y no está implementado en `rmcp` 2.1.0 (la versión más reciente publicada en crates.io, ya usada por el proyecto). Búsqueda exhaustiva en el código fuente vendored de `rmcp` confirmó cero campos de TTL/`cacheScope` en `ListToolsResult`/`Tool`.

**Hallazgo adicional, más grave de lo que sugería el doc original**: `McpToolProvider::list_stubs()` hacía *siempre* un round-trip de red (`tools/list`), sin importar si ya tenía datos en cache — y `braze-engine::Engine::run_turn` llama `self.tools.all_stubs()` (que llama `list_stubs()` de cada provider) **una vez por cada ronda modelo↔tool dentro de un turno**, no una vez por sesión — hasta 20 veces en el peor caso (`MAX_TURN_ITERATIONS`). El problema práctico era más urgente que "no hay TTL de protocolo disponible": era una ausencia total de cache respetada del lado del cliente.

**Qué se hizo**: dado que el mecanismo del protocolo no existe, se implementó un TTL basado en tiempo transcurrido del lado del cliente — resuelve el mismo problema práctico sin depender del servidor.
- `braze-mcp-client::provider`: `tool_cache: RwLock<Option<Vec<Tool>>>` pasó a `tool_cache: RwLock<Option<ToolCacheEntry>>`, donde `ToolCacheEntry { tools: Vec<Tool>, fetched_at: tokio::time::Instant }` agrega el timestamp que no existía antes. Constante `TOOL_CACHE_TTL: Duration = Duration::from_secs(60)` — punto de partida razonable, no un valor afinado con datos: comfortablemente más largo que el costo de una sola ronda de turno (cerrando el gap de hasta 20x descrito arriba), y lo bastante corto para que un cambio real de catálogo se recoja dentro de un minuto de quedar obsoleto.
- Método nuevo `tools_respecting_ttl()`: sirve la lista cacheada si `fetched_at.elapsed() < cache_ttl`, si no, hace un `list_tools_fresh()` real (que refresca timestamp). Tanto `list_stubs()` como `resolve_schema()` (intento inicial) rutean por acá, centralizando la política de TTL en un solo lugar.
- `resolve_schema(name)` mantiene el comportamiento de seguridad ya existente: si la tool buscada no aparece en la lista servida por `tools_respecting_ttl()`, fuerza un `list_tools_fresh()` real que **bypassea el TTL** (no solo el cache), antes de responder `None` — cubre el caso de una tool que acaba de aparecer en el servidor mientras el cache seguía "fresco" según el TTL.
- `find_cached` (el helper viejo que buscaba en el cache sin TTL) se eliminó — su única llamadora era el primer intento de `resolve_schema`, que ahora usa `tools_respecting_ttl()` (que ya hace la búsqueda TTL-aware) en su lugar; mantenerlo hubiera sido código muerto duplicando la misma búsqueda.
- API pública de `McpToolProvider::connect` sin cambios (misma firma que usa `braze-cli`). Se agregó un constructor adicional `pub async fn connect_with_ttl(..., cache_ttl: Duration)` (con `connect` implementado como `connect_with_ttl(..., TOOL_CACHE_TTL)`) — no reemplaza a `connect`, solo lo usan los tests de este crate para poder ejercer expiración de TTL con valores de milisegundos en vez de esperar/mockear un reloj real de 60 segundos.

**Tests agregados**: `src/bin/toy_mcp_server.rs` (servidor de juguete usado solo por los tests de integración de este crate) ganó un contador `AtomicU64` de cuántas veces respondió `tools/list`, expuesto vía una tool oculta `call_count` (invocable por `tools/call`, pero nunca listada por `tools/list`, para no alterar el test existente que verifica el conjunto exacto de nombres advertidos). `tests/mcp_toy_server.rs` pasó de 12 a 16 tests: `list_stubs_called_twice_within_the_ttl_only_fetches_once` (dos `list_stubs()` seguidos con TTL largo → 1 sola consulta real, verificado contra el servidor de juguete real vía `call_count`), `list_stubs_refetches_once_the_ttl_has_elapsed` (TTL de 20ms + sleep de 100ms → 2 consultas reales), `resolve_schema_for_a_known_tool_reuses_the_ttl_cache` (confirma que el camino de éxito de `resolve_schema` no golpea la red si el cache sigue fresco), `resolve_schema_bypasses_the_ttl_and_refetches_when_the_tool_is_unknown` (confirma que el fallback de seguridad existente para tools desconocidas se preserva intacto, ahora explícitamente contando el round-trip forzado). Los tests preexistentes de `resolve_schema` (tool conocida, tool desconocida, sin `list_stubs` previo) se mantuvieron sin cambios y siguen pasando.

**Decisiones de diseño no completamente especificadas de antemano**: (1) TTL de producción fijado en 60 segundos — el doc solo pedía "TTL basado en tiempo transcurrido" sin un valor; se eligió por ser cómodamente mayor al costo de un turno completo de reintentos (hasta 20 rondas) pero acotado para no servir un catálogo permanentemente obsoleto tras una reconexión/reconfiguración de servidor en caliente. (2) En vez de instrumentar el conteo de round-trips de forma indirecta o usar solo tests unitarios aislados de la lógica de TTL, se optó por instrumentar directamente el servidor de juguete real (un contador `AtomicU64` expuesto vía una tool nueva no listada) — da cobertura de integración genuina contra el wire real de `rmcp` en vez de solo probar la lógica de cache en aislamiento, a un costo de invasividad bajo (una tool oculta, cero cambios a las tools/tests existentes). (3) El TTL configurable se implementó como un constructor adicional (`connect_with_ttl`) en vez de hacer `TOOL_CACHE_TTL` un valor de instancia siempre configurable desde `connect` — mantiene la firma pública de `connect` sin tocar (restricción explícita, ya que `braze-cli` la llama con 4 argumentos posicionales) mientras da a los tests el control que necesitan.

**Verificación**: `cargo build --workspace`, `cargo test --workspace` (200 tests + 1 doctest, todos verdes), `cargo clippy --workspace --all-targets -- -D warnings` (limpio), `cargo fmt --all --check` (limpio).

## Persistencia y replay de decisiones de permisos entre `--resume` (2026-07-04)

Cierra el gap explícitamente diferido en "Grupo 2 del roadmap SOTA": el mecanismo de "recordar por sesión" de `PermissionGuard` vivía solo en memoria, así que `braze chat --resume <id>` reconstruía un `PermissionGuard` vacío y volvía a preguntar por acciones irreversibles ya aprobadas en esa misma conversación antes de matar el proceso. Este trabajo persiste esas decisiones en el rollout log de la sesión y las reproduce (replay) al reanudar.

**Qué se hizo**:
- `braze-types`: nuevo módulo `permission.rs` con `PermissionKey` (enum `Shell`/`WriteFile`/`DeleteFile`/`McpToolCall`, `Serialize`/`Deserialize`/`Hash`/`Eq`) — movido desde `braze-permissions::guard` (donde vivía como el enum privado `RememberKey`) a `braze-types` para que `braze-events::AgentEvent` pueda embeberlo sin que `braze-events` dependa de `braze-permissions` (misma razón que la ubicación de `ToolStub`, ver `tool.rs`).
- `braze-permissions::guard`: `RememberKey` eliminado; su lógica de derivación pasó de método privado (`PermissionGuard::remember_key`) a función pública libre `derive_permission_key(action: &ActionDescriptor) -> Option<PermissionKey>` — libre (no método) para que `braze-cli::TerminalConfirmationPrompt`, que no tiene una instancia de `WorkdirAllowlist` propia, pueda derivar exactamente la misma key de forma independiente al persistir el evento. `PermissionGuard` gana `pub fn seed_remembered(&self, keys: impl IntoIterator<Item = PermissionKey>)`, aditivo (nunca remueve entradas existentes), para poblar la caché en memoria desde decisiones ya persistidas.
- `braze-events::AgentEvent`: `PermissionRequested`/`PermissionDecided` ganan un campo `#[serde(default)] key: Option<braze_types::PermissionKey>` — `#[serde(default)]` para que un rollout log escrito antes de este cambio siga deserializando (`key: None`).
- `braze-cli::TerminalConfirmationPrompt`: pasó de unit struct a `{ session: SessionId, store: Arc<dyn SessionStore> }`. `confirm()` conserva exactamente la misma lógica de lectura/escritura de stdin/stdout (mismo prompt, mismo default-deny), pero ahora hace un append best-effort (`tracing::warn!` en error, nunca propagado) de `PermissionRequested` antes de mostrar el prompt y de `PermissionDecided` justo antes de retornar — ambos con la `key` derivada vía `derive_permission_key`.
- `braze-engine::Engine`: campo `store` promovido de `Box<dyn SessionStore>` a `Arc<dyn SessionStore>` (mismo cambio de superficie anticipado y diferido en el Grupo 2) — necesario para que `braze-cli` pueda compartir el mismo `SessionStore` entre la construcción de los guards (que necesitan leerlo para el replay) y el `Engine` (que lo usa para escribir). Los usos internos (`self.store.append`/`load`, todos `&self`) no cambiaron.
- `braze-cli::main.rs`: reordenado — `SessionId` ahora se resuelve temprano (antes de construir backend/guards/tools), y `store` se construye como `Arc` justo después del backend de modelo, también antes de los guards. Con `store` ya disponible, `main.rs` hace `store.load(&session)` (tratando `SessionError::NotFound` como sesión nueva, lista vacía) y filtra los eventos a `PermissionDecided { allowed: true, key: Some(key), .. }` para obtener `replayed_keys: Vec<PermissionKey>`. `build_permission_guard` ahora recibe `session`, `store` (clonado vía `Arc::clone` por cada guard — local y uno por servidor MCP) y `replayed_keys`, construye `TerminalConfirmationPrompt::new(session, store)` en vez del unit struct anterior, y llama `guard.seed_remembered(replayed_keys.iter().cloned())` antes de devolver el guard. `Engine::new` recibe el mismo `Arc` (`Arc::clone(&store)`), no una instancia nueva de `FileSessionStore`. Los cuerpos de `Command::Run`/`Command::Chat` ya no resuelven `SessionId` por su cuenta — usan la variable ya resuelta en el paso temprano.

**Tests agregados/cambiados**: `braze-types` gana 4 tests de round-trip serde de `PermissionKey` (uno por variante). `braze-events` gana 1 test (`PermissionDecided` sin campo `key` en el JSON deserializa con `key: None`, simulando un log pre-existente). `braze-permissions` pasa de 49 a 50 tests: 1 test nuevo (`seeding_a_remembered_key_skips_the_prompt_for_the_matching_action`) que siembra una key vía `seed_remembered` y confirma que `check()` con la misma acción nunca invoca el `CountingPrompt` (contador en 0). El test de `confirm()` de `TerminalConfirmationPrompt` que verificaría el append de ambos eventos con la key correcta se omitió — simular una respuesta y/n programática contra `tokio::io::stdin()` real (proceso completo, no mockeable sin reescribir la implementación para inyectar el reader) resultó impracticable con la infraestructura actual del archivo; se priorizó que el código compile con sentido y se compensó con la verificación manual en vivo (ver abajo).

**Verificación**: `cargo build --workspace`, `cargo test --workspace` (206 tests + 1 doctest, todos verdes; sube desde 200+1 del Grupo 4: +4 `braze-types`, +1 `braze-events`, +1 `braze-permissions`), `cargo clippy --workspace --all-targets -- -D warnings` (limpio, tras corregir un lint `if_same_then_else` en `terminal_prompt.rs` fusionando la condición de `write_all`/`flush` con `||`), `cargo fmt --all --check` (limpio tras un `cargo fmt --all`).

**Verificación manual en vivo (2026-07-04)** — confirmada de punta a punta contra Ollama real (`qwen2.5:3b`, `BRAZE_SESSION_DIR` apuntando a un directorio temporal): primera corrida de `braze chat` pidiendo escribir un archivo fuera del cwd → prompt y/n apareció, se respondió "y", archivo escrito, rollout log confirmado con `permission_requested`/`permission_decided` (`allowed: true`) llevando la `key` `WriteFile` con la ruta resuelta. Segunda corrida con `braze chat --resume <mismo-id>` pidiendo la misma escritura → **cero apariciones** de "¿Permitir?" en la salida, el archivo se sobrescribió directamente; el rollout log de esa segunda corrida confirma la secuencia `tool_call_started → tool_call_completed` sin ningún `permission_requested`/`permission_decided` intermedio, es decir, `PermissionGuard::check` resolvió por completo desde la caché sembrada por `seed_remembered` sin siquiera invocar `prompt.confirm()`. Directorio temporal limpiado al terminar.

**Decisiones de diseño no completamente especificadas de antemano**: (1) `derive_permission_key` no recibe una `WorkdirAllowlist` (a diferencia del `remember_key` original, que resolvía `WriteFile`/`DeleteFile` contra el cwd vía `self.allowlist.resolve`) — como función libre invocable desde `TerminalConfirmationPrompt` (que no tiene cwd disponible), normaliza la ruta solo léxicamente (`WorkdirAllowlist::normalize_lexically`, ahora `pub(crate)`) sin unirla al directorio de trabajo. Esto solo afecta la forma exacta de la key para el caso ya-Irreversible de una ruta *relativa* que escapa el allowlist (p. ej. `../../etc/passwd`) — un edge case que no se ejercita en ningún test existente ni en la verificación manual (que siempre usó rutas absolutas), y que sigue produciendo una key estable y consistente aunque no cwd-aware. (2) `CliError` gana una variante nueva `Session(#[from] braze_session::SessionError)` (mismo patrón `#[from]` que ya usan `Config`/`Engine`) para poder propagar errores de `store.load` durante el replay temprano en `main.rs` sin introducir una variante de string genérica.

## Grupo G del roadmap de auditoría — observabilidad y calidad (2026-07-05)

Implementa los 4 ítems concretos de `docs/AUDITORIA-2026-07.md` § "Grupo G — Observabilidad y calidad": A9 (spans/logs por turno-ronda-tool-call), C9 (versionado del formato en disco), C10 (ventana táctica/umbral configurables) y C11 (cache de eventos en memoria). El quinto ítem del grupo ("tests de todos los edge cases A15/C16/D12/E7/F") queda fuera de este incremento — es una cobertura transversal sobre hallazgos de otras auditorías, no una pieza autocontenida de observabilidad.

**A9 — spans/logs por turno, ronda y tool-call** (`braze-engine::engine`):
- `Engine::run_turn` gana `#[tracing::instrument(name = "turn", skip(self, user_input, on_text), fields(session = %session))]` — equivalente seguro-para-async de `info_span!("turn", %session)`: todo log emitido dentro del turno (incluso varias llamadas anidadas adentro, como `load_messages`/`dispatch_tool_calls`) queda automáticamente taggeado con `session`, sin tener que pasarlo a mano a cada `tracing::warn!`/`debug!` existente. `user_input` se excluye explícitamente de la auto-captura de parámetros del macro (el comportamiento por defecto sin `skip` lo habría agregado como campo del span completo) — filtrar el texto del usuario en cada línea de log del turno no era la intención de A9 y es un riesgo de log verboso/sensible innecesario.
- El loop `for _ in 0..MAX_TURN_ITERATIONS` pasó a `for round in 0..MAX_TURN_ITERATIONS`, y gana `tracing::debug!(round, n_tool_calls = tool_calls.len(), "round completed")` una vez que `tool_calls` está resuelto (después del rescate de tool-calls emitidos como texto plano) — la ausencia total de esta señal era el gap concreto que A9 señalaba ("diagnosticar la secuencia patológica de A1-A5 con RUST_LOG=debug es imposible hoy").
- `load_messages` gana un `tracing::warn!(tactical_len, tactical_compaction_threshold, over_event_count_threshold, over_token_budget, "context compaction triggered")` justo antes de compactar — antes de este cambio la compactación no dejaba ningún rastro en logs, solo el `AgentEvent::CompactionOccurred` persistido silenciosamente.
- `dispatch_tool_calls` gana dos `tracing::debug!` para el camino normal/exitoso (antes solo los caminos de fallo —rechazo de schema, tool desconocida, timeout— logueaban algo): uno al despachar (`tool`, `id`) y uno al completar (`tool_call_id`, `is_error`).
- El `warn!` de agotamiento del cap de iteraciones (`attempt_final_summary_round`, ya existente desde antes de este incremento) no se tocó — al quedar ahora anidado bajo el span "turn", automáticamente hereda el campo `session` sin cambios de código.

**C9 — versionado forward-compatible del formato en disco** (`braze-events::event`):
- `AgentEvent` (enum internamente taggeado, `#[serde(tag = "type")]`, contrato congelado por PLAN.md) gana una variante unitaria `Unknown` con `#[serde(other)]` — el mecanismo propio de serde para enums taggeados: cualquier valor de `"type"` no reconocido por este binario deserializa a `Unknown` en vez de fallar, descartando el resto de los campos de esa línea (no hay nada útil que conservar de una forma que este binario no sabe interpretar). Antes de este cambio, un binario viejo leyendo un rollout log escrito por un binario más nuevo (con una variante que el viejo no conoce) hacía fallar `load()` completo de la sesión en esa línea, no solo el evento que no entendía.
- Es un cambio aditivo (una variante nueva, no una restructuración del tag) — respeta la restricción de "contrato congelado" documentada para `AgentEvent`.
- Downstream: `Unknown` se agregó al brazo catch-all (audit-only, nunca se renderiza como `Message`) de `braze_engine::history::event_to_message` y al brazo catch-all de `braze_session::SimpleContextCompactor::compact_tactical` — los únicos dos matches exhaustivos sobre `AgentEvent` sin comodín `_` en todo el workspace (confirmado por búsqueda exhaustiva de `match.*event` sobre todos los crates). El resto de los usos de `AgentEvent` en el workspace ya usaban `_`/`matches!`, así que compilaron sin cambios.

**C10 — ventana táctica y umbral de compactación configurables** (`braze-config`, `braze-engine`):
- `Config` gana `tactical_window: usize` (default 20, igual a `SimpleContextCompactor::DEFAULT_TACTICAL_WINDOW`) y `tactical_compaction_threshold: usize` (default 40, igual a `braze_engine::DEFAULT_TACTICAL_COMPACTION_THRESHOLD`) — mismo patrón exacto de plomería que `ollama_num_ctx` (campo en `Config`, override opcional en `ConfigOverrides`, parseo en `ConfigOverrides::from_env` bajo `BRAZE_TACTICAL_WINDOW`/`BRAZE_TACTICAL_COMPACTION_THRESHOLD`, sin flag de CLI — `ollama_num_ctx` tampoco expone uno).
- `Engine` gana `pub fn with_tactical_compaction_threshold(mut self, threshold: usize) -> Self`, builder chainable con la misma forma que `with_context_budget` ya existente.
- `braze-cli::main.rs` y `braze-bench::runner::run_task` dejaron de construir `SimpleContextCompactor::default()` a secas — ahora es `SimpleContextCompactor::new(config.tactical_window)` encadenado con `.with_tactical_compaction_threshold(config.tactical_compaction_threshold)` sobre el `Engine`, en ambos binarios (así un sweep de `braze-bench` mide el mismo comportamiento de ventana/umbral que tendría una invocación real de `braze` con ese config).

**C11 — cache de eventos en memoria** (`braze-session::file_store`):
- `FileSessionStore` gana `cache: Mutex<HashMap<SessionId, Vec<AgentEvent>>>`. `load()` sirve desde el cache si hay una entrada (sin tocar disco); si no, hace la lectura+parseo completo de siempre y **entonces** siembra el cache. `append()` solo extiende una entrada de cache ya "caliente" (poblada por un `load()` anterior en este mismo proceso) — si está "fría" (nunca cargada en este proceso), no la siembra, precisamente para no ocultar historia que ya exista en disco de un proceso anterior; el próximo `load()` de esa sesión hace la lectura completa (que ya incluye lo recién apendeado) y calienta el cache desde ahí. Sano únicamente por el supuesto de "single writer process" ya documentado en este mismo store — nada más puede invalidar el cache por debajo.
- Resuelve el patrón O(n²) descrito en C11: `Engine::load_messages` llama `store.load()` una vez por ronda dentro de un mismo turno (hasta `MAX_TURN_ITERATIONS` veces), y antes de este cambio cada llamada releía y reparseaba el archivo completo del disco.
- No se tocó la firma de `SessionStore` (contrato congelado) — el cache es enteramente interno a `FileSessionStore`, no un wrapper/decorator separado.

**Tests agregados**: `braze-session` pasó de 22 a 24 tests (`append_after_a_load_keeps_serving_from_cache_not_disk`: corrompe el archivo en disco después de calentar el cache y confirma que `load` sigue sirviendo el contenido correcto desde memoria en vez de fallar al releer; `append_before_any_load_does_not_hide_prior_disk_state_on_the_next_load`: un `append` antes de cualquier `load` no debe sembrar un cache parcial). `braze-events` gana 1 test (`unrecognized_type_tag_deserializes_as_unknown_instead_of_erroring`). `braze-config` gana 3 tests (`tactical_fields_are_overridable_via_env`, `from_env_rejects_invalid_tactical_window`, más la extensión de `defaults_without_file_or_env`/`from_env_parses_known_fields` existentes con los 2 campos nuevos).

**Verificación**: `cargo build --workspace`, `cargo test --workspace` (344 tests + 1 doctest, todos verdes), `cargo clippy --workspace --all-targets -- -D warnings` (limpio). Verificación manual en vivo contra `openrouter:deepseek/deepseek-v4-flash` real con `RUST_LOG=braze_engine=debug`: confirmado el span `turn{session=...}` envolviendo cada línea del turno, `round completed round=0 n_tool_calls=1` seguido de `dispatching tool call tool=read_file id=...` y `tool call completed tool_call_id=... is_error=false` en un turno con una tool call real, y `round completed round=0 n_tool_calls=0` en un turno sin tool calls — sin fugar el texto del usuario en ningún campo del span.

**Decisiones de diseño no completamente especificadas de antemano**: (1) A9 no agregó spans anidados por ronda/tool-call (una lectura literal de "spans por turno/ronda/tool-call" del resumen del Grupo G) — el detalle completo del hallazgo A9 en la auditoría pide explícitamente `info_span!("turn", %session)` (uno solo, por turno) más `debug!(round, n_tool_calls)`/`warn!` como líneas de log con campos, no spans anidados adicionales; anidar un span por ronda habría requerido extraer el cuerpo del loop a un método aparte (dado que mantener un `Span::enter()` a través de puntos `.await` en un runtime multi-hilo es el anti-patrón que la propia documentación de `tracing` advierte) — refactor de alcance mayor no justificado por lo que A9 realmente pedía. (2) El ítem 5 del grupo ("tests de edge cases A15/C16/D12/E7/F") se dejó fuera deliberadamente — cada hallazgo referenciado pertenece a un área temática distinta (permisos, MCP, robustez de red, etc.) sin relación directa con observabilidad, y closearlos de forma apropiada requeriría releer cada auditoría de área por separado en vez de ser una extensión natural de A9/C9/C10/C11.

## Archivos críticos

- `/home/franciscoparrao/proyectos/braze/Cargo.toml` — manifiesto de workspace
- `/home/franciscoparrao/proyectos/braze/crates/braze-tools-core/src/provider.rs` — trait `ToolProvider`, contrato congelado
- `/home/franciscoparrao/proyectos/braze/crates/braze-model/src/backend.rs` — trait `ModelBackend`
- `/home/franciscoparrao/proyectos/braze/crates/braze-session/src/store.rs` — `SessionStore` + `ContextCompactor`
- `/home/franciscoparrao/proyectos/braze/.github/workflows/ci.yml` — espejo de `/home/franciscoparrao/proyectos/datacube-rs/.github/workflows/ci.yml`

Referencia de patrón async (aunque ya no se usa como frontera aislada, dado que el workspace es 100% async — sigue siendo útil como ejemplo de manejo de runtime tokio compartido si algún crate necesitara exponer una API bloqueante hacia afuera en el futuro): `/home/franciscoparrao/proyectos/surtgis/crates/cloud/src/sync_api.rs`.
