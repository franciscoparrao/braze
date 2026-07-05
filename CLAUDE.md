# braze — motor agéntico genérico en Rust (experimento)

> **Estado:** Fase 5 completa (integración: `braze-engine` + `braze-cli`
> implementados y compuestos sobre los 7 crates de Fases 1-4). El binario
> `braze` (subcomandos `chat`/`run`) compila y compone el loop agéntico
> completo. Pendiente: Fase 6 (verificación adicional/review) y
> verificación manual end-to-end contra Anthropic/Ollama reales. Creado
> 2026-07-03.
> Ver `PLAN.md` para la arquitectura completa, el grafo de dependencias y el
> plan de implementación por oleadas.

## Qué es

Un CLI agéntico genérico ("coding agent" al estilo OpenAI Codex / Claude
Code) construido desde cero en Rust, como **ejercicio de experimentación**
explícito — no es un compromiso de producto. El alcance del MVP favorece un
sistema pequeño y funcional sobre cobertura exhaustiva.

## El gap que llena

No es un fork de `openai/codex` ni de `google-gemini/gemini-cli`. Es un
diseño híbrido: la infraestructura que Codex ya resuelve bien (sandboxing,
cliente MCP, abstracción de proveedor de modelo) combinada con principios de
cómo opera Claude Code que no están explícitos en la estructura de crates de
Codex — carga diferida de herramientas (solo nombres en contexto, schema
completo bajo demanda), compactación diferencial de contexto (estado
durable vs. ventana táctica), ejecución en background con notificación push
(no polling), y un modelo de permisos de dos capas.

## Alcance MVP

Ver `PLAN.md` § "Alcance del MVP" para el detalle completo. Resumen:
loop de tool-calling funcionando contra **Anthropic** (proveedor
principal), **Ollama local** (segundo implementador de `ModelBackend`,
gratis) y **OpenRouter** (tercer implementador, agregado 2026-07-05 —
API OpenAI-compatible, da acceso a modelos de terceros vía una única
cuenta/key); herramientas locales + cliente MCP implementando el mismo
trait `ToolProvider`; carga diferida de herramientas end-to-end;
persistencia de sesión a disco con compactación diferencial simple; capa
mínima de confirmación de permisos (sin sandboxing de SO todavía).

Diferido a Fase 2: sandboxing Landlock/seccomp, multi-agente/grafo de
threads, observabilidad OTLP, paquetes de skills cargables, sistema de
hooks plugueable.

**TUI (`braze-tui`)**: implementada completa (2026-07-05, PLAN.md §
"Fase TUI — diseño", 5 oleadas) — `braze chat --tui`, opt-in. Viewport
inline + scrollback nativo, streaming markdown con gateo por fence,
tool-call cells, approval overlay real con interrupción por Esc, status
bar, snapshot tests. `--tui` sigue siendo opt-in, no el default; ver
PLAN.md § "fase TUI 2" para lo diferido (pager overlay, temas, promover a
default).

## Arquitectura

Workspace de 12 crates (`crates/braze-*`, incluye `braze-tui`), grafo de
dependencias en niveles — ver `PLAN.md` para el diagrama y las firmas de
los tres traits que actúan de contrato congelado: `ToolProvider`
(`braze-tools-core`), `ModelBackend` (`braze-model`),
`SessionStore`/`ContextCompactor` (`braze-session`).

**Desviaciones deliberadas de la convención del resto del ecosistema Rust
del autor** (datacube-rs, geostat-rs, swarm-abm son 100% sync + rayon):
- `tokio` en todo el workspace (dominio de I/O concurrente: streaming del
  modelo, cliente MCP, tareas en background).
- `tracing`/`tracing-subscriber` en los crates del loop agéntico (el modo
  de falla central — "la secuencia de tool-calls hizo algo inesperado" —
  es invisible sin trazas estructuradas, a diferencia de las librerías
  numéricas donde la corrección se valida con tests).

Todo lo demás sigue la convención habitual: `thiserror` v2, `clap` v4,
`serde`+`serde_json`, licencia dual `MIT OR Apache-2.0`, archivo-por-módulo.

## Estado del código (2026-07-03)

Los 11 crates del workspace tienen lógica real (no placeholders):
`braze-types`, `braze-events` (con `AgentEvent::AssistantToolCall`,
agregado en Fase 5), `braze-config` (con `anthropic_model`/`ollama_model`,
agregados en Fase 5), `braze-permissions`, `braze-session`,
`braze-tools-core`, `braze-model`, `braze-tools-local`, `braze-mcp-client`,
`braze-engine` (el loop agéntico: `Engine::run_turn`) y `braze-cli` (el
binario `braze`, subcomandos `chat`/`run`). `cargo build --workspace` y
`cargo test --workspace` verdes (170 tests + 1 doctest), `cargo clippy
--workspace --all-targets -- -D warnings` limpio.

## Próximos pasos al retomar

Ver `PLAN.md` § "Fases de Implementación" — Fase 6 (verificación adicional:
agente de tests, agente de review de código) y la verificación manual
end-to-end contra Anthropic/Ollama reales (PLAN.md § "Verificación
end-to-end (tras Fase 5)") — aún no ejecutada con credenciales reales.

## Modelos locales recomendados (Ollama)

El sweep real de `braze-bench` del 2026-07-04 (`crates/braze-bench/suites/default.toml`,
`--repetitions 5`, ver `docs/AUDITORIA-2026-07.md`) dejó dos hallazgos
concretos sobre qué modelos priorizar para desarrollo/benchmarking local:

- **No todo modelo instruct genérico soporta tool-calling en Ollama.**
  `gemma3:1b` falló 0/50 tareas instantáneamente (~185ms promedio) porque
  Ollama devuelve HTTP 400 "does not support tools" para ese modelo — no es
  un problema de capacidad de razonamiento, es incompatibilidad de API.
  Verificar `ollama show <modelo>` o probar una tarea trivial con tools
  antes de asumir que un modelo nuevo funciona con `braze`.
- **Modelos orientados a function-calling superan claramente a los
  genéricos del mismo tamaño.** `qwen2.5:3b` y `qwen2.5:7b` (ambos con
  soporte de tools nativo) alcanzaron 62-64% de pass rate en el sweep;
  `llama3.2:1b` (soporta tools, pero es un modelo pequeño genérico) quedó
  en 18%. Para evaluar `braze` con modelos locales, priorizar modelos ya
  fine-tuneados para tool-calling (p.ej. la familia Qwen2.5/3.5 orientada a
  function-calling, xLAM-2-3B, Hammer, Nemotron Nano) por sobre variantes
  instruct genéricas del mismo tamaño (Llama3.2, Gemma3) — técnica G6 del
  roadmap de la auditoría 2026-07.

Esto no cambia el default de `braze-config` (`ollama_model = "llama3.1"`,
`crates/braze-config/src/config.rs`) — queda como criterio para elegir qué
modelo configurar/tener disponible localmente, no como un cambio de código.
