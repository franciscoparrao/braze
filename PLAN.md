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
| `braze-mcp-client` | Cliente MCP sobre `rmcp` (SDK oficial), implementa `ToolProvider`, expone nombres primero y schemas bajo demanda | 2 | tools-core, types |
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

Sin ciclos: `braze-engine` es el único crate que conoce simultáneamente `tools-local` y `mcp-client` (hermanos, nunca dependen entre sí — eso es lo que mantiene `ToolProvider` como un seam válido). `braze-model` y `braze-tools-core` nunca dependen entre sí.

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

### Fase 5: Integración (orquestador, secuencial)
- [ ] `braze-engine`: composición de todos los traits anteriores. No paralelizar — es quien reconcilia todos los contratos a la vez.
- [ ] `braze-cli`

### Fase 6: Verificación (subagentes)
- [ ] Agente de tests: `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Agente de review de código
- [ ] Verificación manual end-to-end (ver sección siguiente)

## Verificación end-to-end (tras Fase 5)

1. `cargo build --workspace` compila sin warnings.
2. `braze run "lista los archivos en /tmp" --backend ollama` — corre contra Ollama local (gratis), confirma que el loop de tool-calling + `braze-tools-local` funcionan sin tocar la API de pago.
3. `braze run "..." --backend anthropic` — mismo prompt contra Anthropic real, confirma que `ModelBackend` no es un one-off atado a un solo proveedor.
4. Conectar un servidor MCP de prueba (p. ej. `@modelcontextprotocol/server-filesystem` vía stdio) y verificar en logs (`RUST_LOG=braze=debug`) que solo se listan `ToolStub` (nombres) hasta que el modelo intenta invocar una tool específica — ahí debe verse la resolución de schema bajo demanda.
5. Disparar una acción de la tabla de confirmación (ej. pedirle que borre un archivo fuera del cwd) y verificar que el prompt y/n intercepta antes de ejecutar.
6. Matar el proceso a mitad de sesión y verificar que `braze chat --resume <session-id>` recupera el historial desde el rollout log en disco.

## Archivos críticos

- `/home/franciscoparrao/proyectos/braze/Cargo.toml` — manifiesto de workspace
- `/home/franciscoparrao/proyectos/braze/crates/braze-tools-core/src/provider.rs` — trait `ToolProvider`, contrato congelado
- `/home/franciscoparrao/proyectos/braze/crates/braze-model/src/backend.rs` — trait `ModelBackend`
- `/home/franciscoparrao/proyectos/braze/crates/braze-session/src/store.rs` — `SessionStore` + `ContextCompactor`
- `/home/franciscoparrao/proyectos/braze/.github/workflows/ci.yml` — espejo de `/home/franciscoparrao/proyectos/datacube-rs/.github/workflows/ci.yml`

Referencia de patrón async (aunque ya no se usa como frontera aislada, dado que el workspace es 100% async — sigue siendo útil como ejemplo de manejo de runtime tokio compartido si algún crate necesitara exponer una API bloqueante hacia afuera en el futuro): `/home/franciscoparrao/proyectos/surtgis/crates/cloud/src/sync_api.rs`.
