# braze — motor agéntico genérico en Rust (experimento)

> **Estado:** SCAFFOLD (Fase 1 completa: workspace + contratos congelados,
> sin lógica implementada aún). Creado 2026-07-03.
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
loop de tool-calling funcionando contra **Anthropic** (proveedor principal)
y **Ollama local** (segundo implementador de `ModelBackend`, gratis);
herramientas locales + cliente MCP implementando el mismo trait
`ToolProvider`; carga diferida de herramientas end-to-end; persistencia de
sesión a disco con compactación diferencial simple; capa mínima de
confirmación de permisos (sin sandboxing de SO todavía).

Diferido a Fase 2: sandboxing Landlock/seccomp, multi-agente/grafo de
threads, TUI, observabilidad OTLP, paquetes de skills cargables, sistema de
hooks plugueable.

## Arquitectura

Workspace de 11 crates (`crates/braze-*`), grafo de dependencias en 5
niveles — ver `PLAN.md` para el diagrama y las firmas de los tres traits
que actúan de contrato congelado: `ToolProvider` (`braze-tools-core`),
`ModelBackend` (`braze-model`), `SessionStore`/`ContextCompactor`
(`braze-session`).

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

Scaffold inicial: `Cargo.toml` de workspace + 11 manifiestos de crate,
tipos compartidos completos (`braze-types`), `AgentEvent`+`TaskNotifier`
completos (`braze-events`), y las firmas congeladas de `ToolProvider`
(`braze-tools-core`), `ModelBackend` (`braze-model`) y
`SessionStore`/`ContextCompactor` (`braze-session`). El resto de los crates
(`braze-config`, `braze-permissions`, `braze-tools-local`,
`braze-mcp-client`, `braze-engine`, `braze-cli`) son placeholders vacíos —
implementación por oleadas de subagentes, ver `PLAN.md` Fases 2-5.

## Próximos pasos al retomar

Ver `PLAN.md` § "Fases de Implementación" — Fase 2 (Nivel 0:
`braze-config`), Fase 3 (Nivel 1: `braze-permissions`, `braze-session`,
`braze-tools-core`, `braze-model`), Fase 4 (Nivel 2: `braze-tools-local`,
`braze-mcp-client`), Fase 5 (integración: `braze-engine` + `braze-cli`).
