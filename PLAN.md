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

`braze-sandbox-linux` (Landlock/seccomp), sandboxing multi-OS, `braze-otel` (export OTLP), `braze-skills` (paquetes de capacidades cargables), `braze-agent-graph` (multi-agente/grafo de threads), `braze-tui` (diseñado el 2026-07-05, ver § "Fase TUI — diseño" — MVP es CLI de línea), `braze-hooks` (sistema de hooks plugueable — los puntos de hook del MVP van hardcodeados en `braze-engine`).

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
- **Paso 3 (Anthropic real) — confirmado 2026-07-05**, tras cerrar el Grupo I
  de `docs/AUDITORIA-2026-07-v2.md`: `braze chat --backend anthropic --model
  claude-haiku-4-5-20251001` con API key real del usuario. La primera corrida
  (sesión multi-turno con 2 tool calls concurrentes y `tactical_window`
  angosto para forzar compactación) reprodujo un **400 real** de Anthropic
  (`tool_use ids were found without tool_result blocks immediately after`) —
  un hallazgo nuevo (N-2b: un par tool_use/tool_result puede quedar partido
  entre `durable_events` y `tactical` cuando la ventana cae justo entre
  ambos) que ningún test unitario con `ScriptedModel` podía atrapar por
  diseño. Corregido en `SimpleContextCompactor::split` y re-verificado: la
  misma sesión exacta corre limpia. Con esto, **los 6 pasos del checklist
  end-to-end quedan confirmados**; ver el detalle completo en
  `docs/AUDITORIA-2026-07-v2.md` § 7.

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

## Técnica G10 del roadmap SOTA — Best-of-n / Test-Time Scaling (2026-07-05)

Implementa la técnica G10 de `docs/AUDITORIA-2026-07.md` § "Grupo F — SOTA
nuevo": generar `N` candidatos independientes por ronda y votar cuál usar,
en vez de un solo intento greedy — cheap test-time scaling sin
entrenamiento, respaldado por evidencia de que modelos chicos mejoran
sustancialmente cuando se les permite intentar varias veces y votar
(Corradini et al. 2025, BDCC). Candidatos naturales del propio sweep del
proyecto: `error_recovery` y `distractor_selection`, ambos 0/5 para
`qwen2.5:3b`/`7b` (ver `CLAUDE.md`).

**Refactor previo en `braze-engine::engine`**: el cuerpo del loop de
rondas que consumía el stream de una sola llamada a `model.complete()` se
extrajo verbatim a un método privado nuevo, `Engine::complete_once` (toma
`req`, `observer`, y un flag `emit_deltas` que controla si los text
deltas se reenvían en vivo al observer). El `run_turn` original con
`best_of_n <= 1` (el default) llama a este método exactamente una vez —
mismo bytecode, cero cambio de comportamiento, confirmado porque los 35
tests preexistentes de `engine.rs` pasan sin tocarlos.

**`Engine::complete_with_best_of_n`** (nuevo, solo se ejecuta con
`best_of_n > 1`):
- Llama a `complete_once` `best_of_n` veces **secuencialmente** (no
  concurrente — cada llamada reutiliza `req.clone()`, por lo que
  `CompletionRequest` ganó `#[derive(Clone)]` en `braze-model`), con
  `emit_deltas: false` en cada una — no hay una sola "la" respuesta que
  mostrar en vivo hasta que la votación resuelve una.
- **Canonicaliza cada candidato** (`candidate_signature`) como el
  conjunto ordenado de pares (nombre de tool, argumentos canónicos) que
  pidió — reusando exactamente la misma canonicalización que
  `dispatch_tool_calls` ya usa para detectar llamadas repetidas
  (`arguments.to_string()`, estable porque `serde_json::Value` serializa
  claves ordenadas sin `preserve_order`) — o el conjunto vacío para un
  candidato de "respuesta final, sin tool call".
- **Vota por pluralidad**: el candidato cuya firma aparece más veces
  gana; empates se resuelven a favor del candidato generado **primero**
  (nunca `Iterator::max_by_key`, cuyo comportamiento de "el último gana"
  en empates haría el resultado depender de un detalle de
  implementación del loop de conteo, no del contenido).
- **El rescate de tool call textual (hallazgo B5) se aplica por
  candidato**, dentro de `complete_once`, no después de la votación — un
  candidato que describe la tool call como JSON en texto plano cuenta
  con esa firma rescatada para el voto, igual que en el camino
  single-call.
- **El uso de tokens persistido es la suma de todos los candidatos**, no
  solo el del ganador — `best_of_n` rondas hacen `best_of_n` llamadas
  reales al modelo, y el `AgentEvent::Usage` debe reflejar el costo
  agregado real o el accounting de costo/tokens subreporta
  silenciosamente por cada candidato descartado. `stop_reason` sí viene
  específicamente del candidato ganador (lo único que tiene sentido
  reportar de "por qué paró la respuesta que terminamos usando").
- **El texto del ganador llega al observer como un solo delta**, emitido
  después de la votación — un trade-off deliberado y documentado: no se
  puede mostrar en vivo, token a token, una respuesta cuyo "ganador" no
  se conoce hasta tener los `N` intentos completos. Esto degrada la
  sensación de streaming en vivo solo para las rondas con
  `best_of_n > 1`, sin requerir ningún cambio en `braze-cli` ni
  `braze-tui` (ambos ya consumen texto vía `on_text_delta`, sin importar
  si llega en un fragmento o en muchos).

**Config** (`braze-config`, mismo patrón exacto que `tactical_window`):
`Config::best_of_n: usize` (default `1`), `ConfigOverrides::best_of_n:
Option<usize>`, parseo bajo `BRAZE_BEST_OF_N` en `overrides.rs`.
`Engine::with_best_of_n(n)` builder chainable, wireado en
`braze-cli::main.rs` y `braze-bench::runner::run_task` (este último
comentado explícitamente como "mide el mismo comportamiento que una
invocación real" — mismo criterio que C10). `braze-bench` no necesita un
flag de CLI dedicado: como ya carga `Config::load()`, `BRAZE_BEST_OF_N`
fluye igual que cualquier otro campo de config.

**Alcance explícitamente NO tocado**: `attempt_final_summary_round` (la
ronda de degradación tras agotar `MAX_TURN_ITERATIONS`) no pasa por
best-of-n — es tools-free por diseño, y la técnica G10 aplica
específicamente "solo en la tool call" (título de la técnica en la
tabla de `docs/AUDITORIA-2026-07.md` § 6), no a la calidad general de la
prosa final.

**Tests agregados**: `braze-engine` pasó de 35 a 40
(`best_of_n_dispatches_the_majority_tool_call_signature`,
`best_of_n_breaks_ties_by_keeping_the_earliest_candidate`,
`best_of_n_sums_usage_across_candidates_and_keeps_the_winners_stop_reason`,
`best_of_n_set_to_zero_behaves_like_disabled_not_a_panic`,
`best_of_n_suppresses_live_deltas_but_delivers_the_winners_full_text_once`).
`braze-config` gana 2 (`best_of_n_is_overridable_via_env`,
`from_env_rejects_invalid_best_of_n`). `braze-cli` no gana tests nuevos
(el wiring es mecánico, mismo patrón ya cubierto por
`with_tactical_compaction_threshold`).

**Verificación**: `cargo build/test/clippy --workspace` verdes (380 → 388
tests). El mecanismo de voto se verificó correcto inspeccionando logs
`RUST_LOG=braze_engine=debug` en una corrida puntual: voto 2-1 real en
la ronda de la tool call (`grep`), voto 3-0 unánime en la ronda final de
texto, respuesta correcta — el pipeline no tiene bugs.

**Hallazgo empírico honesto — la técnica NO ayudó a `qwen2.5:3b` en
estos dos skills, y probablemente empeora las cosas**: sweep de
`braze-bench` (`crates/braze-bench/suites/g10-weak-skills.toml`,
`--repetitions 5`) comparando baseline (`best_of_n=1`) contra
`BRAZE_BEST_OF_N=3` sobre `error_recovery_wrong_filename` +
`distractor_selection_among_files` — datos crudos en
`docs/g10-baseline-n5.json`/`.log` y `docs/g10-bestof3-n5.json`/`.log`:

| | pass_rate | error_recovery | distractor_selection | avg tokens_in | timeouts |
|---|---|---|---|---|---|
| baseline (`best_of_n=1`) | 2/10 (±23pp) | 0/5 | 2/5 | ~1480 | 0 |
| `best_of_n=3` | 0/10 (±14pp) | 0/5 | 0/5 | ~3744 | **3/10** |

El costo en tokens se triplicó como se diseñó (correcto), pero el pass
rate empeoró en vez de mejorar, y aparecieron **3 timeouts** de 180s en
`distractor_selection` que el baseline nunca tuvo (3 llamadas
secuenciales a veces se acumulan más de lo esperado). Los intervalos de
confianza (Wilson) se solapan parcialmente, así que no es
estadísticamente aplastante — pero la dirección (peor, no mejor) más el
riesgo de timeout es una señal real, no ruido puro.

**Hipótesis de por qué, respaldada por el log de debug de arriba**: a
`temperature=0.2` (default de `OllamaBackend`, nunca tocado por esta
técnica — ver "alcance no tocado" más abajo) los `N` candidatos
probablemente convergen al mismo error de forma consistente en vez de
diversificar. Votar entre 3 respuestas casi idénticas no puede corregir
un error sistemático del modelo — solo agrega costo y, peor, riesgo de
timeout. La diversidad real que la técnica necesita para funcionar
(ver la evidencia de Corradini et al. citada en el diseño) probablemente
requiere una temperatura más alta específicamente para la generación de
candidatos — algo que esta implementación **deliberadamente no incluyó**
(ver diseño: `CompletionRequest` no ganó un campo `temperature` en este
incremento, para no tocar los 3 backends sin datos que lo justificaran
de antemano). Con esta evidencia, ese sería el primer refinamiento a
probar si se retoma la técnica — no antes.

**Decisión**: la implementación queda mergeada (`best_of_n` default `1`
= desactivado, cero riesgo para quien no lo configure explícitamente;
correcta y bien testeada a nivel unitario/mecanismo), pero **no se
recomienda activarla en producción con Ollama sin antes resolver la
diversidad de candidatos** (probablemente vía temperatura elevada
específica para la generación de candidatos). Documentado aquí en vez de
revertido: el código es correcto, la pregunta abierta es de eficacia
empírica bajo un ajuste que todavía no se probó, no un bug — y el propio
espíritu de "bench-as-regression-tool" del proyecto es justamente medir
antes de asumir, en vez de descartar una técnica de la literatura sin
haberla intentado.

## Fase TUI — diseño (2026-07-05)

Saca `braze-tui` de la lista de diferidos de Fase 2 y lo diseña como el
siguiente incremento mayor. Basado en la investigación comparativa de las
TUIs de Codex CLI (ratatui), Gemini CLI (fork de Ink) y opencode
(Bubbletea→OpenTUI) — ver `docs/TUI-INVESTIGACION-2026-07.md` para los
cuatro informes completos y las fuentes. **Aún no implementado.**

### Decisiones fundamentales (las tres convergencias de la investigación)

1. **Inline viewport + scrollback nativo, NO alternate screen.** La TUI
   ocupa solo N filas al fondo del terminal (celda activa + composer +
   status); el historial finalizado se escribe **una sola vez** al
   scrollback nativo vía `Terminal::insert_before` con la feature
   `scrolling-regions` de ratatui (imprescindible: sin ella,
   `insert_before` con streaming produce el flickering documentado en
   ratatui#584). El usuario conserva scroll/selección/copy nativos. Costo
   de render O(viewport), no O(historial). Los tres proyectos
   investigados convergieron en este patrón (Codex lo implementa a mano
   con escapes ANSI — esa es nuestra ruta de madurez si el
   resize/wrapping duele, no el punto de partida).
2. **Streaming committeado por fronteras semánticas, no por token.** Los
   deltas se acumulan en una celda activa mutable dentro del viewport;
   solo se "sellan" líneas completas al scrollback (commit newline-gated,
   patrón `MarkdownStreamCollector` de Codex). Nunca se redibuja por
   token: un `FrameRequester` con canal de capacidad 1 coalesce los
   redraws (los requests intermedios se dropean).
3. **La TUI es consumidor puro de eventos del engine.** Nunca llama al
   engine sincrónicamente ni mantiene estado agéntico propio — solo
   estado de UI. El corte ya existe en braze (`braze-engine` emite
   `AgentEvent` al store); lo único que falta es exponerlo **en vivo**
   (hoy solo se persiste), ver la costura `TurnObserver` abajo.

### Cambio previo en `braze-engine`: la costura `TurnObserver`

`Engine::run_turn` hoy expone `on_text: &mut dyn FnMut(&str)` — un canal
de deltas de texto y nada más. La TUI necesita el ciclo de vida completo
del turno en vivo (tool call despachada/completada, compactación, usage).
Generalización mínima:

```rust
// braze-engine (o braze-events): trait con defaults no-op
pub trait TurnObserver: Send {
    fn on_text_delta(&mut self, delta: &str) {}
    fn on_event(&mut self, event: &AgentEvent) {}   // espejo en vivo de cada append al store
}
```

- `run_turn` cambia `on_text: &mut dyn FnMut(&str)` →
  `observer: &mut dyn TurnObserver`. `on_event` se invoca en los mismos
  puntos donde hoy se hace `store.append(...)` (espejo, no reemplazo — la
  persistencia no cambia).
- **No toca ningún contrato congelado** (`ToolProvider`, `ModelBackend`,
  `SessionStore` quedan intactos; `AgentEvent` solo se *reusa* como
  vocabulario del observer, sin variantes nuevas). Sí rompe la firma
  pública de `run_turn` → actualizar `braze-cli` (adapter trivial que
  imprime deltas, `on_event` no-op) y `braze-bench` en el mismo commit.
- Alternativa considerada y descartada: decorator `TeeStore` sobre
  `SessionStore` que reenvíe appends por canal — cubre los eventos pero
  no los deltas de streaming (que no son `AgentEvent`), así que habría
  que mantener `on_text` igual; dos mecanismos a medias en vez de una
  costura explícita.

### Crate nuevo: `braze-tui` (nivel 4; `braze-cli` pasa a nivel 5)

Depende de: `engine`, `events`, `types`, `permissions`, `session`,
`config`. Deps externas: `ratatui` 0.30 (feature `scrolling-regions`),
`crossterm` (features `event-stream`, `bracketed-paste`), `tui-textarea`
(composer), `tui-markdown` (render markdown), `tokio`, `tracing`, `insta`
(dev, snapshot tests).

Estructura (archivo-por-módulo, convención del workspace):

- `app.rs` — loop `tokio::select!` sobre {`crossterm::EventStream`
  (teclado/paste/resize), `mpsc::Receiver` del `TurnObserver` del turno
  en curso, frame requests}. El turno corre en un `tokio::spawn` cuyo
  `JoinHandle` guarda la app (habilita interrupción, ver abajo).
- `history_cell.rs` — trait `HistoryCell` (render a `Vec<Line>` dado un
  ancho) + celdas tipadas: `UserCell`, `AssistantMarkdownCell`,
  `ToolCallCell` (máquina de estados: pending → running →
  success/error, glifo y color por estado, output truncado a N líneas),
  `PermissionCell`, `CompactionCell`. Las celdas guardan el **modelo**
  (texto fuente), no líneas pre-wrappeadas — el wrap se hace al ancho
  vigente en render.
- `chat_widget.rs` — transcript: celdas finalizadas → `insert_before`
  (batcheadas, flusheadas una vez por draw) + `active_cell` mutable en el
  viewport.
- `markdown_stream.rs` — colector de deltas con commit newline-gated:
  `commit_ready_lines()` devuelve solo las líneas completas nuevas; la
  cola parcial se re-renderiza en la celda activa. Los code blocks
  abiertos se retienen hasta cerrar (lección de Gemini:
  `findLastSafeSplitPoint` — nunca sellar dentro de un code block).
- `composer.rs` — `tui-textarea`: Enter envía, Ctrl+J newline (Shift+Enter
  vía Kitty keyboard protocol donde el terminal lo soporte —
  `PushKeyboardEnhancementFlags` de crossterm), Up/Down historial de
  input, Ctrl+C/Ctrl+D salida con doble-tap.
- `approval.rs` — `ChannelConfirmationPrompt`: implementa
  `braze_permissions::ConfirmationPrompt` enviando
  `(ActionDescriptor, oneshot::Sender<bool>)` por canal a la app y
  esperando la respuesta. La app lo renderiza como overlay en el bottom
  pane (reemplaza al composer mientras está pendiente, como
  `DialogManager` de Gemini): `y` permitir, `n`/`Esc` denegar. Reusa la
  persistencia `PermissionRequested`/`PermissionDecided` que ya hace
  `TerminalConfirmationPrompt` (extraer esa lógica compartida o
  duplicarla mínimamente — decidir en implementación). **El overlay
  muestra el nivel de riesgo y la razón de la clasificación** que
  `PermissionGuard`/el clasificador ya computan (Grupo 2), no solo el
  comando pelado — la literatura de confianza calibrada (ver anexo
  académico de `docs/TUI-INVESTIGACION-2026-07.md`) y la práctica de los
  tres proyectos de referencia coinciden en mostrar el porqué.
- `status_bar.rs` — línea inferior: backend/modelo, id de sesión, tokens
  acumulados (de `AgentEvent::Usage`), spinner cuando hay turno corriendo.
- `frame.rs` — `FrameRequester` (canal capacidad 1) + draw envuelto en
  synchronized update (BSU/ESU) de crossterm.

**Interrupción con Esc**: abortar el `JoinHandle` del turno. Es seguro
gracias a la reparación de tool calls colgantes que `braze-engine` ya
hace al cargar la sesión (el repair de `AssistantToolCall` sin
`ToolCallCompleted` existente desde el Grupo 3) — un turno abortado a
mitad de dispatch deja el log en un estado que el próximo turno ya sabe
reparar.

### Integración con `braze-cli`

`braze chat --tui` opt-in en este incremento; el chat plano actual queda
como default. Promover la TUI a default (con `--plain` como escape y
fallback automático cuando stdout no es TTY) se decide **después** de la
verificación manual, como incremento separado. `braze run` no cambia
(one-shot, sin TUI). `braze-bench` no cambia (usa el adapter no-op).

### Alcance del MVP de la fase

**Incluido**: viewport inline + `insert_before` de stock; streaming con
commit por líneas; las 5 celdas tipadas; composer multi-línea con
historial; approval overlay; interrupción con Esc; status bar; snapshot
tests `insta` de las celdas (render a buffer con ancho fijo — los dos
proyectos de referencia tratan los snapshot tests de UI como
imprescindibles).

**Incluido además, por el ángulo académico** (ver anexo OpenAlex en
`docs/TUI-INVESTIGACION-2026-07.md`):

- **Recibo de turno** (`TurnSummaryCell`): al cerrar cada turno, una
  celda compacta con qué archivos se tocaron y qué comandos se corrieron
  (derivable de los `ToolCallCompleted` del turno, sin costo de modelo).
  La evidencia (UIST 2024, CHI 2024 sobre carga metacognitiva) dice que
  el cuello de botella del usuario es *verificar*, no leer el stream.
- **Notificaciones de background no modales**: las completions de
  `TaskNotifier` se muestran como cambio de status/toast, nunca robando
  foco del composer — se materializan en fronteras seguras (estudio de
  campo 2026 sobre receptividad a IA proactiva).
- **Telemetría de UI vía `tracing`** (extensión natural del Grupo G):
  tiempo hasta primer token, duración del approval overlay abierto,
  turnos interrumpidos con Esc. Instrumenta los estados de interacción
  donde se va el tiempo del usuario (taxonomía CUPS, CHI 2024).

**Diferido (fase TUI 2)**: ~~slash commands con popup, @-menciones de
archivos~~ (implementados 2026-07-05, ver § "Fase TUI 2 — slash commands
+ @-menciones" más abajo), ~~temas configurables~~ (implementado
2026-07-05 como preset picker dark/light/high-contrast, ver § "Fase TUI
2 — temas configurables" más abajo), ~~pager overlay del transcript
completo (Ctrl+T)~~ (implementado 2026-07-05 con alcance reducido — una
celda que expande el output completo, no un overlay fullscreen real, ver
§ "Fase TUI 2 — Ctrl+T expande el output completo de una tool call" más
abajo), imágenes/clipboard, modo vim, inserción ANSI propia estilo Codex
(`custom_terminal`), soporte especial Zellij, TUI como default, y
**celda de plan/todo editable antes de ejecutar** (descomposición
interactiva de tareas — UIST 2024 — requiere soporte del engine para
planes, no solo de la TUI). ~~Backtrack~~ (Esc-Esc para retroceder a un
mensaje anterior y editarlo, como Codex) implementado 2026-07-05 — ver
§ "Fase TUI 2 — backtrack: Esc-Esc para retroceder y editar" más abajo.

### Riesgos conocidos (aceptados para el MVP)

- **Resize del viewport inline es el talón de Aquiles de ratatui**
  (ratatui#984/#2086): resize horizontal puede corromper el render y el
  texto ya insertado en el scrollback no re-wrappea. Aceptado — Codex
  resuelve esto con re-flow propio del transcript; queda para fase TUI 2
  si duele.
- **`tui-markdown` puede quedar corto** para streaming (Codex terminó
  escribiendo su markdown propio). Mitigación: `markdown_stream.rs` aísla
  el colector del renderer — cambiar el renderer no toca el resto.
- `insert_before` limitado a `u16::MAX` filas por inserción
  (ratatui#1426) — irrelevante en la práctica con commits incrementales
  por líneas.

### Orden de implementación (oleadas)

1. **COMPLETA (2026-07-05)** — Costura `TurnObserver` en `braze-engine` +
   adaptación de `braze-cli` y `braze-bench` — workspace verde sin TUI
   todavía. Implementación: el trait vive en `braze-events::observer`
   (nivel 0, junto a `AgentEvent`, que es su vocabulario), con
   `NoopObserver` (headless: bench, tests) y `TextDeltaObserver<F>`
   (adapter que preserva la forma del viejo `on_text` para el chat plano)
   incluidos. `Engine` ganó el helper privado `append_and_notify` (espejo
   *después* del append exitoso — un frontend nunca puede mostrar un
   evento que el rollout log no tiene) y el observer se threadea por
   `run_turn` → `dispatch_tool_calls` / `load_messages` /
   `repair_orphaned_tool_calls` / `attempt_final_summary_round`, cubriendo
   los 12 sitios de persistencia (incluidos compactación y reparación de
   huérfanos, que la TUI renderiza como celdas). Tests: 344 → 347 (2 de
   los adapters en `braze-events`; 1 en `braze-engine` —
   `the_observer_mirrors_every_persisted_event_in_order` — que verifica
   contra el log persistido que el espejo ve exactamente los mismos
   eventos, en el mismo orden, más los deltas). `cargo
   build/test/clippy --workspace` verdes.
2. **COMPLETA (2026-07-05)** — Esqueleto `braze-tui`: loop `select!`,
   viewport inline, composer, `UserCell` + texto plano del asistente
   streameando a la celda activa. Crate nuevo (nivel 4; `braze-cli` pasa
   a nivel 5), deps verificadas reales contra crates.io: `ratatui` 0.30.2
   (features `scrolling-regions` + `unstable-rendered-line-info` — esta
   segunda no estaba en el diseño original; `Paragraph::line_count`, que
   `commit_cell` usa para dimensionar cada `insert_before` al alto exacto
   en líneas envueltas, vive detrás de ese feature flag inestable, igual
   que en `codex-rs/tui`), `crossterm` 0.29 (`event-stream` +
   `bracketed-paste`), `ratatui-textarea` 0.9.2 (el fork de la org
   `ratatui`, no `tui-textarea` 0.7 del diseño original — mismo crate que
   `docs/TUI-INVESTIGACION-2026-07.md` ya señalaba como el mantenido).
   `tui-markdown` **no** se agregó todavía — no hace falta hasta la
   oleada 3 (markdown), agregar la dependencia antes sería inflar el
   crate sin uso.
   - **`braze-events::TurnObserver` en acción**: `ChannelObserver`
     (`observer.rs`) reenvía cada callback por un canal `mpsc` no acotado
     hacia el loop de la app — la costura de la oleada 1 resultó ser
     exactamente la interfaz necesaria, sin cambios.
   - **Patrón de scrollback simplificado respecto al diseño original**:
     en vez de mantener una `active_cell` que crece sin límite mientras
     el mensaje del asistente streamea (lo que exigiría un viewport de
     alto variable — el punto débil de ratatui, ver riesgos), el texto
     se commitea línea por línea al scrollback apenas cada línea se
     completa (`drain_ready_lines`, gateado por `\n`), dejando solo la
     última línea parcial en un área fija de `ACTIVE_ROWS = 2` filas con
     scroll automático al fondo. El viewport total queda fijo en 5 filas
     (2 preview + 1 hint + 2 composer) sin importar cuán larga sea la
     respuesta — evita el problema de resize dinámico del viewport por
     completo, a costa de diferir a la oleada 3 el gateo *semántico*
     (nunca sellar dentro de un code block markdown).
   - **Aprobación de permisos**: en vez de la `ChannelConfirmationPrompt`
     completa (oleada 4), este incremento usa
     `AutoDenyConfirmationPrompt` — deniega toda acción irreversible bajo
     `--tui`, documentado como stopgap seguro (no se puede usar
     `TerminalConfirmationPrompt`'s lectura de stdin con la terminal en
     raw mode: sin edición de línea canónica, Enter manda `\r` no `\n`).
     `braze-cli::build_permission_guard` gana un parámetro `tui_mode`
     que selecciona el prompt correcto antes de construir cada
     `ToolProvider`.
   - **Integración**: `braze chat --tui` (nuevo flag en `cli_args.rs`),
     dispatcha a `braze_tui::run(engine, session)` en vez del loop
     stdin/stdout; `braze run` y el `chat` sin `--tui` no cambian.
   - **Tests**: 347 → 356 (9 nuevos en `braze-tui`: 3 de
     `drain_ready_lines`, 3 de `HistoryCell`, 2 de `ChannelObserver`, 1 de
     `AutoDenyConfirmationPrompt`). `cargo build/test/clippy --workspace`
     verdes.
   - **Verificación manual en vivo**: contra Ollama local
     (`qwen2.5:3b`, ya recomendado en `CLAUDE.md` por soportar
     tool-calling nativo) usando un harness propio en Python
     (`pty.openpty()` + `pyte` para reconstruir la pantalla real, ya que
     crossterm consulta la posición del cursor con `ESC[6n` al iniciar
     el viewport inline — un pty desnudo nunca responde esa consulta sin
     un emulador de verdad detrás). Confirmado en una sesión real:
     tipeo multi-tecla en el composer, commit del `UserCell` al
     scrollback nativo con el marcador `> `, cambio del hint a "esperando
     respuesta...", el modelo real invocando una tool (intentó leer un
     archivo inexistente) sin que su evento se dibuje (esperado, oleada
     3/4), el texto de la respuesta del modelo streameando y
     commiteándose al scrollback en vivo, y salida limpia con Ctrl+C
     (raw mode restaurado, proceso terminó con exit code 0 incluso con
     un turno todavía en vuelo — el log de sesión quedó en un estado
     válido y resumible, sin tool call huérfano sin resultado).
     **Hallazgo cosmético, re-investigado y explicado en la oleada 3**:
     la línea de hint mostró una vez texto corrupto ("esphoando" en vez
     de "esperando"). Con el análisis byte-a-byte hecho en la oleada 3
     (ver más abajo) quedó claro que esto **no es un bug de braze-tui**:
     es una limitación conocida del harness de verificación (`pyte` no
     emula perfectamente `scroll_region_up`/`scroll_region_down`/DECSTBM
     bajo la feature `scrolling-regions` de ratatui, que se usa tanto
     para `insert_before` como para el diffing normal de `draw()` cuando
     detecta contenido que se desplazó dentro del mismo viewport). El
     stream de bytes real es correcto (confirmado inspeccionando el
     stream ANSI crudo directamente, sin pasar por `pyte`); un terminal
     real (xterm/kitty/gnome-terminal, con soporte VT100 completo)
     renderiza esto sin problema. No hay acción pendiente sobre esto.
3. **COMPLETA (2026-07-05)** — `markdown_stream.rs` + `AssistantMarkdownCell`
   + `ToolCallCell` con estados.
   - **`markdown_stream.rs`**: `MarkdownStreamCollector` adapta el
     `findLastSafeSplitPoint` de Gemini (gateo por párrafo/`\n\n`) a
     gateo por **línea** — igual que `drain_ready_lines` de la oleada 2
     — pero con conciencia de fences: una línea que abre un bloque
     ` ``` ` retiene todo lo que sigue (aunque sean líneas completas)
     hasta que llega el cierre, momento en el que **todo el fence se
     commitea de una sola vez** como un chunk atómico. La razón: cada
     commit se renderiza independientemente vía
     `tui_markdown::from_str`, sin memoria del commit anterior — partir
     un fence en dos renderizaría su mitad de cierre como texto plano,
     no como código. Solo reconoce ` ``` ` (no `~~~`), simplificación
     consistente con otras heurísticas del codebase
     (`engine::try_parse_textual_tool_call`). Invariante clave (con test
     de propiedad dedicado, chunking byte-a-byte adversarial): la
     concatenación de todo lo que devuelve `commit_ready()` más
     `finish()` reconstruye el texto original exacto sin importar cómo
     se fragmentaron los `push()`.
   - **Vista previa (`ACTIVE_ROWS`) sigue en texto plano**, deliberadamente
     no renderizada como markdown — solo los chunks ya commiteados
     (estables, terminados) pasan por `tui-markdown`. Renderizar
     markdown parcial/inestable en vivo es un problema bastante más
     grande que el gateo de fronteras de commit y quedó fuera de
     alcance.
   - **`HistoryCell::as_text` cambió de `Text<'static>` a `Text<'_>`**
     (préstamo desde `&self`) — necesario porque
     `tui_markdown::from_str(&str) -> Text<'_>` toma prestado del
     markdown de entrada; las demás celdas (que ya construían `Text`
     propio) siguen compilando sin cambios por varianza de lifetime.
   - **`ToolCallCell`**: no es una celda mutable que se actualiza in
     situ — los commits al scrollback son append-only (como el
     scrollback real de una terminal), así que una tool call que
     arranca y termina produce **dos líneas separadas** en el
     transcript (una en `ToolCallStarted`, otra en `ToolCallCompleted`),
     ambas usando el mismo tipo de celda con distinto estado
     (`Running`/`Done{is_error, summary}`). Una llamada rechazada antes
     de ejecutar (schema inválido, tool desconocida, duplicado) nunca
     emite `ToolCallStarted` — salta directo a `Done{is_error: true}`,
     así que no aparece una línea "running" engañosa para algo que
     nunca corrió. `ToolCallCompleted` no trae el nombre de la tool
     (solo `id`) — se correlaciona con un `HashMap<String, String>`
     poblado en `AssistantToolCall` (el único evento con `id` + `name`
     que siempre precede tanto a `ToolCallStarted` como a un rechazo
     directo). El output se trunca a la primera línea + 80 caracteres +
     conteo de líneas restantes (mismo criterio que
     `codex-rs/tui`) — el contenido completo sigue en el rollout log,
     solo no hay pager overlay todavía para verlo (fase TUI 2).
   - **Tests**: 356 → 364 (8 nuevos: 6 en `markdown_stream` incluyendo
     el test de propiedad de reconstrucción, 2 más en `history_cell`
     para `AssistantMarkdownCell`/`ToolCallCell`/`summarize_tool_output`
     — en total `history_cell` pasó de 3 a 8 tests).
     `cargo build/test/clippy --workspace` verdes.
   - **Verificación manual en vivo** contra Ollama (`qwen2.5:3b`) con el
     mismo harness pty+pyte de la oleada 2, más un tercer método:
     inspección directa del stream ANSI crudo (sin pasar por `pyte`)
     para un caso con bloque de código. Confirmado a nivel de bytes: el
     fence se retiene completo (las líneas dentro de él no aparecen
     sueltas en el transcript) y se commitea atómico una vez cerrado;
     la vista previa muestra el markdown crudo sin renderizar mientras
     está pendiente, exactamente como se diseñó. Un tool call real
     (`read_file`) generó las dos líneas esperadas (running→done). Nota
     de proceso: `qwen2.5:3b` corre 100% CPU en esta máquina (`ollama
     ps`), con load average ~6 — las respuestas con tool calls pueden
     tardar 30-90s reales; los primeros intentos de verificación
     asumieron un cuelgue antes de confirmar que solo era latencia de
     inferencia CPU-bound.
4. **COMPLETA (2026-07-05)** — `approval.rs` (`ChannelConfirmationPrompt`
   reemplazando `AutoDenyConfirmationPrompt`) + interrupción con Esc +
   `status_bar.rs`.
   - **Corrección honesta al diseño original**: la idea de "mostrar el
     nivel de riesgo y la razón de la clasificación" no mapea a datos
     reales — `Reversibility` (`braze-permissions::classifier`) es
     binario, y por construcción `ConfirmationPrompt::confirm` **solo**
     se invoca para acciones ya clasificadas `Irreversible` (las
     `Reversible` nunca preguntan) — mostrar "nivel de riesgo:
     irreversible" sería tautológico, y el clasificador no expone una
     cadena de "razón" en absoluto (solo el enum binario). En vez de
     forzar ese dato inexistente, el overlay muestra la descripción
     concreta de la acción (`ActionDescriptor::Display`, ya rica: `run
     \`mv /tmp/a /tmp/b\`` / `write file /etc/passwd`) más un texto de
     encuadre explícito ("esta acción puede ser irreversible") — mismo
     principio de "mostrar el porqué" del anexo académico, con los datos
     que realmente existen.
   - **`ChannelConfirmationPrompt`** (`approval.rs`) espeja casi
     exactamente a `TerminalConfirmationPrompt` de `braze-cli` (misma
     persistencia `PermissionRequested`/`PermissionDecided` para replay
     de `--resume`), pero pregunta por un canal (`ApprovalRequest { description,
     respond: oneshot::Sender<bool> }`) en vez de bloquear en stdin.
     `AutoDenyConfirmationPrompt` de la oleada 2 se eliminó por completo
     (su único propósito era ser el stopgap hasta este punto).
   - **Cola de aprobaciones** (`pending_approvals: VecDeque`, no un
     `Option`): dos tool calls despachadas concurrentemente en la misma
     ronda pueden pedir confirmación a la vez — solo se muestra la
     primera de la cola; responderla revela la siguiente.
   - **Dos condiciones de carrera reales encontradas y corregidas
     durante el diseño** (no en verificación posterior — se detectaron
     leyendo el propio código antes de compilar):
     1. *Canal de turno reusado entre turnos*: si `update_tx`/`update_rx`
        vivieran toda la vida de la app, un mensaje residual de un turno
        recién abortado (`interrupt_turn`) podría llegar DESPUÉS de que
        el usuario ya envió un turno nuevo, corrompiendo el
        `MarkdownStreamCollector` recién reseteado. Arreglo: `submit`
        reemplaza `update_tx`/`update_rx` por un par fresco cada turno
        — el sender del turno anterior queda huérfano (su receiver ya
        no existe), cualquier envío tardío falla en silencio sin tocar
        el estado del turno nuevo.
     2. *Aprobaciones huérfanas al interrumpir*: abortar el `JoinHandle`
        del turno principal (`run_turn`) **no cancela** las tareas de
        fondo que despachan tool calls (spawneadas aparte vía
        `TaskNotifier`) — una de ellas podría seguir bloqueada en
        `confirm()` esperando una respuesta que ya está en la cola.
        Arreglo: `interrupt_turn` vacía `pending_approvals` denegando
        cada una (`respond.send(false)`, el default de seguridad ya
        establecido en el codebase) en vez de dejar un prompt fantasma
        que podría resurgir confuso en el turno siguiente.
   - **Esc tiene significado dependiente del contexto**: si hay una
     aprobación pendiente, deniega (mismo atajo que Gemini/Codex); si
     no, e interrumpe el turno en curso — Ctrl+C siempre sale
     incondicionalmente, sin importar el estado (vía de escape
     universal).
   - **`interrupt_turn`**: aborta el `JoinHandle`, hace *flush* de lo que
     quedó sin commitear en `MarkdownStreamCollector` (el usuario pidió
     parar la generación, no des-ver lo que ya se mostró) y commitea una
     `NoticeCell` (amarilla, "⏸ interrupted by user" — deliberadamente
     *no* `ErrorCell`, ya que una interrupción pedida por el usuario no
     es una falla). Seguro porque `Engine::repair_orphaned_tool_calls`
     ya repara cualquier `AssistantToolCall` colgante la próxima vez que
     la sesión carga (mismo mecanismo que cubre un proceso matado/caído
     — nada nuevo que persistir desde el lado de la TUI).
   - **`status_bar.rs`**: función pura de formato (backend:modelo ·
     prefijo de session id de 8 caracteres · tokens acumulados
     `↑`/`↓` desde `AgentEvent::Usage`), renderizada en la mitad
     derecha de la fila de hint (no una fila nueva — `VIEWPORT_HEIGHT`
     no creció desde la oleada 2). `braze-cli` calcula el string
     "backend:modelo" antes de construir el `Engine` (que no expone el
     nombre del modelo del `ModelBackend` que recibió) y se lo pasa a
     `braze_tui::run` como parámetro.
   - **Nuevas celdas**: `PermissionCell` (✓/✗ + descripción, commiteada
     al responder una aprobación — el registro de auditoría de qué se
     permitió/denegó vive en el transcript, no solo en el overlay
     efímero que preguntó) y `NoticeCell` (nota neutra, reutilizable
     para futuras notificaciones no-error).
   - **Tests**: 364 → 371. `approval` pasó de 1 a 3 (el único test de
     `AutoDenyConfirmationPrompt` se reemplazó por 3 sobre
     `ChannelConfirmationPrompt`: responder que sí persiste ambos
     eventos, receiver caído deniega, respond-sender dropeado sin
     responder deniega). `status_bar` es módulo nuevo con 2 tests.
     `history_cell` pasó de 8 a 11 (`PermissionCell`×2, `NoticeCell`×1).
     `cargo build/test/clippy --workspace` verdes.
   - **Verificación manual en vivo, dos métodos**: (a) contra Ollama
     real (`qwen2.5:3b`) para interrupción con Esc — confirmado en
     pantalla: la celda "⏸ interrupted by user" aparece, el hint vuelve
     a la normalidad, y un turno posterior funciona sin problemas
     (sesión no corrupta tras interrumpir). (b) contra un **binario de
     prueba con un `ModelBackend` scripteado** (determinístico, sin la
     latencia real de Ollama de 30-90s que había hecho lento verificar
     el flujo de aprobación) que fuerza una tool call de `shell_exec`
     con `mv` — confirmado end-to-end en ambas ramas: denegar (`n`)
     produce `PermissionCell(✗ denied)` + `ToolCallCell(✗, "action
     denied: ...")`, y el `mv` real **nunca se ejecuta**; permitir (`y`)
     produce `PermissionCell(✓ allowed)` y el comando `mv` **se ejecuta
     de verdad** (falló con "no such file" porque la ruta de prueba no
     existía — confirma que el permiso se aplica de verdad, no es
     cosmético). Status bar visible y correcto en ambas verificaciones
     (`scripted:test · <8 chars> · tokens 0↑/0↓`).
5. **COMPLETA (2026-07-05)** — Snapshot tests `insta` + verificación de
   resize. La fase TUI planeada (oleadas 1-5) queda **completa**; lo que
   sigue son incrementos posteriores explícitamente fuera de esta fase
   (ver "Diferido (fase TUI 2)" más arriba, y la decisión pendiente de
   promover `--tui` a default).
   - **`insta` 1.48** (dev-dependency de `braze-tui`): 9 snapshots, uno
     por variante visual de celda (`UserCell` multilínea,
     `AssistantMarkdownCell` con heading+lista+bloque de código,
     `ToolCallCell` en sus 3 estados, `PermissionCell` en sus 2 estados,
     `ErrorCell`, `NoticeCell`), renderizadas a un `Buffer` de ancho fijo
     (40 columnas, igual que `commit_cell` en `app.rs`) y snapshoteadas
     vía el `Debug` propio de `Buffer` — formato idiomático de ratatui:
     muestra el contenido fila por fila **y** cada tramo de estilo
     (fg/bg/modifier), así que un cambio que rompa el wrapping o pierda
     un color se detecta igual que uno que cambie el texto. Snapshot del
     bloque de código confirma syntax highlighting real (colores RGB
     por token) y el heading markdown (`# Titulo`) confirma que
     `tui-markdown` aplica su estilo de h1 (negrita+subrayado+fondo
     cyan) — no solo que el texto pasa, que el *render* es el esperado.
     `.gitignore` ganó `*.snap.new` (un snapshot pendiente sin revisar
     nunca debe llegar al repo).
   - **Verificación de resize**: contra el mismo binario scripteado de
     la oleada 4 (determinístico — no depende de la latencia de un LLM
     real para esto), se achicó la ventana del pty de 100×30 a 60×20 a
     mitad de un overlay de aprobación pendiente, y se confirmó que la
     app **sigue viva y responde correctamente** al siguiente input
     (responder la aprobación, ver el resultado de la tool call, ver el
     texto de seguimiento del modelo) — sin crash, sin cuelgue, sin
     necesitar reiniciar. El primer frame inmediatamente posterior al
     resize salió en blanco en la captura (posible artefacto del
     harness pyte reconstruyendo mal la secuencia de limpieza+redibujo
     tras el `Resize` event, no confirmado como bug real — mismo tipo de
     limitación de harness ya documentado en la oleada 3). Esto valida
     exactamente el riesgo tal como estaba enmarcado desde la oleada 2:
     "resize no rompe la app" es lo que importaba verificar; la
     fidelidad visual perfecta del re-flow del scrollback ya
     commiteado sigue como riesgo aceptado, diferido a si molesta en la
     práctica.
   - **OpenRouter no se verificó en vivo por separado**: decisión
     deliberada, no un olvido — `braze-tui` no conoce el backend en
     absoluto (`Engine` es opaco a qué `ModelBackend` recibió), y el
     backend-agnosticismo ya está cubierto extensivamente en la capa de
     `braze-model` (66 tests) y ya se ejercitó en esta misma fase con
     dos backends reales distintos (Ollama en oleadas 2-4, más el
     `ModelBackend` scripteado de la oleada 4). Repetir con OpenRouter
     solo confirmaría lo mismo con una tercera implementación del mismo
     trait ya probado.

## Fase TUI 2 — slash commands + @-menciones (2026-07-05)

Primer incremento del backlog "Diferido (fase TUI 2)" — el resto (temas,
pager overlay, imágenes/clipboard, modo vim, backtrack, celda de
plan/todo editable, inserción ANSI propia, promover `--tui` a default)
sigue diferido, sin oleadas definidas todavía.

**`composer_trigger.rs`** (nuevo, pura y testeable sin `TextArea` real):
`detect_trigger(line, col, is_first_line)` — dado la línea actual del
composer y la posición del cursor (ambos character-indexed, como expone
`TextArea::cursor()`), busca hacia atrás el token delimitado por
espacios en blanco inmediatamente detrás del cursor y lo clasifica:

- `/comando` — **solo se reconoce como el primer token de todo el
  mensaje** (fila 0, empezando en columna 0) — nunca a mitad de frase,
  igual que en Codex/Gemini.
- `@mención` — se reconoce en cualquier parte del texto (a diferencia de
  `/`, una referencia a un archivo puede aparecer a mitad de una frase).

**`slash_commands.rs`**: registro estático `SLASH_COMMANDS` (`help`,
`quit`, `exit` — este último alias de `quit`, igual que el loop de chat
plano acepta ambas palabras sueltas). `matching_commands(query)` hace
*prefix match* case-insensitive (no substring — con nombres de comando
cortos, "lo que vas a escribir a continuación" es más útil que "aparece
en cualquier parte del nombre").

**`mentions.rs`**: `list_files(root)` camina el cwd recursivamente,
podando un puñado de directorios pesados hardcodeados (`.git`, `target`,
`node_modules`, `.venv`, `__pycache__`, `dist`, `build`) — **no**
parseo de `.gitignore` (para eso existe el crate `ignore`; de más para
un MVP), documentado como simplificación aceptada, no bug. Tope duro de
5000 archivos por si un árbol patológico (o un symlink cycle que la
lista de exclusión no atrapa) se cuela. `matching_files(files, query)`
es *substring* case-insensitive (a diferencia de los comandos, una ruta
es lo bastante larga como para que "contiene el fragmento tipeado en
cualquier parte" — p.ej. tipear "main" para encontrar
`crates/braze-cli/src/main.rs` — sea mucho más útil que exigir prefijo
contra la ruta completa).

**El popup reusa el espacio de la vista previa de streaming, no crece el
viewport**: ratatui no tiene una API pública para cambiar la altura de
un viewport inline en tiempo de ejecución fuera de responder a un
`Resize` real del terminal (`Terminal::resize` para `Viewport::Inline`
recalcula la posición usando la altura ORIGINAL fijada en
`Terminal::with_options`, no permite pedir una nueva altura) — crecer
dinámicamente el viewport cada vez que se abre/cierra un popup habría
tocado exactamente el punto frágil que la oleada 2 ya documentó como
riesgo aceptado. En cambio: `refresh_popup` nunca deja un popup abierto
mientras `turn_running` es verdadero, así que el área de vista previa
(`ACTIVE_ROWS` + fila de hint, 3 filas) está garantizado ociosa cada vez
que un popup podría estar activo — `draw` simplemente dibuja el popup
ahí en vez de la vista previa+hint+status bar, sin tocar
`VIEWPORT_HEIGHT` en absoluto. Costo: como consecuencia, **los popups
solo pueden abrirse cuando no hay un turno corriendo** — tipear `/` o
`@` mientras el modelo streamea inserta el carácter literal, sin popup
(el usuario puede seguir encolando su próximo mensaje mientras tanto,
simplemente sin autocompletado hasta que el turno termine). Aceptado
como limitación razonable: el uso típico de comandos/menciones ocurre
al empezar un mensaje nuevo, no a mitad de un streaming.

**Solo 3 sugerencias visibles a la vez** (`POPUP_MAX_VISIBLE`), sin
scroll dentro del popup — tipear otro carácter para acotar la búsqueda
en vez de navegar una lista larga. `Up`/`Down` navegan entre las
visibles, `Tab`/`Enter` aceptan (autocompletan el texto, **no**
ejecutan/envían todavía — aceptar `/help` deja `"/help "` en el
composer, igual que autocompletar cualquier palabra; un Enter aparte,
ya con el popup cerrado, es lo que efectivamente ejecuta/envía), `Esc`
cierra sin aceptar.

**Los comandos slash se manejan enteramente del lado del cliente**: en
`submit`, si el texto (trimeado) es exactamente `/nombre` para un
comando reconocido, `run_slash_command` se ejecuta y la llamada
**nunca** llega a `Engine::run_turn` — el engine no tiene ni necesita
noción de comandos slash. Un mensaje que solo *empieza* con `/` pero no
matchea un nombre exacto se envía al modelo como texto normal (mismo
comportamiento que Codex/Gemini para comandos no reconocidos).
`/help` commitea una `HelpCell` nueva (estática, lista atajos +
comandos + hint de menciones) al scrollback; `/quit`/`/exit` salen
igual que Ctrl+C.

**Tests**: 388 → 407 (19 nuevos: 8 en `composer_trigger` incluyendo el
caso "un `/` que no está al inicio del mensaje no es un comando" y "un
token ya terminado con espacio no es un trigger activo", 5 en
`slash_commands`, 4 en `mentions` incluyendo la exclusión de
`.git`/`target`, 2 en `history_cell` para `HelpCell` — uno de contenido,
uno de snapshot `insta`). `cargo build/test/clippy --workspace` verdes.

**Verificación manual en vivo** contra el binario de prueba con
`ModelBackend` scripteado de la oleada 4 (mismo patrón — determinístico,
sin depender de la latencia de un LLM real): tipear `/h` mostró el popup
filtrado a solo `/help` (no `/quit`/`/exit`); Tab autocompletó a
`"/help "`; Enter commiteó el contenido de ayuda al scrollback **sin**
disparar un turno del modelo (confirmando la intercepción client-side).
Tipear `revisa @main` sobre un sandbox con `main.rs`/`src/lib.rs`/
`README.md` mostró el popup con `@main.rs` (único match real); Tab
completó a `"revisa @main.rs "`, preservando el resto del texto ya
escrito. Salida limpia con Ctrl+C en ambos casos.

## Fase TUI 2 — Ctrl+T expande el output completo de una tool call (2026-07-05)

Resuelve el "pager overlay del transcript completo (Ctrl+T)" del
backlog original — **con un cambio de alcance deliberado, decidido
antes de implementar**: no un overlay fullscreen real, sino commitear
una celda nueva al scrollback con el contenido completo. Motivo: un
pager fullscreen requeriría un segundo `Terminal` de ratatui en modo
`Fullscreen` alternando con el viewport inline actual (ratatui no
expone forma de decirle a un `Terminal` ya construido "usá toda la
pantalla ahora" — solo se fija el `Viewport` al construirlo), sería el
primer código de este proyecto manejando dos `Terminal`s a la vez, y
resuelve el mismo dolor real (ver el output completo de una tool call,
hoy truncado a ~80 caracteres en `ToolCallCell`) con una fracción del
riesgo arquitectónico.

**`ExpandedToolOutputCell`** (`history_cell.rs`): nueva celda, con un
tope de 200 líneas (`EXPANDED_TOOL_OUTPUT_MAX_LINES`, mucho más generoso
que el resumen de 80 caracteres de `ToolCallCell` — esta *es* la vista
"dame todo" — pero no ilimitado: un output extremo volcado sin límite
sería inmanejable de scrollear, y arriesga el techo de `u16::MAX` filas
de `insert_before` documentado en ratatui#1426) con una nota si se
trunca ahí.

**`app.rs::expand_last_tool_call`** (Ctrl+T, ahora global — funciona en
cualquier estado, incluso con un popup o una aprobación pendiente, ya
que es de solo lectura y nunca muta nada): lee **fresco desde el
`SessionStore`** (no desde ningún cache del lado de la TUI) los eventos
de la sesión, busca el `ToolCallCompleted` más reciente, correlaciona su
`id` contra el `AssistantToolCall` correspondiente para recuperar el
nombre de la tool (mismo patrón de correlación que `apply_update` ya usa
para los `ToolCallCell` en vivo, solo que sobre el log completo en vez
del mapa en memoria), y commitea la celda expandida. Si todavía no se
completó ninguna tool call en la sesión, o si el store falla al leer,
commitea una `NoticeCell` en vez de fallar en silencio.

**`braze_tui::run` y `App` ganan un parámetro `store:
Arc<dyn SessionStore>`** — el mismo handle con el que se construyó
`Engine`, pasado por separado (no hay accessor público en `Engine` para
recuperarlo) porque `Engine` no expone ninguna forma de leer su propio
store hacia atrás. Mismo patrón que `ChannelConfirmationPrompt` ya usa
(recibe su propio `Arc<dyn SessionStore>` directamente desde
`braze-cli::main.rs`, no a través de `Engine`) — consistente, no un
mecanismo nuevo). `on_key` pasó de método sync a `async fn` — el único
cambio de arquitectura que este incremento exige, para poder hacer
`store.load(...).await` directo dentro del manejo de la tecla en vez de
spawnear una tarea aparte y comunicar el resultado por canal (el
mecanismo que sí hace falta para `run_turn`, que puede tardar segundos o
minutos reales). Una lectura de `FileSessionStore` es rápida — una
pausa breve del loop principal mientras se resuelve es aceptable, a
diferencia de bloquear en algo de duración real.

**Tests**: 407 → 411 (4 nuevos en `history_cell` para
`ExpandedToolOutputCell`: contenido completo sin truncar, truncamiento
con nota pasado el tope de líneas, glifo de error, más el snapshot
`insta`). `cargo build/test/clippy --workspace` verdes.

**Verificación manual en vivo** contra el binario de prueba con
`ModelBackend` scripteado, esta vez pidiendo `read_file` sobre un
archivo de 5 líneas (cada una >80 caracteres): confirmado que
`ToolCallCell` mostró el resumen truncado de siempre ("...deberia
truncarse e… (+N more lines)"), y que Ctrl+T commiteó
`"✓ read_file (completo)"` seguido de las **5 líneas completas sin
truncar** — el contenido íntegro llega correctamente desde el rollout
log. Salida limpia con Ctrl+C.

## Fase TUI 2 — temas configurables (2026-07-05)

**Hallazgo previo a implementar, que cambió el marco del problema**:
todos los colores que usa `braze-tui` ya son slots ANSI nombrados
(`Color::Red/Green/Yellow/DarkGray`), nunca RGB literal — confirmado con
`grep -rn "Color::Rgb\|Color::Indexed"` sin resultados. Eso significa
que el problema que originalmente motivó "temas configurables" en
Codex/Gemini (colores hardcodeados en RGB que chocan con el fondo del
terminal) **no aplica acá**: el terminal del usuario ya decide el RGB
real de cada color vía su propio esquema. Lo que un `Theme` sí aporta:
dejar elegir un mapeo semántico distinto (p.ej. sacar el warning de
amarillo puro, que se lee mal en fondo claro en terminales que no lo
remapean) o bajar a una paleta de alto contraste — no "arreglar" un
choque de RGB que no existe. Se lo planteó explícitamente al usuario
antes de implementar (junto con la alternativa de soportar `NO_COLOR`,
de costo mucho menor) — el usuario eligió seguir con el preset picker
completo pese al costo mecánico mayor.

**`theme.rs`** (nuevo): `struct Theme { success, error, warning, muted:
Color }`, `Copy`. Tres presets: `dark()` (el default, colores idénticos
a los que ya había hardcodeados — verificado porque los snapshots
`insta` existentes no cambiaron ni un byte al migrar), `light()`
(warning: Magenta en vez de Yellow — amarillo puro sobre fondo claro es
el clásico combo de bajo contraste en terminales que no lo remapean),
`high_contrast()` (muted: White en vez de DarkGray — un tono
deliberadamente "apagado" contradice el propósito de un modo de alto
contraste). `Theme::from_name(&str) -> Option<Theme>` para el mapeo
nombre→preset.

**Threading**: en vez de cambiar la firma del trait `HistoryCell::as_text(&self)`
(que habría tocado cada test existente que llama `.as_text()`
directamente), cada celda que usa color (`ToolCallCell`, `ErrorCell`,
`PermissionCell`, `NoticeCell`, `ExpandedToolOutputCell` — 5 de los 8
tipos; `UserCell`/`AssistantMarkdownCell`/`HelpCell` no usan `Color` en
absoluto) gana un campo `theme: Theme` fijado al construirse, y
`as_text()` lee `self.theme.X` en vez de la constante hardcodeada. Más
mecánico por sitio de construcción, pero el trait público queda
intacto. `App` gana un campo `theme: Theme` (resuelto una sola vez al
arrancar, nunca cambia en la sesión) y lo pasa en cada construcción de
celda; los dos usos de color directos en `draw()` (texto de hint,
aviso de aprobación pendiente) también leen `self.theme` ahora.

**Config**: `Config::tui_theme: String` (default `"dark"`), overridable
vía `BRAZE_TUI_THEME` — mismo patrón exacto que `best_of_n`. No
validado en `braze-config` (no depende de `braze-tui`); `braze-cli`
resuelve `Theme::from_name(&config.tui_theme)` al arrancar y falla
rápido con `CliError::Startup` si el nombre no es reconocido, antes de
construir el engine o la sesión — mismo momento/patrón que la
validación de `--backend`. Nuevo flag `braze chat --tui --theme
<dark|light|high-contrast>`.

**Tests**: 411 → 421 (6 nuevos en `theme.rs`: cada preset reconocido por
nombre, nombre desconocido rechazado, default es `dark`, alto contraste
nunca usa un tono apagado; más 1 en `braze-config` para el override vía
env, y 3 tests movidos con el refactor mecánico de las 5 celdas
themadas — mismo conteo, solo con `theme: Theme::dark()` agregado).
`cargo build/test/clippy --workspace` verdes. Los snapshots `insta`
existentes de las 5 celdas afectadas **no cambiaron** — confirma que
`Theme::dark()` reproduce exactamente los colores que estaban
hardcodeados antes del refactor.

**Verificación manual**: `braze chat --tui --theme bogus` falla al
arrancar con `"tema de TUI desconocido: 'bogus' (esperado 'dark',
'light', o 'high-contrast')"`, antes de tocar el engine o la sesión;
`braze chat --help` muestra el flag `--theme` documentado.

## Fase TUI 2 — backtrack: Esc-Esc para retroceder y editar (2026-07-05)

**Investigación previa a implementar** (agente `Explore`, ver
`crates/braze-session/src/store.rs`): el trait `SessionStore` es
estrictamente append-only —

```rust
async fn append(&self, session: &SessionId, event: &AgentEvent) -> Result<(), SessionError>;
async fn load(&self, session: &SessionId) -> Result<Vec<AgentEvent>, SessionError>;
async fn list_sessions(&self) -> Result<Vec<SessionId>, SessionError>;
```

— sin ningún método `truncate`/`rewind`. `FileSessionStore` (única
implementación) mantiene además un cache en memoria por sesión que
`append` extiende — "reescribir el store con menos eventos" no es un
patrón existente en absoluto (la compactación tampoco lo hace: genera
un `AgentEvent::CompactionOccurred` **agregado** al final, nunca
reemplaza nada). Truncar/rebobinar la sesión actual in-place habría
significado ampliar uno de los tres traits "contrato congelado" del
proyecto (`ToolProvider`, `ModelBackend`, `SessionStore`/
`ContextCompactor` — ver PLAN.md) y reconciliar el cache de
`FileSessionStore` — alcance mucho mayor al de un incremento de TUI.

**Resolución adoptada — sin tocar el contrato congelado**: en vez de
mutar la sesión existente, "retroceder" arma una **sesión nueva**:
`Engine::run_turn` ya recarga el log completo del store en cada turno
(`load_messages`, sin ningún estado de historial cacheado en el
`Engine` mismo), así que reproducir el prefijo de eventos anterior al
mensaje objetivo en un `SessionId::new()` — vía `store.append` en un
loop, evento por evento — usando solo `append`/`load` (que ya existen)
logra exactamente la semántica de "como si lo que pasó después nunca
hubiera ocurrido", sin tocar el trait para nada. Ventaja adicional
sobre mutar in-place: la sesión original queda intacta y sigue siendo
`--resume`-able con su historial completo — nada se destruye, coherente
con la cultura de este proyecto de "rollout log como fuente única de
verdad" (Grupo G).

**UI**: Esc-Esc (dos toques dentro de `DOUBLE_ESC_WINDOW` = 600ms,
solo con `!turn_running` — un Esc con turno corriendo sigue
interrumpiendo como antes) abre `ComposerPopup::Backtrack`, una
variante nueva del mismo popup que ya usan `/`/`@` (reusa
`POPUP_MAX_VISIBLE`, `move_popup_selection`, `draw_popup` — mismo
patrón, sin viewport nuevo). Lista los últimos `AgentEvent::UserMessage`
más recientes primero, cada uno resumido a su primera línea
(`backtrack_preview`, mismo criterio de recorte que
`summarize_tool_output`). Aceptar (Tab/Enter) no ejecuta nada por sí
solo — mismo criterio que `/`/`@`: `backtrack_to` reproduce el prefijo
en la sesión nueva, cambia `self.session`, y carga el texto del
mensaje elegido en el composer para editar; un Enter aparte (con el
popup ya cerrado) reenvía.

**Bug encontrado y corregido antes de dar por bueno el incremento**:
`refresh_popup` (que `on_key` llama incondicionalmente al final de
cada tecla para re-evaluar triggers `/`/`@`) sobreescribía el popup de
backtrack recién abierto en la MISMA tecla que lo abrió, porque no
está condicionado a ningún trigger de cursor — el composer sigue vacío
cuando se abre. Encontrado en la primera pasada de verificación en vivo
(el popup nunca aparecía en el volcado de pantalla) antes de asumir que
funcionaba. Fix: `refresh_popup` ahora retorna temprano si
`self.popup` ya es `Backtrack` — solo `accept_popup_selection` o un Esc
explícito lo cierran.

**Tests**: 421 → 424 (3 nuevos para `backtrack_preview`, función pura).
`cargo build/test/clippy --workspace` verdes.

**Verificación manual en vivo** contra el binario de prueba (3 rondas
scripteadas: `read_file` + texto, texto solo, texto solo): tras enviar
dos mensajes ("leeme el archivo", "dame otra cosa"), Esc-Esc mostró el
popup con "↩ dame otra cosa" y "↩ leeme el archivo" (más reciente
primero); flecha abajo movió la selección; Enter cargó
`"leeme el archivo"` en el composer, commiteó el aviso de backtrack, y
**la barra de estado cambió el id de sesión corto** (confirmando que se
creó una sesión nueva de verdad, no una mutación); editar el texto
(`"leeme el archivo otra vez"`) y reenviar produjo un turno nuevo normal
con la tercera ronda scripteada, sin errores. Salida limpia con Ctrl+C.

## Fase TUI 2 — `/model` picker + `--ollama-url` (2026-07-06)

Cierra el "gap #1 de UX" identificado en la revisión de OpenCode
(`docs/SOTA-2026-07.md` § Adenda 2026-07-06): cambiar de backend/modelo
a mitad de sesión sin salir de la TUI. Diseño = el esbozado ahí mismo:
**rebuild del `Engine` + resume del mismo session id** (el engine relee
el historial del store en cada turno, así que reconstruirlo no pierde
nada; la conversación sigue exactamente donde iba).

- **`/model <backend>[:<modelo>]`** cambia directo (split en el *primer*
  `:`, misma convención que `--planner`); **`/model`** a secas abre un
  picker (`ComposerPopup::Model`). El picker *ventanea* sobre la lista
  completa de candidatos alrededor de la selección (marcador `(n/total)`,
  `popup_window_start`) en vez de capear a `POPUP_MAX_VISIBLE` como
  Backtrack — no hay query que estreche la lista y la cola no es menos
  relevante que la cabeza. Fuzzy search estilo OpenCode queda diferido.
- **Candidatos** (computados una vez al arranque en `braze-cli`, misma
  staleness aceptada que `mentionable_files`): cada backend construible
  con la config actual (credenciales + modelo presentes), con la entrada
  Ollama expandida a los modelos instalados del server vía
  `braze_model::list_ollama_models` (`GET /api/tags`, timeouts cortos,
  best-effort: Ollama caído degrada al modelo configurado, nunca bloquea
  el arranque).
- **Costura `braze_tui::EngineFactory`** (`spec -> Future<Result<(Engine,
  String), String>>`): `braze-tui` sigue sin depender de
  `braze-config`/`braze-model`; `braze-cli` implementa el factory sobre
  el nuevo `build_engine(...)` extraído de `run()` — la misma
  composición que el arranque (guards re-sembrados con las aprobaciones
  de la sesión *viva* vía `store.load`, providers locales + MCP
  reconectados, compactor, budget de contexto Ollama, planner del
  arranque preservado vía `planner_spec`). El rebuild corre como task en
  background (`switching_model` gatea submits y switches concurrentes;
  el resultado vuelve por un canal dedicado `switch_rx`); un spec
  inválido o un backend sin credenciales produce un `ErrorCell` y el
  engine anterior sigue intacto.
- **`--ollama-url <url>`** en `chat` y `run`: override de
  `ollama_base_url` con la misma precedencia que `--model` (gana a config
  file y `BRAZE_OLLAMA_BASE_URL`) — para apuntar a un nodo de inferencia
  LAN (p.ej. Nitro) sin editar config.

**Tests**: 548 → 558 (parseo de `--ollama-url`, registro/parseo de
`/model`, `popup_window_start`, `list_ollama_models` contra el server
enlatado ×4). `cargo build/test/clippy --workspace` verdes; snapshot de
`HelpCell` actualizado (línea `/model`).

**Verificación manual en vivo** (pty scripteado, binario real, config
real openrouter + Ollama local con 7 modelos): (1) arranque con status
`openrouter:openai/gpt-4o-mini`; (2) `/model ollama:qwen2.5:3b` → notice
"cambiando…", "✓ modelo cambiado", status bar actualizada; (3) `/model`
→ picker con 8 candidatos, 4×Down deslizó la ventana y Enter cambió al
candidato 5 (`ollama:gemma4:latest`) — windowing y selección correctos
end-to-end; (4) `/model foo:bar` → `ErrorCell` "no se pudo cambiar…" con
la TUI intacta; (5) `/quit` salió limpio (status 0). Nota de harness: en
pty hay que detectar la salida con `waitpid`, no `kill(pid, 0)` (el
zombie sin cosechar responde como vivo) — y los asserts van sobre las
celdas commiteadas al scrollback, no sobre el render diffeado del popup
(ratatui intercala escapes de posicionamiento dentro del texto).

## Rescate textual por formato nativo Qwen `<tool_call>` (2026-07-06)

Ítem 2 del backlog post-revisiones (docs/SOTA-2026-07.md § Adenda,
"Rescate textual por familia de modelo" — la palanca [S] de máxima
evidencia según el TR de Qwen: los modelos chicos sobreajustan a su
tool-template de entrenamiento, y matchear el rescate a lo que la
familia emite nativamente es la palanca de confiabilidad más respaldada).

**Qué se agregó** (`crates/braze-engine/src/engine.rs`):

- `extract_tagged_tool_calls`: reconoce el formato nativo Qwen/Hermes
  `<tool_call>\n{"name": ..., "arguments": ...}\n</tool_call>`. A
  diferencia del rescate B5 (JSON desnudo que debe ser la respuesta
  *completa* — sin marcadores, la prosa alrededor es demasiado ambigua),
  los tags explícitos permiten: **prosa alrededor preservada** como texto
  de la ronda (run_turn ya persiste texto antes de los tool calls),
  **varios bloques** por respuesta (Qwen emite un par de tags por call
  paralela), y JSON interno opcionalmente fenceado. Un bloque taggeado
  con JSON malformado queda *visible en el texto* en vez de tragarse
  (claramente era un intento de call, pero inventar una reparación
  arriesga ejecutar algo que el modelo no dijo); un tag sin cerrar
  (ronda cortada a media emisión) no rescata nada.
- Escalera en `complete_once`, de lo más específico a lo más ambiguo:
  (1) tool calls estructurados del backend, (2) formato taggeado,
  (3) JSON desnudo (B5). Aplicada por intento (no post-voto best-of-n),
  igual que B5. Helpers compartidos `parse_tool_call_json` /
  `trim_json_fences` para que ambos formatos no deriven.
- **Diferido**: la gramática XML `<function=...><parameter=...>` de
  qwen3-coder — no hay modelo de esa familia instalado contra el cual
  validarla (ver SOTA § Adenda).

**Tests**: 558 → 569 (10 unitarios del extractor + 1 integración con
`ScriptedModel`: dispatch real del call taggeado, prosa persistida como
`AssistantText`, tags/JSON fuera de la conversación).
`cargo build/test/clippy --workspace` verdes.

**Validación con bench real** (g10-weak-skills, ollama:qwen2.5:3b, 3
reps, seed 42): pass 2/6 vs 1/6 del baseline pre-cambio — dentro del
ruido (±30pp) y **no atribuible al rescate**: en este sweep el formato
taggeado no apareció (qwen2.5 vía el template nativo de Ollama emite
calls estructurados normalmente; el formato taggeado es el modo de
falla documentado por el TR — template sin tools, truncación, otros
serving stacks — no el caso común local). El rescate es una red de
seguridad con costo cero cuando no dispara, validada por unit tests.

**Hallazgo colateral (arreglado)**: `braze-bench` no instalaba
subscriber de tracing, así que `RUST_LOG=info` sobre un sweep no
mostraba nada — imposible saber si una palanca del engine (rescates,
compactación) disparó durante el bench. Ahora instala el mismo
subscriber que `braze-cli` (stderr, respeta `RUST_LOG`; los binarios y
solo los binarios instalan subscribers, per convención del workspace).

## Backlog post-revisiones — ítem 3: hardening de streaming (2026-07-06)

Último ítem del backlog rankeado (cierra el backlog completo, 1-7).
Préstamo del StreamingToolCallParser de OpenCode (docs/SOTA-2026-07.md §
Adenda) — fallas reales de proveedores en el ensamblaje de tool calls
por streaming:

- **Escalera de reparación de argumentos** (`braze-model/args_repair.rs`,
  compartida por los wires de Anthropic y OpenRouter): parse directo →
  reparación de truncamiento (cerrar string abierta con cuidado del `\`
  colgante, quitar coma colgante, balancear `{}`/`[]` — solo repara
  cortes, no daño estructural previo) → colapso a `{}`. Los
  `finalize_tool_call` de ambos wires pasan de "dropear la call en
  silencio" (la ronda 'convergía' sin ejecutar lo pedido) a infalibles:
  la call se despacha y el error de schema/tool que produzca es la señal
  de reintento visible para el modelo.
- **Remap de colisión index/id** (OpenRouter): un upstream que reusa
  `index: 0` para varias calls secuenciales (re-anunciando otro id) ya
  no corrompe el buffer fusionando dos calls — la anterior se desplaza a
  `displaced_tool_calls` y se emite primero, en orden de llegada.
- **Fragmentos sin `index`** (upstreams estilo LM Studio): antes se
  descartaban enteros; ahora un fragmento con id/name anuncia una call
  nueva (call completa en un solo chunk) y uno solo-argumentos continúa
  la última.
- **`finish_reason` duplicado** (OpenRouter): ya era inocuo por el
  `mem::take` del drain — ahora está pineado por test.
- El quinto modo de la lista ("XML crudo en streaming", LM Studio) lo
  cubre el rescate textual del engine (ítem 2 + gramática XML), no el
  wire.

**Tests**: 597 → 612 (9 de la escalera + nuevos de colisión/sin-index/
duplicado en openrouter_wire + semántica infalible en ambos wires).
Verificación en vivo del happy path: `braze run` contra OpenRouter real
(gpt-4o-mini) con tool call `read_file` — streaming, dispatch y
respuesta correctos.

## Backlog post-revisiones — ítems 4-7 + extensión XML (2026-07-06)

Cuatro préstamos de la revisión OSS (docs/SOTA-2026-07.md § Adenda)
implementados en tanda, más la parte diferida del ítem 2 destrancada por
el nodo Nitro. Commits: `5509c11` (4), `e4a1bf2` (5), `18c2d44` (7,
knobs), `dc0df4d` (6), `bf4bc1e` (XML). Tests 569 → 597 acumulado.

- **Ítem 4 — colapso ACI de observaciones** (`history.rs`): la i-ésima
  observación táctica se colapsa a su primera línea (cap 160 chars) +
  marcador cuando hay ≥5 observaciones más recientes
  (`TACTICAL_FULL_OBSERVATIONS`, ACI Tabla 3: +3.0 pp). En render, no
  destructivo; guard de no-op para contenido corto.
- **Ítem 5 — guardrail `cargo check` post-edit** (`braze-tools-local/
  post_edit_check.rs`): tras write/edit exitoso de un `.rs` (Cargo.toml
  ancestro más cercano), los errores de `cargo check
  --message-format=short` (timeout 60s, cap 2000 chars) vuelven en el
  mismo tool result — solo en fallo, happy path gratis. Opt-out
  `disable_post_edit_check`. cargo ausente/timeout: skip silencioso.
- **Ítem 6 — `EscalatingBackend`** (`braze-model/escalation.rs`):
  decorator lead/worker reactivo estilo Goose — lead abre
  (`lead_turns=3`), 2 observaciones fallidas consecutivas al final del
  historial lo traen de vuelta por 3 calls. Detección stateless por
  request (la cola de `ToolResult is_error`): retries/best-of-n no
  double-cuentan, un mensaje nuevo del usuario resetea. `--lead
  <backend>[:modelo]` + `lead_backend`/`lead_model`, espejo de
  `--planner` — proactivo (planner) y reactivo (lead) componen. A/B en
  bench pendiente (falta sintaxis de spec con lead).
- **Ítem 7 — knobs de sampling Ollama**: `with_top_p/with_top_k/
  with_repeat_penalty` (None = defer al Modelfile) + `--top-p/--top-k/
  --repeat-penalty` en braze-bench.
- **Extensión ítem 2 — gramática XML qwen3-coder**: `<function=name>` +
  `<parameter=key>` (valores verbatim como strings; solo `{...}`/`[...]`
  se parsea como JSON), aceptada dentro del wrapper `<tool_call>` y
  bare. Escalera final: estructurado → `<tool_call>` (JSON|XML) →
  `<function=>` bare → JSON desnudo.

### Sweep A/B de sampling (ítem 7) — veredicto

g10-weak-skills, 3 reps, seed 42, `--no-ollama-stop`, **contra Nitro**
(192.168.1.8, box dedicada — ver abajo). Config A = temp 0.2 (default
del bench); config B = recomendación Qwen (temp 0.7 / top_p 0.8 /
top_k 20 / repeat_penalty 1.05). JSONs en scratchpad de la sesión
(`nitro-*.json`).

| modelo | temp 0.2 | qwen-rec | mediana ms/tarea |
|---|---|---|---|
| qwen2.5:3b | 0/6 | **2/6** (distractor 2/3) | ~1.9k |
| qwen3.5-coder | **6/6** | 5/6 | ~20-27k |

- **Para qwen2.5:3b el sampling recomendado ayuda**: 0/6 → 2/6 en Nitro,
  y la corrida local (contaminada por builds concurrentes, ver nota)
  fue direccionalmente igual (0/6 → 3/6). N=6 por celda (±30pp), pero
  consistente en dos entornos: para la familia Qwen chica conviene
  correr con qwen-rec. El default 0.2 del bench no cambia (es el
  neutral entre backends); usar los flags por sweep.
- **error_recovery sigue 0/3 en qwen2.5:3b con ambos samplings** — esa
  skill no es de sampling, es de capacidad/harness.
- **qwen3.5-coder clava 6/6 en las skills débiles a temp 0.2** — primer
  modelo local que satura g10; con qwen-rec 5/6 (ruido; a un modelo ya
  saturado el sampling no le suma). Es modelo *thinking* (Ollama
  devuelve campo `thinking` separado; con `num_predict` chico el
  content puede salir vacío — presupuestar tokens acorde). **Nuevo
  mejor modelo local del proyecto**, corre en Nitro a ~20-27s/tarea.
- **0 activaciones de rescate textual en los 4 legs**
  (`RUST_LOG=braze_engine=info`, ahora medible tras el subscriber de
  braze-bench) — vía template nativo de Ollama no hay leak; ambos
  formatos quedan como red de seguridad validada por unit tests.

### Nota de metodología: benchear en Nitro, no en la máquina de trabajo

La re-corrida local del baseline dio 0/6 donde la corrida de la mañana
(misma config, mismo seed) había dado 2/6 — la diferencia más probable:
los builds/tests que esta sesión corría en paralelo sobre una máquina
CPU-bound contaminaron la inferencia. En Nitro, qwen2.5:3b corre el
mismo bench ~50× más rápido (mediana ~1.9s vs ~90-100s por tarea) y sin
competir con el trabajo interactivo. **Regla operativa**: sweeps de
braze-bench van a Nitro (`BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434`
+ `--no-ollama-stop` — el `ollama stop` del bench habla con el server
local, no el remoto), y la TUI/CLI puede apuntar ahí con `--ollama-url`.
Pendientes de infra que esto agrava: fijar la IP de Nitro en el router
(hoy DHCP) y configurar `OLLAMA_KEEP_ALIVE` en el nodo.

## Split planificador/ejecutor (`with_planner`) — diseño (2026-07-06)

> **Estado: diseño aprobado, implementación pendiente.** Primera pieza de
> la reorientación "maestro en modelos pequeños": un modelo grande
> planifica una vez por turno, un modelo chico ejecuta el volumen de
> rondas de tool-calling.

### Motivación y pre-commitment de medición

El sweep de `braze-bench` (2026-07-04/05, ver `docs/AUDITORIA-2026-07.md`
§ G10) mostró el patrón exacto que este split ataca: `qwen2.5:3b`/`7b`
rinden razonablemente en `single_tool` pero colapsan en las skills que
requieren razonamiento multi-paso (`error_recovery` y
`distractor_selection` en 0/5 pre-G10; `multi_step` débil). La hipótesis:
el costo de una única llamada de planificación a un modelo fuerte
(OpenRouter/Anthropic) es marginal frente al volumen de rondas del
ejecutor, y un plan explícito le quita al modelo chico justo la parte en
la que falla.

**Cautela obligatoria**: `docs/SOTA-2026-07.md` § hallazgo 5 registra una
señal débil (arXiv 2511.00872, no verificada a fondo) de que
arquitecturas multi-agente rinden *peor* que un agente secuencial en
tareas de código. Este split es más simple que multi-agente real
(traspaso unidireccional, sin negociación), pero la lección aplica: **la
técnica se adopta solo si el sweep A/B la valida** (criterio abajo). Si
no mejora, se documenta como técnica rechazada y se elimina — mismo
pre-commitment que el resto de la metodología de auditoría.

### Decisión de arquitectura: engine-level, no decorator

Dos opciones evaluadas:

- **(A) Decorator `PlannerExecutorBackend`** implementando `ModelBackend`
  sobre dos backends internos — cero cambios al engine. Fue la sugerencia
  inicial en conversación; **se descarta tras análisis**: el plan viviría
  en un side-channel invisible al log de sesión — se pierde en `--resume`,
  la compactación no lo conoce (contexto contradictorio post-compact), la
  TUI no puede mostrarlo, y el bench no puede distinguir turnos
  planificados. Además el decorator tendría que inferir límites de turno
  desde la forma de los mensajes (frágil ante reparaciones de huérfanos y
  resúmenes de compactación). Contradice el principio central del diseño:
  el event log es la fuente de verdad.
- **(B) Soporte opt-in en `Engine` (elegida)**: el plan es un
  `AgentEvent` persistido como cualquier otro — sobrevive resume, la
  compactación lo digiere, la TUI lo espeja vía `TurnObserver`, el bench
  lo cuenta.

### Contrato

- **`AgentEvent::PlanCreated { plan: String }`** — enmienda aditiva al
  contrato congelado, mismo precedente que `AssistantToolCall` (Fase 5),
  `Usage` (Grupo G) y `Unknown` (C9). Binarios viejos leyendo un log con
  este evento deserializan a `Unknown` sin romper `load()` (mecanismo C9
  ya verificado). Agregar la variante obliga a tocar todos los `match`
  exhaustivos (`history::event_to_block*`, compactor, `event_text_len`,
  TUI) — el compilador guía; es el mismo costo que pagó `Usage`.
- **`Engine::with_planner(Box<dyn ModelBackend>)`** — campo
  `planner: Option<Box<dyn ModelBackend>>`, `None` por default (cero
  cambio de comportamiento sin opt-in), chainable como
  `with_best_of_n`/`with_context_budget`.
- **Config**: `planner_backend: Option<String>` +
  `planner_model: Option<String>` (`#[serde(default)]`, env
  `BRAZE_PLANNER_BACKEND`/`BRAZE_PLANNER_MODEL`, flag `--planner
  <backend:modelo>` en `chat`/`run`). Validación del nombre de backend en
  `braze-cli`, igual que `default_backend`.

### Flujo en `run_turn`

Pre-loop, después de `load_and_repair` + append del `UserMessage` +
primer `load_messages`; **no cuenta contra `MAX_TURN_ITERATIONS`**:

1. Request al planner: mismos `messages`, `tool_stubs` **vacío** — las
   herramientas van inlined como texto (name + summary de los stubs) en
   el system prompt de planificación. Razón: queremos *conciencia* de
   herramientas sin riesgo de invocación, y es exactamente la filosofía
   de carga diferida (solo nombres/resúmenes en contexto).
   `max_tokens` capped a `PLANNER_MAX_TOKENS = 1024` (los planes son
   cortos; control de costo).
2. System prompt de planning = prompt base + instrucciones: "no llames
   herramientas; escribe un plan numerado corto (3-7 pasos) usando las
   herramientas disponibles, con nombres concretos; si la petición es
   trivial, responde con el único paso". La cláusula de trivialidad evita
   inflar `no_tool`/`single_tool` con planes-overhead.
3. Degradación, nunca fallo: error del planner ⇒ `warn!` y el turno
   procede sin plan (misma filosofía que N-13 — un enhancement opcional
   jamás mata el turno). Respuesta con tool_calls ⇒ se ignoran con
   `warn!` y se usa el texto. Respuesta `truncated` (espíritu N-24) o
   vacía ⇒ plan descartado con `warn!`.
4. Persistir `PlanCreated` vía `append_and_notify` + persistir el
   `Usage` del planner como evento normal (costo visible; el conteo de
   `rounds` del bench incluirá la ronda de plan — documentado, es honesto:
   ES una ronda de modelo). Recargar `messages` y entrar al loop normal.

### Render en history y protocolo

`PlanCreated` → bloque `Text` con rol `Assistant`, contenido
`"Plan:\n{plan}"`. El framing "el modelo sigue su propio plan" es
deliberado (mejor adherencia que un plan inyectado como user). Sobre
mensajes assistant consecutivos (plan seguido del `tool_use` de la ronda
1): **ya es la forma existente** — `push_grouped` no fusiona `Text` con
`ToolUse`, así que un round con texto-antes-de-tools ya produce hoy dos
mensajes assistant consecutivos, verificado en vivo contra Anthropic real
(sweep + N-2b re-test). Aun así, oleada 1 incluye test con
`ProtocolValidatingModel` del shape plan→tool_use→tool_result.

En el compactor: tipo *orphan* (no settled — un plan es estado por-turno,
no durable), y `compact_tactical` extrae el plan al digest (es
exactamente el contexto que quieres que sobreviva una compactación a
mitad de un turno largo).

### Interacciones

- **best_of_n (G10)**: ortogonal — aplica solo a las rondas del ejecutor.
  Best-of-n de planes: diferido.
- **Replanning**: fuera del MVP. Política futura candidata: replan tras K
  fallos consecutivos de tools. Se decide con datos del sweep, no antes.
- **N-34/N-36**: el planner del bench se construye vía el mismo
  `BackendSpec::build(config, sampling)` — misma temperatura/seed y mismo
  system prompt base que producción.
- **TUI**: `PlanCreated` llega por el espejo de `TurnObserver`; oleada 1
  lo renderiza como celda simple (texto muted con prefijo "plan"); pulido
  visual diferido.

### Bench A/B — el criterio de éxito

- Sintaxis de spec: `<executor-spec>+plan:<planner-spec>` (split en el
  literal `"+plan:"`, inambiguo frente a los `:` de los tags de Ollama).
  Ej.: `--backends
  ollama:qwen2.5:3b,ollama:qwen2.5:3b+plan:openrouter:deepseek/deepseek-v4-flash`
  — baseline y variante planificada en el mismo sweep, apples-to-apples.
- `TaskResult` gana `planned: bool` (derivado de la presencia de
  `PlanCreated` en el log del run) para el análisis del JSON.
- **Criterio (pre-comprometido)**: con `--seed`/`--temperature` fijos y
  ≥5 repeticiones, la variante planificada debe mejorar el pass rate de
  `multi_step` + `error_recovery` **fuera del intervalo de Wilson del
  baseline** (no dentro del ruido de ±5pp), sin degradar
  `no_tool`/`single_tool`/`distractor_selection`. El costo extra de
  tokens del planner queda registrado por las sumas de `Usage` y entra al
  veredicto. Si no cumple: técnica rechazada, se documenta y se remueve.

### Oleadas

1. **Evento y render**: `PlanCreated` en `braze-events`; arms en
   `history::event_to_block`/`event_to_block_cleared`, clasificación
   orphan + extracción al digest en `simple_compactor`, `event_text_len`
   en el engine, celda simple en la TUI. Tests unitarios + test de
   protocolo (`ProtocolValidatingModel`, shape plan→tools).
2. **`Engine::with_planner`**: campo + builder, llamada de planning
   pre-loop con las 4 reglas de degradación, prompt de planning, tests
   con `ScriptedModel` como planner y ejecutor (incluye: planner que
   falla ⇒ turno procede; planner truncado ⇒ plan descartado).
3. **Config/CLI**: campos + env + `--planner`, validación, wiring en
   `braze-cli::main`.
4. **Bench + veredicto**: sintaxis `+plan:`, wiring en `runner`,
   `planned` en `TaskResult`, y el sweep real (qwen2.5:3b solo vs
   +plan:deepseek-v4-flash, 5 reps, seed fijo) → decisión adoptar/rechazar
   documentada aquí.

### Veredicto del sweep A/B (2026-07-06) — RECHAZADA en su forma actual

Sweep real (`docs/sweep-planner-ab.json`/`.log`): 10 tareas × 5 reps ×
2 filas, `--seed 42`, temperatura 0.2, planner deepseek-v4-flash sobre
ejecutor qwen2.5:3b. Los 50 runs planificados persistieron plan
(`planned: true` en los 50 — cero degradaciones del planner).

**Contra el criterio pre-comprometido: falla decisivamente.**

| skill | baseline | +plan | Δ |
|---|---|---|---|
| single_tool | 25/30 | 22/30 | −3 |
| no_tool | 5/5 | **0/5** | −5 |
| multi_step | 1/5 | 1/5 | 0 |
| error_recovery | 0/5 | 0/5 | 0 |
| distractor_selection | 2/5 | 4/5 | +2 |
| **total** | **33/50 (66%)** | **27/50 (54%)** | **−12pp** |

Las skills objetivo (`multi_step`+`error_recovery`) no se movieron un
milímetro (1/10 vs 1/10); `no_tool` colapsó por completo; y el costo fue
real: +29% tokens de entrada, 4× tokens de salida, +1 ronda por turno.

**Diagnóstico del colapso (verificado en el JSON, no especulado)**: los
5 fallos de `no_tool` y 4-5 de `single_tool` son todos
`EmptyModelResponse` — el ejecutor ve el plan renderizado como mensaje
*assistant* ("Plan:\n1. responder X"), lo interpreta como que ya
respondió, y emite una completion vacía (que la estrictez nueva del
Grupo N convierte correctamente en error en vez de éxito silencioso).
El framing "el modelo sigue su propio plan" — elegido deliberadamente en
el diseño — **se vuelve en contra en tareas triviales**: un plan con rol
assistant que contiene la respuesta lee como conversación terminada
para un 3B. La cláusula de trivialidad del prompt de planning resultó
insuficiente como mitigación. Señal secundaria: la fila planificada
produjo 6 schema_fails (vs 0) y 4 denegaciones de permiso (vs 0) — el
plan vuelve al ejecutor chico *más* errático en tareas simples, no
menos.

**Lo único que mejoró**: `distractor_selection` 2/5 → 4/5 — consistente
con la hipótesis de que el plan ayuda a *seleccionar* (el planner
identifica el archivo correcto), aunque con n=5 está dentro del ruido.

**Calibraciones externas pertinentes** (revisiones del mismo día,
`docs/SOTA-2026-07.md`): Aider observó +3-4pp con architect/editor — la
ganancia esperable era chica; y su lección "si el split no rinde, el
primer sospechoso es el formato de edición del ejecutor" sigue en pie
para `multi_step` (el plan correcto no sirve si el 3B no puede
materializar la edición).

**Decisión**: conforme al pre-commitment, la técnica queda **rechazada
en su forma actual** — `with_planner` permanece en el código como
opt-in (default `None`, cero efecto sin activarlo) pero NO se
recomienda ni se promueve a default. Queda registrada la opción de UNA
iteración acotada y pre-registrada atacando el artefacto diagnosticado
(descartar planes de un solo paso en vez de persistirlos, y/o render
del plan con rol user como contexto en vez de assistant) con el mismo
criterio A/B; si tampoco mueve `multi_step`/`error_recovery`, remoción
completa.

## Grupo O — la superficie de archivo usable (docs/AUDITORIA-2026-07-v3.md, 2026-07-06)

La auditoría v3 (primera pasada SLM-first, no un bug-hunt) identificó que
`read_file`/`write_file`/`edit_file` tenían una trampa estructural para el
archivo *mediano* (no el enorme): `read_file` truncaba a 8 KB
(`MAX_TOOL_OUTPUT_BYTES`, ~200 líneas) sin paginación, así que un ejecutor
3-7B que necesitaba editar más allá del corte no podía — `edit_file` solo
puede matchear texto que el modelo vio, y el steering "usa write_file con el
contenido completo" presuponía un contenido completo que el modelo nunca
recibió. Peor: si el modelo obedecía ese steering igual, `write_file`
sobrescribía el archivo entero con lo poco que vio, destruyendo el resto en
silencio.

**Grupo O — ✅ CERRADO 2026-07-06.** Los cuatro hallazgos (A1-A4):

- **A1 (paginación de `read_file`)**: `ReadFileArgs` gana `offset`/`limit`
  (1-indexados, por línea). Sin argumentos y archivo ≤200 líneas
  (`DEFAULT_PAGE_LINES`), comportamiento sin cambios (verbatim). Un archivo
  más grande sin argumentos devuelve la primera página con un trailer
  `[N more lines below — call read_file again with offset=X to continue]`;
  con `offset`/`limit` explícitos, devuelve esa ventana con el encabezado
  `[lines X-Y of N]`. `offset=0` y `offset` más allá del fin del archivo son
  errores recuperables con mensaje explícito. Schema actualizado
  (`schema.rs`) para que el modelo vea los dos parámetros nuevos.
- **A2 (guarda en `write_file`)**: antes de escribir, se lee el tamaño previo
  del archivo (si existe). Si el nuevo contenido es ≥500 bytes más chico que
  el anterior, la escritura se aplica igual (no se bloquea — puede ser
  intencional) pero el resultado exitoso lleva un `WARNING` explicando que
  puede haber descartado el resto de un archivo que solo vio truncado/
  paginado, con la sugerencia de usar `read_file` con `offset` o `edit_file`
  la próxima vez.
- **A3 (mejor error de matching en `edit_file`)**: `find_closest_line` hace
  un best-effort "¿quisiste decir?" por solapamiento de palabras entre la
  primera línea no vacía de `old_string` y cada línea del archivo; si hay
  solapamiento, el mensaje de `NotFound` incluye `"The closest match in the
  file is line N: ..."`. Sin solapamiento, el mensaje queda como antes (sin
  ruido).
- **A4 (steering al crear archivo)**: `edit_file` sobre una ruta inexistente
  ya no propaga el `io::Error` crudo ("No such file or directory (os error
  2)") — detecta `ErrorKind::NotFound` y responde con steering explícito a
  `write_file`.

12 tests de regresión nuevos entre `read_file.rs`, `write_file.rs` y
`edit_file.rs` (paginación en sus 6 variantes — verbatim, primera página,
ventana explícita, última página sin hint, offset fuera de rango, offset=0;
warning de shrink en sus 3 variantes — shrink real, tamaño similar, archivo
nuevo; hint de línea cercana en sus 2 variantes — con y sin solapamiento; y
el steering de archivo inexistente). Workspace completo verde (`cargo test
--workspace`), clippy limpio (`cargo clippy --workspace --all-targets -- -D
warnings`).

## Grupo P — calibrar la aritmética de contexto a `num_ctx=8192` (docs/AUDITORIA-2026-07-v3.md, 2026-07-06)

Los cuatro hallazgos (B5, B1, B2, B4) — todos consecuencia de constantes
copiadas de contextos grandes (SWE-agent/ACI mide contra GPT-4) o de un
margen fijo que nunca se pensó para el `num_ctx=8192` default de Ollama.

**Grupo P — ✅ CERRADO 2026-07-06.**

- **B5 (max_tokens regala la mitad de la ventana)**: `max_tokens` es un
  campo compartido por los tres backends (Anthropic/OpenRouter tienen
  contexto grande y no lo necesitan chico) — bajar su *default* global
  habría regresionado esos dos backends. En vez de eso,
  `Config::validate` emite un `tracing::warn!` (no error — la config
  default sigue siendo válida) cuando `default_backend == "ollama"` y
  `max_tokens` reserva al menos la mitad de `ollama_num_ctx`, señalando el
  lever real (bajar `max_tokens` en la config, no en el código).
- **B1 (sin cap agregado sobre las 5 observaciones full)**: `history.rs`
  gana `MAX_FULL_OBSERVATIONS_TOTAL_CHARS = 8_000` — `tactical_full_observation_indices`
  camina las candidatas (últimas `TACTICAL_FULL_OBSERVATIONS`) de más
  nueva a más vieja, admitiendo cada una al set "full" mientras el total
  acumulado quepa; la más nueva siempre entra primero sin importar su
  tamaño (la salida del turno actual nunca debe desaparecer). 5
  observaciones de 3000 chars cada una (15.000 total, dentro de la
  ventana de recencia) ya no quedan las 5 completas — solo las que caben
  en el agregado, el resto colapsa aunque estén "dentro de las últimas
  5".
- **B2 (estimador mide la táctica en crudo)**: nueva `render_tactical_events`
  (extraída del bloque que antes vivía inline en `build_messages_with_never_clear`,
  aplica el mismo colapso ACI + el cap de B1) — `estimate_prompt_tokens`
  ahora mide el lado táctico a través de ella (`estimate_message_tokens`),
  igual que ya hacía con `render_durable_events` del lado durable (fix
  N-6). Antes del fix, el estimador contaba el contenido crudo de
  observaciones que el renderer ya había colapsado a una línea —
  sobre-contaba y podía disparar compactación que no bajaba el prompt
  real ni un token.
- **B4 (margen fijo de 1024 no crece con tools MCP)**: nueva
  `braze_tools_core::tool_stub_definition_bytes` (suma nombre+summary+schema
  de cada stub) + `ollama_context_budget_tokens` en `braze-config` pasa de
  una constante a un margen derivado del largo real del system prompt más
  ese conteo de bytes de tools (divisor de 3 chars/token para el lado
  JSON, más denso que el 4 chars/token del estimador de texto natural;
  piso de 256 tokens para el caso degenerado). `braze-cli::main.rs` y
  `braze-bench::runner.rs` computan `tool_stub_definition_bytes` antes de
  mover `tools`/`system_prompt` a `Engine::new` y aplican el budget
  después de construir el engine.

12 tests de regresión nuevos: `braze-config::config` (4, el warning de
B5), `braze-config::prompt` (2 nuevos + 1 renombrado, el margen dinámico
de B4), `braze-tools-core::registry` (4, `tool_stub_definition_bytes`) y
`braze-engine::history` (2, el cap agregado de B1) — más los 98 tests
preexistentes de `braze-engine` re-verificados verdes bajo el nuevo
estimador de B2 sin tocar ninguna aserción. Workspace completo verde
(`cargo test --workspace`), clippy limpio (`cargo clippy --workspace
--all-targets -- -D warnings`).

## Grupo Q — hablarle al modelo en su plantilla (docs/AUDITORIA-2026-07-v3.md, 2026-07-06)

El gap conceptual central de la v3: toda la inteligencia por-familia de
`braze` vivía del lado de la *salida* (parsers de rescate), ninguna del
lado de la *entrada* (prompt/formato) — al revés de donde la tesis del
propio proyecto (Qwen3-Coder TR: los modelos chicos sobreajustan a su
tool-template de entrenamiento) ubica el mayor retorno. Seis hallazgos
(D1, D2, F2, C1, C2, F1).

**Grupo Q — ✅ CERRADO 2026-07-06.**

- **D1 (prompt/formato por familia)**: `default_system_prompt` gana un
  segundo parámetro `model_name: Option<&str>`. Nuevo `ModelFamily`
  (`braze-config/src/prompt.rs`) infiere `QwenTagged`/`Qwen3CoderXml`/
  `Generic` del nombre del modelo (convención Ollama `family[:tag]`) y
  añade una línea de hint proactivo con la sintaxis nativa de tool-call
  de esa familia — movido del rescate puramente reactivo a una pista en
  el prompt, antes de que el modelo genere nada. Solo se activa cuando
  el backend activo es Ollama (`braze-cli::main.rs`,
  `braze-bench::runner.rs`, este último vía el nuevo
  `BackendSpec::executor_model_name`); Anthropic/OpenRouter no reciben
  hint.
- **D2 (sampling cableado a producción)**: `Config` gana
  `ollama_temperature/seed/top_p/top_k/repeat_penalty` (todos
  `Option<T>`, `None` = default de `OllamaBackend`), con soporte de env
  var (`BRAZE_OLLAMA_*`) y `apply_overrides`. `build_model_backend` en
  `braze-cli::main.rs` los aplica a la rama Ollama — los `with_*` ya
  existían en `OllamaBackend` y en los flags de `braze-bench`, pero
  nunca llegaban a `braze chat`/`braze run`: un sampling mejor
  encontrado en un sweep podía medirse pero no aplicarse jamás en
  producción.
- **F2 (coerción de tipos del rescate XML)**: nueva
  `coerce_arguments_to_schema` en `engine.rs`, invocada en
  `dispatch_tool_calls` justo donde ya se resuelve el schema real, antes
  de validar — corrige en las dos direcciones el defecto de
  `parse_function_xml_tool_call` (sin tipo numérico/booleano nativo):
  string→integer/number/boolean cuando el schema lo pide, y
  object/array→string cuando el schema pide string (el caso espejo:
  `<parameter=content>{"a":1}</parameter>` parseado como objeto en vez
  de texto literal). No-op para llamadas ya bien tipadas (el caso wire).
  Rompía sistemáticamente con qwen3.5-coder, el mejor modelo local del
  proyecto, en cualquier tool con parámetro numérico/booleano.
- **C1 (schema real para MCP ya tocadas)**: `McpToolProvider` gana
  `resolved_names: RwLock<HashSet<String>>` — `resolve_schema` marca el
  nombre como resuelto; `list_stubs` promueve el schema real (ya
  disponible gratis en `tool_cache`) para cualquier tool en ese set, en
  vez de seguir mostrando el placeholder permisivo
  `{"type":"object","additionalProperties":true}` para siempre. Una tool
  MCP nunca usada sigue diferida como antes.
- **C2 (formato pythonic de Llama)**: nuevo `extract_pythonic_tool_calls`
  en la escalera de rescate (entre el XML de qwen3-coder y el JSON
  desnudo) — parsea `[nombre(k="v", n=5, b=true)]` (uno o más calls
  separados por coma dentro de un único par de corchetes), con un
  scanner manual consciente de comillas/paréntesis/corchetes anidados
  para no partir un argumento en una coma interna ni cerrar el bloque en
  un `]` dentro de un string. El wrapper `[...]` es lo que hace seguro
  escanear sin marcador explícito (a diferencia de un `nombre(...)`
  suelto, que colisionaría con prosa). Cubre Llama 3.x, una de las
  familias de modelo local más instaladas y la única sin ninguna vía de
  rescate hasta ahora.
- **F1 (no ejecutar ejemplos fenceados / rechazar definiciones OpenAI)**:
  `extract_tagged_tool_calls`/`extract_function_xml_tool_calls` ganan un
  chequeo `is_inside_code_fence` — un bloque dentro de un fence ```` ``` ````
  se deja como texto (el modelo mostrando un ejemplo, no una fuga real
  del template) en vez de despacharse; y `parse_tool_call_json` rechaza
  `parameters` con forma de definición JSON-Schema
  (`{"type":"object","properties":{...}}`), el shape más común en
  documentación de tool-calling, que antes pasaba el chequeo de forma y
  se despachaba con el schema como si fueran los argumentos.

36 tests de regresión nuevos: `braze-config` (8: 6 de D1 —
`ModelFamily` + hint — y 2 de D2 — parsing de env de los cinco campos de
sampling), `braze-mcp-client` (1, la promoción de schema en
`list_stubs`), `braze-engine` (27: 9 de F2 — 8 unitarios + 1
end-to-end —, 13 de C2 — 12 unitarios + 1 end-to-end —, 5 de F1) — más
los tests preexistentes de los cinco crates tocados, todos
re-verificados verdes. Workspace completo verde (`cargo test
--workspace`), clippy limpio (`cargo clippy --workspace --all-targets --
-D warnings`), `rustfmt` aplicado a los archivos tocados en la sesión.

## Grupo R — economía de la escalación (docs/AUDITORIA-2026-07-v3.md, 2026-07-06)

Ocho hallazgos sobre las palancas de escalación/prompting del loop:
`F3, D3, D6, D4/F4, D5, F6, F7`.

**Grupo R — ✅ CERRADO 2026-07-06.**

- **F3 (la escalación es ciega al guardrail post-edit)**: nueva
  `observation_is_a_failure` en `escalation.rs` — un `ToolResult` cuyo
  contenido incluye el marcador literal `"[post-edit check]"` (ya
  presente en `post_edit_check.rs`) cuenta como falla para
  `trailing_failed_observations` aunque `is_error` sea `false` (el
  guardrail nunca marca error — la edición sí se aplicó). Sin esto, un
  worker cuyas ediciones aplican pero rompen el build nunca disparaba la
  escalación al lead.
- **D3 (no escalar por falla de entorno)**: la misma
  `observation_is_a_failure` excluye del conteo los dos shapes que el
  hallazgo nombra explícitamente — `shell_exec`'s JSON con `"exit_code"`
  y los mensajes `"failed to read/write '...'"` de `read_file`/
  `write_file` — vía `is_environment_signal`. Todo lo demás (validación
  de schema, tool alucinada, ambigüedad de `edit_file`) sigue contando
  igual que antes; alcance deliberadamente acotado a los dos shapes que
  el propio hallazgo nombra, no una taxonomía general.
- **D6 (best_of_n no gateado por backend)**: `braze-cli::main.rs` emite
  un `tracing::warn!` cuando `default_backend == "ollama"` y
  `best_of_n > 1` — el default (1) no se toca; solo avisa cuando el
  usuario lo sube explícitamente contra un backend local ya
  CPU-bound.
- **D4 (contadores de escalación por candidato en vez de por ronda)**:
  `EscalationState` gana `last_round_message_count`/`last_decision` —
  `route()` detecta una llamada repetida de best-of-n (mismo
  `req.messages.len()` que la llamada anterior) y reusa la decisión ya
  tomada en vez de avanzar `calls`/`escalated_remaining` de nuevo. Los
  dos tests preexistentes que repetían `request(vec![])` idéntico entre
  rondas genuinamente distintas se reescribieron con un helper `filler`
  que varía la longitud del mensaje por ronda, honrando la invariante
  real del motor (el historial solo crece dentro de un turno).
- **F4 (remap de índice sin id en OpenRouter)**: `accumulate_tool_call_fragment`
  ahora también desplaza el slot cuando un fragmento reanuncia `name`
  sobre un slot cuyo `arguments_buf` ya parsea como JSON completo (la
  llamada previa "se ve terminada"), no solo cuando cambia el `id` — el
  mismo criterio "anuncio sobre llamada terminada" que ya usa la ruta
  sin índice, con la guarda extra de completitud del buffer para no
  desplazar una continuación genuina a mitad de valor.
- **D5 (narración cross-turn sin acción)**: nuevo campo
  `Engine::consecutive_turns_without_tool_calls` (persiste entre
  llamadas a `run_turn` sobre la misma instancia) — tras
  `NARRATION_WITHOUT_ACTION_THRESHOLD` (2) turnos consecutivos que
  terminan en texto plano sin ningún tool call, el turno siguiente
  inyecta un `UserMessage` recordatorio adicional junto al mensaje real
  del usuario. Un booleano local `any_tool_calls_this_turn` asegura que
  un turno que sí llama una tool (aunque termine con texto en la última
  ronda) resetee el contador en vez de seguir sumando.
- **F6 (el nudge miente tras una mutación intermedia)**: nueva constante
  `MUTATING_TOOL_NAMES` (`write_file`/`edit_file`/`shell_exec`) —
  `dispatch_tool_calls` limpia todo `seen_calls` cuando una de estas
  tools completa con éxito, así que un `read_file(x)` repetido tras un
  `write_file(x)` intermedio se re-despacha de verdad en vez de quedar
  bloqueado por el nudge con una afirmación ("el resultado no cambió")
  que ya no es cierta.
- **F7 (el rescate mutila el plan del planner)**: `complete_once_with`
  gana un parámetro `rescue_enabled: bool` — el ejecutor sigue usando
  `self.textual_rescue_enabled`, pero `attempt_planning_round` siempre
  pasa `false`, así que un planner que emite su propio tool-template
  nativo (p.ej. `<tool_call>` de Qwen) mientras nombra la tool que usaría
  (justo lo que pide `planning_system_prompt`) sobrevive como texto del
  plan en vez de que el rescate se lo extraiga.

15 tests de regresión nuevos: `braze-model::escalation` (9: 7 de F3/D3 +
2 de D4, más 2 tests preexistentes reescritos con message-length
variable para reflejar la invariante real del motor), `braze-model::openrouter_wire`
(2, F4 en su variante de ataque y su variante de seguridad-contra-falso-positivo),
`braze-engine` (4: 1 de F6, 2 de D5, 1 de F7). Workspace completo verde
(`cargo test --workspace`), clippy limpio (`cargo clippy --workspace
--all-targets -- -D warnings`), `rustfmt` aplicado a los archivos
tocados.

Queda en el roadmap de la v3, no atacado en esta sesión: Grupo S
(ablación en braze-bench para el paper — E1, E4, E6, E3, E2, E5).

## Archivos críticos

- `/home/franciscoparrao/proyectos/braze/Cargo.toml` — manifiesto de workspace
- `/home/franciscoparrao/proyectos/braze/crates/braze-tools-core/src/provider.rs` — trait `ToolProvider`, contrato congelado
- `/home/franciscoparrao/proyectos/braze/crates/braze-model/src/backend.rs` — trait `ModelBackend`
- `/home/franciscoparrao/proyectos/braze/crates/braze-session/src/store.rs` — `SessionStore` + `ContextCompactor`
- `/home/franciscoparrao/proyectos/braze/.github/workflows/ci.yml` — espejo de `/home/franciscoparrao/proyectos/datacube-rs/.github/workflows/ci.yml`

Referencia de patrón async (aunque ya no se usa como frontera aislada, dado que el workspace es 100% async — sigue siendo útil como ejemplo de manejo de runtime tokio compartido si algún crate necesitara exponer una API bloqueante hacia afuera en el futuro): `/home/franciscoparrao/proyectos/surtgis/crates/cloud/src/sync_api.rs`.
