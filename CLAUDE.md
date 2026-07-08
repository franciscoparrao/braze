# braze — motor agéntico genérico en Rust (experimento)

> **Estado (2026-07-06):** MVP completo y en reorientación **"maestro en
> modelos pequeños"** — el harness como variable que compensa la escala
> del modelo (respaldo: SWE-agent/ACI y el TR de Qwen3-Coder-Next, ver
> `docs/SOTA-2026-07.md`). Fases 1-5 + TUI (2 fases) + auditorías v1/v2
> cerradas; split planificador/ejecutor implementado con **veredicto A/B
> negativo** (queda opt-in, ver PLAN.md); backlog rankeado
> post-revisiones OSS (ítems 1-7) **completo**. Creado 2026-07-03.
> Ver `PLAN.md` para la arquitectura, el grafo de dependencias, y una
> sección por cada incremento con su verificación.

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

**TUI (`braze-tui`)**: completa con fase 2 incluida (2026-07-05/06) —
`braze chat --tui`, opt-in (promoverla a default sigue diferido).
Viewport inline + scrollback nativo, streaming markdown con gateo por
fence, tool-call cells, approval overlay, slash commands con popup
(`/help`, `/model`, `/quit`), @-menciones, Ctrl+T (output completo de la
última tool call), backtrack Esc-Esc, temas (dark/light/high-contrast),
y **`/model` picker** para cambiar de backend/modelo a mitad de sesión
(rebuild del Engine + mismo session id; candidatos = backends
configurados + modelos instalados en el server Ollama).

**Palancas de confiabilidad para modelos chicos** (backlog 1-7,
2026-07-06 — cada una con su sección en PLAN.md): rescate textual de
tool calls por familia (`<tool_call>{json}` de qwen2.5, gramática XML
`<function=...>` de qwen3-coder, JSON desnudo); colapso ACI de
observaciones viejas a 1 línea salvo las últimas 5; guardrail `cargo
check` post-edit (errores de compilación de vuelta al modelo en el
mismo tool result; opt-out `disable_post_edit_check`); escalación
reactiva lead/worker estilo Goose (`--lead <backend>[:modelo]`,
decorator `EscalatingBackend` — compone con `--planner`); knobs de
sampling Ollama (`--top-p/--top-k/--repeat-penalty` en braze-bench);
hardening del ensamblaje de tool calls en streaming (escalera de
reparación de args compartida, remap de colisiones index/id, fragmentos
sin index — los wires nunca dropean una call en silencio); y
`--ollama-url` en chat/run para apuntar a un nodo de inferencia LAN.

## Arquitectura

Workspace de 13 crates (`crates/braze-*`, incluye `braze-tui` y
`braze-bench`), grafo de
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

## Estado del código (2026-07-06)

Los 13 crates tienen lógica real y verificada. `cargo build/test/clippy
--workspace` verdes: **612 tests**, clippy `-D warnings` limpio. La
convención de verificación del proyecto: cada incremento se prueba
también **en vivo** (pty scripteado contra el binario real para la TUI,
sweeps de braze-bench contra modelos reales, smoke contra APIs reales) —
compilar ≠ funcionar. Técnica pty reusable: responder `ESC[6n`,
`wait_for` con offsets, asserts sobre celdas commiteadas al scrollback
(no sobre el render diffeado), y detectar salida con `waitpid` (no
`kill(pid, 0)` — el zombie responde como vivo).

## Benchmarking: en Nitro, no en la máquina de trabajo

Regla operativa (2026-07-06, evidencia en PLAN.md): los sweeps de
`braze-bench` corren contra el nodo LAN **Nitro**
(`BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434` + `--no-ollama-stop`;
la IP es DHCP — fijarla en el router sigue pendiente). Benchear en la
máquina de trabajo con builds/tests concurrentes contamina los números
(misma config+seed: 2/6 vs 0/6), y Nitro corre qwen2.5:3b ~50× más
rápido (~2s vs ~90-100s por tarea). `RUST_LOG=braze_engine=info` sobre
un sweep muestra las activaciones de palancas del engine (rescates,
compactación) — braze-bench instala subscriber de tracing.

## Próximos pasos al retomar

- Iteración pre-registrada del planner (opcional, PLAN.md): descartar
  planes de un solo paso y/o render con rol user; si no mueve
  multi_step/error_recovery, remoción completa.
- A/B del `EscalatingBackend` en braze-bench (falta sintaxis de spec con
  lead, anotado en `docs/SOTA-2026-07.md`).
- Paper ángulo A (EMS/ESIN): la curva harness-vs-escala por skill, con
  qwen3.5-coder 6/6 vs qwen2.5:3b 0-2/6 en las skills débiles como
  contraste central; suite ampliada de tareas de edición pendiente.
- Infra Nitro: IP fija en el router, `OLLAMA_KEEP_ALIVE`.
- Circuit breaker por costo acumulado por turno (idea de
  `@openrouter/agent`'s `maxCost(amount)` stop condition,
  docs/usability-log-2026-07-07-si2.md): `MAX_TURN_ITERATIONS` corta por
  cantidad de rondas pero no por gasto — un turno de investigación puede
  acumular cientos de miles de tokens sin que nada lo frene antes (caso
  real: 481K tokens de entrada en un turno de 40 rondas, sesión
  `ccd4621b`). Con `cache_read_tokens`/`cache_write_tokens` ya fluyendo
  por `AgentEvent::Usage` (prompt-caching, cerrado hoy), agregar un tope
  de costo acumulado sería una extensión barata. No diseñado todavía.

## Modelos locales recomendados (Ollama)

**El mejor modelo local del proyecto es `qwen3.5-coder` corriendo en
Nitro** (sweep 2026-07-06, datos en `docs/sweep-nitro-sampling-2026-07-06/`):
**6/6 en `g10-weak-skills`** a temp 0.2 — primer modelo local que satura
las skills débiles (error_recovery + distractor_selection), ~20-27s por
tarea en Nitro. Caveat: es *thinking model* — Ollama devuelve el
razonamiento en un campo `thinking` separado y con `num_predict` chico el
content puede salir vacío; presupuestar tokens. Para la familia Qwen
**chica** (qwen2.5:3b), el sampling recomendado por Qwen (temp 0.7 /
top_p 0.8 / top_k 20 / repeat_penalty 1.05, flags de braze-bench) rinde
mejor que el 0.2 default del bench (0/6 → 2/6 en g10, direccional en dos
entornos).

El sweep del 2026-07-04 (`default.toml`, `--repetitions 5`, ver
`docs/AUDITORIA-2026-07.md`) dejó además dos hallazgos vigentes sobre qué
modelos priorizar:

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

## Modelo recomendado vía OpenRouter (`deepseek/deepseek-v4-flash`)

Sweep de `braze-bench` del 2026-07-05 (5 repeticiones, mismo suite
`default.toml`) contra `openrouter:deepseek/deepseek-v4-flash` — datos
crudos en `docs/sweep-deepseek-v4-flash.json`/`.log`:

- **49/50 pass rate** (±5pp), 2.4 rondas promedio, ~6.6s de latencia
  promedio por tarea, 0 fallos de validación de schema, 0 fallos de
  ejecución, 0 denegaciones de permiso. Perfecto en 4 de 5 skills
  (`no_tool`, `multi_step`, `error_recovery`, `distractor_selection`);
  29/30 en `single_tool`.
- **Verificado también en vivo con la TUI** (`braze chat --tui --backend
  openrouter --model deepseek/deepseek-v4-flash`, 2026-07-05): streaming
  fluido, tool calls reales (`write_file`) renderizando correctamente.
  Notablemente más rápido en la práctica que Ollama local en esta
  máquina — inferencia en la nube vs. CPU local, no una diferencia de
  arquitectura de `braze`.
- Buen default de bajo costo para probar `braze` sin la latencia CPU-bound
  de un modelo local ni el costo de un modelo de frontera — recomendado
  como primera opción al evaluar cambios rápidos o hacer demos.

Esto no cambia el default de `braze-config` (`ollama_model = "llama3.1"`,
`crates/braze-config/src/config.rs`) — queda como criterio para elegir qué
modelo configurar/tener disponible localmente, no como un cambio de código.
