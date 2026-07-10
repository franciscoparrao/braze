# Qué le vendría bien a braze de OpenCode

Fecha: 2026-07-08
Autor: generado por OpenCode (glm-5.2) revisando su propio diseño contra el estado de braze post-v5

## Resumen ejecutivo

OpenCode y braze atacan el mismo dominio (motor agéntico de código) con tesis opuestas: OpenCode es un agente coding maduro (~160k stars, SDK AI-native, runtime TS/TS) pensado para modelos de frontera; braze es un ejercicio SLM-first ("el harness compensa la escala del modelo") escrito en Rust. Eso los hace complementarios: OpenCode ya resuelvió problemas de extensibilidad, configuración y observabilidad que braze tiene abiertos en `docs/AUDITORIA-2026-07-v5.md`, y braze ya resuelve problemas de tolerancia a fallos (rescates textuales por familia, escalación lead/worker, compaction diferencial) que OpenCode no documenta.

Este reporte cruza el schema JSON de `https://opencode.ai/config.json` con los 41 ítems abiertos del backlog v5 y propone 10 features de OpenCode que cerrarían hallazgos o desbloquearían trabajo en braze, ordenadas por valor/esfuerzo. Incluye 5 hallazgos nuevos inspirados por el schema que no estaban flagueados en v1-v5.

## Cómo se leyó OpenCode

Fuentes:

- `webfetch https://opencode.ai` (landing + features publicadas).
- `webfetch https://opencode.ai/config.json` (JSON Schema authoring, ~1100 líneas, cubre `Config`, `AgentConfig`, `ProviderConfig`, `McpLocalConfig`/`McpRemoteConfig`, `PermissionConfig`, `compaction`, `experimental`, `tool_output`).
- `~/.config/opencode/opencode.jsonc` del usuario (3 subagents custom, 13 commands custom, 3 references, permissions declarativas, 3 MCP servers locales).
- El skill customizer `customize-opencode` del propio runtime (cubrió agents/commands/skills/plugins/permissions/MCP).

Lo que NO se inspeccionó (fuera de alcance de un fetch público): el source TypeScript de los built-in agents (`build`, `plan`, `general`, `explore`, `compaction`, `title`, `summary`), el runtime del loop agéntico, las internals del sistema de snapshots. Las conclusiones se basan en el contrato público (config schema + comportamiento observable).

## Features de OpenCode que le vendrían bien a braze

### 1. `AgentConfig.steps` — `max_turn_iterations` por agente (cierra v4 P0.2 parcial)

**OpenCode**: cada agente declara `steps: integer` en su frontmatter. Es el tope de iteraciones agentic antes de forzar una respuesta text-only. En OpenCode además `maxSteps` quedó deprecated a favor de `steps` (mejor nombre). El campo ya está en el schema (https://opencode.ai/config.json `AgentConfig`).

**braze hoy**: `MAX_TURN_ITERATIONS = 20` hardcoded en `crates/braze-engine/src/engine.rs:31`. El único override existente es por `BrazeConfig::context_budget_tokens` (indirecto, escala el threshold de compaction pero no el cap de rondas).

**Lo que faltaba en v4 P0.2 / H-5**: un campo `max_steps` en `Config` (y `EngineBuilder::with_max_turn_iterations`), overrideable por backend/familia. Para braze SLM-first esto es especialmente valioso porque el número óptimo de rondas depende de la capacidad del modelo: qwen2.5:3b converge en ~3-5 rondas para tareas single-tool pero más fails_browser en 15-20; con `--planner` o `--lead` arranca un planner con su propio presupuesto de tokens (`PLANNER_MAX_TOKENS = 1024` hardcoded). Un `planner_max_steps: 2` configurable desacoplaría del executor.

**Arreglo**: copiar el campo en `braze-config::Config { max_turn_iterations: u32 }` (default 20) + `EngineBuilder::with_max_turn_iterations`; bajar `PLANNER_MAX_TOKENS` al mismo mecanismo. Cierra la mitad de v4 P0.2 (rondas). El costo-driven (tokens/usd) del TurnBudget sigue abierto: ese necesita pricing-table aparte (ver #8 abajo).

**Refs**: docs/AUDITORIA-2026-07-v4.md P0.2; docs/AUDITORIA-2026-07-v5.md H-4.

---

### 2. `compaction.tail_turns` + `prune` + `preserve_recent_tokens` + `reserved` — 4 knobs de compactación (cierra v3 B1/B2, v4 P2.4)

**OpenCode schema**:

```jsonc
"compaction": {
  "auto": true,                  // enable/disable
  "prune": false,                // poda viejos tool outputs a 1-línea
  "tail_turns": 2,               // turns user recientes verbatim
  "preserve_recent_tokens": N,   // tope tokens de turnos recientes preservados verbatim
  "reserved": N                  // buffer de tokens para no overflow al compactar
}
```

**braze hoy**: compaction tiene 4 constantes hardcoded en `engine.rs` y `history.rs`:

- `DEFAULT_TACTICAL_COMPACTION_THRESHOLD = 40` (eventos).
- `TACTICAL_FULL_OBSERVATIONS = 5` (observaciones full).
- `MAX_FULL_OBSERVATIONS_TOTAL_CHARS = 8_000`.
- `NO_CONTEXT_BUDGET_SCALE_MULTIPLIER = 10` (escalado cuando no hay budget).

El WIP actual (U-17/U-18) hace el escalado-aware de `context_budget_tokens`, pero todo sigue siendo constantes internas. `prune` ya existe en braze como el "colapso ACI" del commit 5509c11 (item 4 del backlog 2026-07-06) — pero **no tiene knob de apagado**, está siempre-on. v3 E1 pide ablation para medirlo; OpenCode expone `prune: bool` perfectamente.

**Lo que cierra**:

- v3 B1 (cap agregado de observaciones = siempre 5): `tail_turns` o `preserve_recent_tokens` da el control de "cuántos turnos recientes preservar verbatim" que B1 pide.
- v3 B2 (estimador mide táctica en crudo, no colapsada): `reserved` explicita el buffer en vez de derivarlo de `chars/4` implícito.
- v4 P2.4 (límites hardcodeados): 3 de los 4 limites de compaction expuestos de golpe.
- v3 E1 (ablation infra en bench): `+ablate:no-prune` directo desde config, sin añadir nada al parser de ablation (`prune: false` en el SamplingSpec).

**Arreglo**: añadir `BrazeConfig::CompactionConfig { auto: bool, prune: bool, tail_turns: u32, preserve_recent_tokens: Option<u32>, reserved: u32 }` en `braze-config`, reemplazando las constantes internas de `Engine`. El hook de `+ablate:` ya existe en `BackendSpec` — `prune: false` es trivial de wirear.

**Refs**: docs/AUDITORIA-2026-07-v3.md Grupo P (B1, B2); docs/AUDITORIA-2026-07-v4.md P2.4.

---

### 3. `tool_output.max_lines` + `max_bytes` — 2 knobs de truncado (cierra v4 P2.4)

**OpenCode**:

```jsonc
"tool_output": { "max_lines": 2000, "max_bytes": 51200 }
```

`max_lines` controla líneas; `max_bytes` controla bytes. Ambos umbrales: cuando output excede cualquiera, se guarda a disco y se devuelve preview.

**braze hoy**: `MAX_TOOL_OUTPUT_BYTES = 8_000` hardcoded en `provider.rs` (v5 WIP lo hizo `pub(crate)`). No hay `max_lines`. El trailer "narrow your query" / "more lines below" construido en `truncate_output` es por bytes solito.

**Lo que cierra**:

- v4 P2.4: límites hardcodeados → exponer en config.
- v3 B1: el cap de 8KB es por observación, pero algunos outputs (logs multi-GB, datasets) tienen 1 línea > 8KB. Un `max_lines = 5000` truncaría sin caerse en el "narrow your query" teaser engañoso para grep-heavy outputs.
- AD: ya hay WIP en `read_file.rs` (`clamp_to_output_budget`) que distingue între truncate-by-bytes y truncate-by-lines. Exponer los 2 knobs le da al modelo una palanca extra.

**Arreglo**: `BrazeConfig::ToolOutputConfig { max_bytes, max_lines }`, pasada al `LocalToolsProvider::with_output_budget`. El acompañamiento post-truncate trailer del WIP actual ya distingue contexto; sólo necesita leer desde config.

**Refs**: docs/AUDITORIA-2026-07-v5.md H-1 (WIP incompleto antes del cierre); v3 B1; v4 P2.4.

---

### 4. `formatter` per-extension — resuelve v4 P1.6 (post-edit check Rust-only)

**OpenCode schema**:

```jsonc
"formatter": false | true | {
  "rust":  { "command": ["rustfmt", "--edition", "2024"], "extensions": [".rs"] },
  "py":    { "command": ["ruff", "format"],                "extensions": [".py"] },
  "ts":    { "command": ["prettier", "--write"],           "extensions": [".ts", ".tsx"] }
}
```

Cada entrada es `{ command, extensions, environment, disabled }`. El mapping extensión → formatter es declarativo.

**braze hoy**: `crates/braze-tools-local/src/post_edit_check.rs:41`:

```rust
if Path::new(path).extension().is_none_or(|ext| ext != "rs") { return Ok(None); }
```

Hardcodeado a `.rs` + `cargo check`. v4 P1.6 lo flaguea explícitamente: "un motor SLM-first general necesita feedback barato y automático para cualquier stack".

**Lo que cierra**:

- v4 P1.6: generalización a cualquier stack.
- Sin costo de diseño: el shape de OpenCode es exacto. `Command + extensions + environment + disabled` mapea 1:1 a un `BrazeConfig::FormatterConfig` HashMap.
- v3 D7 (system prompt y feedback en inglés while modelo habla español): un formatter por extensión puede tener mensajes localizados o un mensaje de error sanitized para SLMs.

**Arreglo**:

```rust
pub struct FormatterConfig {
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub environment: Option<HashMap<String, String>>,
    pub disabled: bool,
    pub timeout_secs: u64,
}
// Config::formatters: HashMap<String, FormatterConfig>
```

El `post_edit_check.rs` se generaliza a "find formatter by extension, run command, surface output".

**Refs**: docs/AUDITORIA-2026-07-v4.md P1.6; docs/AUDITORIA-2026-07-v5.md HF listaHistorial H-7.

---

### 5. `permission` declarativo por-patrón con insert-order "last matching wins" (cierra v5 H-6 + v4 P2.3)

**OpenCode**:

```jsonc
"permission": {
  "bash": { "git *": "allow", "rm *": "deny", "*": "ask" },
  "edit": { "*.rs": "allow", ".env": "deny", "*": "ask" },
  "external_directory": { "~/secrets/**": "deny", "*": "allow" }
}
```

Insert order importa. Último match gana. Cada tool puede tener acción plana (`"ask"`) o sub-objeto `{pattern: action}`. Permite denegar patrones específicos sin tocar código, incluyendo el escape de H-6 (`"env": "deny"` imposible hoy, Reversible blanket en `classifier.rs:74`).

**braze hoy**: `DefaultClassifier` en `crates/braze-permissions/src/classifier.rs` con:

- `WorkdirAllowlist` (paths).
- `safe_readonly_commands` (`env`, `ls`, `cat`, ... hardcoded).
- `is_safe_git`, `is_safe_find`, `is_safe_env` (funciones match-pattern).

Para añadir/quitar un comando, hay que editar Rust. H-6 (`env` solo filtra secrets) está literalmente bloqueado sin refactor de classifier.

**Lo que cierra**:

- v5 H-6: `env` leak → `"bash": { "env": "deny", "env *": "deny", "*": "ask" }` un par de líneas en config.
- v4 P2.3 (MCP taxonomía fina): `mcp.readOnlyHint` se puede mapear a `permission.mcp: { "readonly_*": "allow", "*": "ask" }`.
- v3 Grupo O (A4): steering de edit_file→write_file sería una regla `permission.edit_file = "deny"` cuando el archivo no existe — no necesita código.
- v4 P0.3 (preflight write_file): `permission.write_file = { "*.shrink": "ask" }` o un patrón preflight.
- v4 P2.4 (todos los límites hardcodeados): `permission` cubre el espacio de "quién puede hacer qué", que es exactamente la clase de cosas que estaban hardcoded.

**Diferencia con braze que preservar**: braze tiene `AlwaysIrreversibleClassifier` (modo `--supervised`). Eso es un preset equivalente a `permission: "ask"` blanket. Pero la implementación OpenCode es más barata: `permission.ask` es un fallback default. Lo único que braze preserva mejor es `RememberKey` (persistencia de aprobaciones por categoría entre turnos) — eso sigue siendo propio y valioso.

**Arreglo**: un `PermissionConfig` análogo en braze-config, parseo a `Vec<Rule>` evaluado antes de `DefaultClassifier`. Composición: si `PermissionConfig` matchea, gana; si no, cae al `DefaultClassifier` actual (preserva el código existente sin migración). Esto permite hacerlo incrementalmente sin romper tests.

**Refs**: docs/AUDITORIA-2026-07-v5.md H-6; docs/AUDITORIA-2026-07-v4.md P2.3, P0.3; docs/AUDITORIA-2026-07-v3.md Grupo O (D7).

---

### 6. `ProviderConfig.options.chunkTimeout` — SSE chunk timeout (cierra un gap no flagueado en v5)

**OpenCode schema**:

```jsonc
"provider.options.chunkTimeout": integer
// "Timeout in milliseconds between streamed SSE chunks for this request.
//  If no chunk arrives within this window, the request is aborted."
```

**braze hoy**: `http_client.rs` configura `connect_timeout=10s` + `read_timeout=600s` (reset tras cada read exitoso). El `read_timeout=600s` es "600s sin NINGÚN byte" — un stream SSE que envía 1 byte cada 599s pasa el check y colgaria el turno indefinidamente. No hay `chunkTimeout` análogo.

**El gap no flagueado en v5**: v5 H-5 es "shell_exec sin timeout de pared" — el análogo para el streaming del modelo NO estaba flagueado ni en v4 v5. OpenCode lo trae a la superficie.

Caso real: Anthropic/OpenRouter/Ollama en una red inestable. Una connection HTTP/2 que se cuelga mid-stream (server side) manda keepalive TCP pero no chunks — para braze, el stream nunca termina (`Done` nunca llega), y el `read_timeout=600s` cuenta desde el último byte recibido, que es 0 hoy si el stream nunca se inició. `chunkTimeout=30s` mataría la request a los 30s sin chunk.

**Lo que añade a v5 backlog**:

- **NEW H-26 [ALTA]** — `chunkTimeout` para todos los backends, no sólo OpenRouter. v1 `A3` (stream truncation→respuesta final) cerró el caso "conección cerrada antes de `Done`", pero el caso "conección colgada" (sin fin ni cierre) no se testeó. El `chunkTimeout` de OpenCode es la mecanica de defense correcta.

**Arreglo**: `reqwest::ClientBuilder` con `tcp_keepalive(30s)` + un watchdog `tokio::time::timeout_at(last_chunk_at + chunk_timeout)` en `complete_once_with`. Default de OpenCode está ausente pero `chunk_timeout=30000` (30s) es razonable.

**Refs**: docs/AUDITORIA-2026-07-v2.md A3 (closed); v5 H-5 (análogo para shell_exec); NEW gap para modelo streaming.

---

### 7. `skill` como tool-permission + skill loader roaming (`~/.claude/skills`, `~/.agents/skills`)

**OpenCode**:

- Habilidad de **declarar `permission.skill: "allow"` como cosa de primer nivel** — skill-loading es una tool con permiso.
- Loader roaming: `skills.paths: [".opencode/skills", "/abs/path"]` y `skills.urls: ["https://.../.well-known/skills/"]` (URLs!). Auto-discovery por `**/SKILL.md` con frontmatter `{name, description}`.
- El usuario de este entorno ya tiene 116 skills en `~/.claude/skills/` y `~/.agents/skills/` — cargables por OpenCode automáticamente.
- Las skills son **declarativas, no código**: documentación markdown que ensancha el system prompt cuando el nuestro la pide.

**braze hoy**: `braze-skills` está en Fase 2 diferida (PLAN.md). No existe el loader. Las "instrucciones" del agente son el `system_prompt` static en `braze-config/src/prompt.rs`.

**Por qué es valioso para SLM-first** (no es un nice-to-have):

- Un SLM se beneficia más que un frontier de instrucciones específicas por familia. Las skills son exactamente "instrucciones específicas cargadas on-demand". El usuario tiene 116 skills escritas; si braze las carga con el mismo loader, gana:
  - `paper-review-ijgis` calibraría el comportamiento de `braze run "revisa este manuscrito según ijgis"` sin reescribir el system prompt.
  - `surtgis`, `memoria`, `paper-figures-*` serían directamente reutilizables.
- El costo en tokens es bajo: skills van al contexto sólo on-demand, igual que `ToolRegistry::resolve_schema`.

**Lo que suma**:

- `permission.skill: "allow" | "ask" | "deny"` como primer-class, igual que tools — el usuario puede configurar "sin skills" para ambiente contenido.
- `skills.paths: Vec<PathBuf>` y `skills.urls: Vec<Url>` en `BrazeConfig` — URLs permite cargar skills para un experimento desde un repo GitHub.

**Arreglo**: este es más caro que #1-5 porque implica un skill registry. Pero el formato `SKILL.md` es trivial de parsear en Rust (frontmatter YAML + body markdown). Un `crate::braze-skills` nuevo que implementa `ToolProvider` análogo a `McpToolProvider` es factible. `SkillRef { name, description, source, content_path }` es básicamente `ToolStub` + un contenido body.

**Refs**: docs/SOTA-2026-07.md (cuando se discute OpenCode como caso); CLAUDE.md "Skills disponibles (97 + 19 = 116)"; AGENTS.md OpenCode § "Skills".

---

### 8. `experimental.policies` — policies declarativas para uso de provider (cierra v4 P1.2 parcial + v4 P0.2 costo)

**OpenCode schema**:

```jsonc
"experimental.policies": [
  { "action": "provider.use", "effect": "allow", "resource": "anthropic/*" },
  { "action": "provider.use", "effect": "deny",  "resource": "openai/*" }
]
```

Policy first-class. `action: "provider.use"` (el único por ahora), `effect: "allow" | "deny"`, `resource: pattern`. Permite gobernar "qué agentes pueden usar qué providers" sin código.

**braze hoy**: `EngineBuilder::with_lead` y `with_planner` toman `Box<dyn ModelBackend>` arbitrario. No hay restricción de "el lead siempre anthropic, el executor siempre ollama". `brazeíveis config.lead_backend` y `planner_backend` son strings libres, sin validación.

**Lo que suma**:

- v4 P1.2 (ModelFamily compartido): si braze expone policies, la familia del modelo es información que alimenta la policy. `"anthropic/claude-*"` siempre permitido para lead; `"ollama/qwen2.5*"` siempre permitido para executor local; `"openrouter/z-ai/*"` sólo en `--supervised`. Declarativo en vez de código.
- v4 P0.2 (TurnBudget costo): las policies son el sitio natural para "aborta si el provider del turno superó X USD acumulado" — una policy con `action: "provider.cost"`, `resource: "anthropic/claude-sonnet*"`, `effect: "deny if > $5"` es expressible si braze añade pricing-table. Hoy el costo no se computa (v5 H-3).
- v3 D3 (escalación no distingue falla-modelo vs entorno): una policy podría `action: "escalation.allow"` con `resource: "anthropic/*"` mientras que para `ollama/*` se queda en default. Resuelve el caso "lead también falla" sin código (H-14).

**Arreglo**: un `BrazeConfig::PoliciesConfig: Vec<Policy>` evaluado en `Engine::build_request` antes de delegar a `ModelBackend`. La semantics tiene que estar bien pensada (qué resource matchea qué), pero el shape es directo.

**Refs**: docs/AUDITORIA-2026-07-v4.md P1.2, P0.2; docs/AUDITORIA-2026-07-v5.md H-14.

---

### 9. Plugin / hook surface (cierra Plan.md Fase 2 hooks + dreferred OTel)

**OpenCode hook surface** (plugins TS):

```
event(input)                       — catch-all, todo bus
config(cfg)                        — una vez en init, mutate merged config
chat.message, chat.params, chat.headers
tool.execute.before                — mutar output.args antes de tool
tool.execute.after
tool.definition                    — ajustar schema de tool
command.execute.before             — mutar prompt/template de command
shell.env                          — ajustar env de shell tool
permission.ask                     — override authorization
experimental.chat.messages.transform
experimental.chat.system.transform
experimental.session.compacting
experimental.compaction.autocontinue
experimental.text.complete
```

**braze hoy**: PLAN.md Fase 2 lista `braze-hooks` como un crate deferido. El hook-points son actualmente todos hardcoded en `engine.rs`:

- pre-compactación (`Engine::maybe_compact`).
- pre-dispatch (`Engine::dispatch_tool_calls`).
- post-tool (`Engine::complete_once_with`).
- summary fallback (`attempt_tools_free_summary_round`).
- streaming truncation (`provider.rs::truncate_output`).

Cualquier observabilidad o customization que un tercero quiera añadir requiere forkear braze y editar estos puntos.

**Lo que cierra**:

- Plan.md Fase 2 `braze-hooks`: el surface de OpenCode es el blueprint. Cada hook point de arriba mapea a un trait `EngineHook` con métodos `before_dispatch`, `after_dispatch`, `before_compact`, `after_compact`, `transform_messages`, etc. Implementaciones default no-op.
- OTel diferido: `tracing::instrument` ya está en todo el engine, pero sin exporter OTLP. Un plugin que escupe OTLP podría vivir como un `EngineHook::event` (análogo a OpenCode `event(input)`) sin tocar el crate engine.
- v5 H-3 (métricas de rescates/escalaciones/compaction no trackeadas como AgentEvent): un hook `event` catch-all permitiría a un plugin contar rescates desde fuera del engine, sin añadir variants a `AgentEvent` (que tiene impacto en el wire y tests). **Dependiendo del diseño, H-3 se resuelve más barato vía plugins que via variantes de enum**.
- v3 F3 (guardrail post-edit is_error:false; escalación no cuenta): un `tool.execute.after` que inspecciona `result.is_error` y el `rescue_parser_name` es exactamente el hook que resuelve la observabilidad perdida.

**Arreglo**: el trait `EngineHook` en `braze-engine` con 6 callbacks default no-op, `Engine::hooks: Vec<Box<dyn EngineHook>>`. Plugin crate `braze-hooks-otel` que implanta `EngineHook::event` y exporta OTLP. No es trivial (intentions: async, thread-safe con `Arc`), pero el diseño está dado.

**Refs**: PLAN.md § "Diferido a Fase 2"; docs/AUDITORIA-2026-07-v5.md H-3, H-14 (escalaciones sin test lead-fail).

---

### 10. `references` con descriptions que van al system prompt + `external_directory` auto-permitido (cierra v3 D7 parcial)

**OpenCode**:

```jsonc
"references": {
  "docs":       { "path": "../docs", "description": "Use for product behavior" },
  "effect-sdk": { "repository": "Effect-TS/effect", "description": "..." }
}
```

References con `description` son advertised al agente en el system prompt. El dir auto-permitido como `external_directory: "allow"` implícito. Las que son `hidden: true` no aparecen en TUI pero siguen available para el agente.

**braze hoy**: `WorkdirAllowlist::new(Vec<PathBuf>)` – imperativo. Sin concept de "directorio asociado a una descripción". El system_prompt know de `cwd` pero nada de dirs externos.

**Lo que suma para SLM-first**:

- Un SLM no sabe dónde buscar. Decirle "Aquí hay docs del API en ../docs" en el system prompt es un steering barato y potente.
- v3 D7 (system prompt y feedback en inglés while SLM habla español): las `description`s pueden ser específicas del ambiente local, e.g. "Aquí están los patrones GIS que usas para problemas de cuencas" — Steering proactivo en vez de esperar que el modelo recuerde.
- Combina con skills loader (#7):`references` + `skills.paths` = ambiente "este proyecto tiene docs, tools, y skills — el modelo puede resolver su propia incertidumbre".

**Arreglo**: `BrazeConfig::references: Vec<RefConfig { path, description: Option<String>, hidden: bool }>`. En `EngineBuilder`:

1. Añadir cada `path` a `WorkdirAllowlist`.
2. Si `description.is_some()`, inyectar en el system prompt builder (`braze-config/src/prompt.rs`).
3. `hidden` controla si el path aparece en `@`-menciones de TUI (preservandoBraze lazy-load).

**Refs**: docs/AUDITORIA-2026-07-v3.md D7; CLAUDE.md OpenCode "References" §.

---

## 5 hallazgos NUEVOS inspirados en el schema de OpenCode (no en v1-v5)

### N1 [ALTA][NEW from OpenCode] — ChunkTimeout para SSE streams (ver #6 arriba)

Detallado en § 6. Elevado a hallazgo backlog porque no estaba en v1-v5.

### N2 [MEDIA][NEW from OpenCode] — `experimental.continue_loop_on_deny`

OpenCode: "Continue the agent loop when a tool call is denied". Por default off; si una llamada se niega por permisos, el loop continúa dejando al modelo adaptarse (en vez de abortar el turno).

braze hoy: el turno sigue tras denegación (PermissionDecidedpersiste, `is_error=true` en ToolResult), pero no hay knob para apagar este behaviour. Si alguien quisiera "abortar en cualquier denegación" (más seguro para entornos restrictivos), no puede sin editar `Engine::dispatch_tool_calls`.

Aplica a v3 D5 (nudge intra-turno): `continue_loop_on_deny = true` es ya el behavior de braze; el valor aquí es hacerlo opt-out configurable.

### N3 [MEDIA][NEW from OpenCode] — `experimental.primary_tools` para segregación planner/lead

OpenCode: tools que sólo primary agents pueden usar (no subagents). Literal:

```jsonc
"experimental.primary_tools": ["edit", "bash"]
```

Aplica a braze: hoy el `--planner` no hace distinción de tools disponibles. Un planner que "sólo planea" no debería tener `write_file`/`shell_exec` — son del executor. En la implementación actual todo el `ToolRegistry` está disponible para el planner, aunque `attempt_planning_round` evite tools (pasa `tool_stubs: Vec::new()` en el request, pero `Engine` sigue registrando todas).

`primary_tools` sería el mecanismo declarativo: "el planner no ve write_file". Beneficios:

- v3 D5 (nudge intra-turno vs cross-turn): el planner no puede mutar estado, simplifica reason about.
- v4 P0.3 (preflight write_file): si el planner nunca ve write_file, no puede causar daño destructivo.

### N4 [MEDIA][NEW from OpenCode] — `command.execute.before` hook abre la puerta a slash commands custom

OpenCode tiene el hook `command.execute.before` que muta el template de un command antes de evaluarlo. braze TUI slash commands son actualmente built-in (`/help`, `/quit`, `/model`). No existe un mechanism para que el usuario escriba `/sweep-openrouter $1` y se materialice en un prompt.

El formato de comand file de OpenCode:

```markdown
---
description: ...
agent: build
model: anthropic/claude-sonnet-4-6
---

Template body with $ARGUMENTS / $1, $2
```

Es trivialmente portable a braze. OpenCode ya lo hizo. El loader `**/*.md` en `.opencode/command/` es el mismo patrón que skills. braze `/model` picker podría extenderse con `/<custom-name>` pickers (e.g. `/sweep-openrouter $BACKEND` para el pipeline del paper).

Cierra parcial: v3 E2 (mini-swe-agent como baseline externo) — un command `--external` podría ser un `command.execute.before: inject --external $1`.

### N5 [BAJA][NEW from OpenCode] — `snapshot: true` como preset "guarda estado antes de tergiversar"

OpenCode: `snapshot: true` (default true). Registra snapshots del filesystem del project, permite undo/redo.

braze: v4 P0.3 pide checkpoint automático antes de cambios grandes. Un `Config::snapshot: bool` + `Engine::with_snapshot_strategy` que hace `cp -r $cwd $TMPDIR/braze-snap-$Turn/` antes de cada `write_file`/`edit_file` destructivo sería la implementación cheapest. No tan fino como git worktrees (que es lo que OpenCode probablemente usa), pero cerzero protection.

`snapshot: false` para bench (donde el sandbox es tmpdir de todas formas), `snapshot: true` para prod interactivo.

## Out-of-scope (cosas de OpenCode que no le vienen bien a braze)

| Feature OpenCode | Por qué no aplica a braze |
|---|---|
| `attachment.image` (max_width/max_height/max_base64_bytes) | braze MVP no maneja attachments de imagen. Fase 2+. |
| `lsp: false | true | extension-map` | braze no tiene LSP (no es agente IDE). Fase 2+. |
| `share: "manual" | "auto" | "disabled"` | braze es local-first; `/share` URL es cloud-side feature. v1 lo diferido. |
| `server` config (port, mDNS, CORS) | braze es CLI binario, no server. La arquitectura cliente/servidor de OpenCode (blueprint "braze como backend" en SOTA) es otra dirección. |
| `enterprise` (GitHub Copilot login, ChatGPT Plus) | braze usa API keys propias; login OAuth providers está fuera de scope. |
| `mcp_timeout` (experimental) | braze ya tiene TTL 60s client-side (SOTA item 4). |
| `provider.options.setCacheKey` | Anthropic-native context no es el foco (H-18 pegusa). |
| `auto-share`/`autoupdate` | braze no se autoactualiza (es un experimento del usuario). |

## Top-10 priorizado por valor para braze SLM-first

| # | Feature OpenCode | Cierra | Esfuerzo | ROI |
|---|---|---|---|---|
| 1 | `AgentConfig.steps` | v4 P0.2 (mitad rondas) | trivial (config + builder) | alto - primero a hacer |
| 2 | `compaction.{tail_turns, prune, preserve_recent_tokens, reserved}` | v3 B1/B2, v4 P2.4 | medio (config + reemplazar const en Engine) | alto |
| 3 | `tool_output.{max_lines, max_bytes}` | v4 P2.4 | trivial | alto |
| 4 | `formatter` per-extension | v4 P1.6 | medio | alto - SLM-first win claro |
| 5 | `permission` por-patrón | v5 H-6, v4 P2.3+P0.3, v3 Grupo O | alto (diseño + migrate) | altísimo (resuelve varios) |
| 6 | `ProviderConfig.options.chunkTimeout` (NEW H-26) | gap nuevo | medio (watchdog async) | alto |
| 7 | `skill` como tool + loader roaming | Fase 2 `braze-skills`, permite reusar 116 skills | alto (crate nuevo) | medio-alto (SLM-first) |
| 8 | `experimental.policies` | v4 P1.2 + P0.2 cost | medio (policy engine) | medio |
| 9 | Plugin / hook surface | Fase 2 `braze-hooks`, OTel, H-3 alt route | alto (trait + stable) | alto (estructural) |
| 10 | `references` con descriptions | v3 D7 | bajo-medio | medio (steering proactivo SLM) |

## Cómo aprovecharlo (qué patrón imitar literalmente vs adaptar)

**Copiar literalmente el shape**:

- `tool_output.max_lines + max_bytes` — los nombres y tipos son idénticos.
- `compaction.prune + tail_turns` — misma semántica (braze's "colapso ACI" = OpenCode `prune`).
- `AgentConfig.steps` — mismo campo, mismo default; en braze podría llamarse `max_turn_iterations` para mantener naming consistente.

**Adaptar el shape**:

- `permission` por-patrón — el insert-order semantics ("último match gana") de OpenCode difiere de `DefaultClassifier` que hace pattern match en código. Una elección: braze podría mantener el `DefaultClassifier` hardcoded para backward compat (que es la "tabla fija de siempre confirmar") y añadir un `PermissionConfig` override que se evalúa primero, en cascada. Más conservador que OpenCode.
- `formatter` por extensión — el shape `extensions: Vec<String>` de OpenCode es bueno, pero braze querría además `timeout_secs` (v5 H-5 análogo para post-edit). OpenCode no lo expone.
- `experimental.policies` — la noción `action/effect/resource` es razonable pero el action set de OpenCode (`provider.use` only) es chico. braze necesitaría `escalation.allow`, `provider.cost_limit`, etc. Schema extensible.
- `skill` loader — braze necesita `SkillRef` como `ToolStub` + body path. OpenCode no expone esto como ToolProvider (es internal). El diseño de braze Skills → ToolProvider es una adaptación.

**No copiar**:

- El modelo `agent: { plan: ..., build: ..., general: ..., explore: ... }` pre-baked de OpenCode tiene poca traducción a braze. `build` y `plan` no son primarias en braze; `planner` es un backend, no un agente con contexto propio. Si braze llegara a subagents (ver Moore siguiente), podría tomar la noción de `mode: "subagent"` con permissions heredados.
- `mode` enum `{subagent, primary, all}` no se traduce directo. braze tiene `Engine` (primary) + `--planner`/`--lead` (backends auxiliares, no agents separados). Un `mode: "subagent"` para braze implicaría sesión aislada — que es el multi-agente de Fase 2 blockrado por SOTA (señal débil contraproducente). **No adoptar sin evidence nueva.**
- `lsp`/`formatter` booleanos en top-level — son atajos de OpenCode para "apagar global". braze prefiere no tener tools que no existen (no hay LSP luego apagarlo). El `formatter` extension-map sí vale la pena; el boolean global no.
- Plugin TS runtime — no notion en braze (es Rust). Los plugins equivalentes serían trait impls en Rust crates. El hook surface (§ 9) se puede diseñar sin runtime TS.

## Subagent con contexto aislado — nota separada

OpenCode permite `mode: subagent` (agent con contexto propio aislado). braze Fase 2 bloquea multi-agente (SOTA: señal débil). Pero un subagent con contexto aislado es distinto de "multi-agente": es "spawnea un agente nuevo para un task finito, devuélveme un resultado, libera el contexto". Caso de uso braze: "explora el crate X y dime las firmas relevantes, devuélveme 200 líneas de findings" sin contaminar el contexto del turno principal.

Evidence exogenous: Aider "architect/editor" (+3-4pp) y Goose lead/worker reactivo son formas de subagent (SOTA § adenda agentes open-source). braze ya tiene `--lead` reactivo, pero no proactivo.

Esto se deja como hypothesis de investigación futura, no se incluye en el top-10 — el SOTA pre-screen dice "multi-agente contraproducente" pero el subagent isolation para tasks finitos no se midió en braze todavía. Si braze añadiera Skills (#7), un `@skill-name` invocation implícitamente podría usar subagent isolation.

## Conclusión

OpenCode y braze son **complementarios en prestaciones**: braze innovó en tolerancia a fallos (rescates textuales, escalación reactiva, compaction diferencial con invariants), OpenCode innovó en extensibilidad y ergonomía de configuración. De las 10 features listadas, 6 son "expresar en config algo que braze ya hardcodea", 3 son "añadir una capability nueva que OpenCode tiene y braze no", y 1 (chunkTimeout, N1) es "un gap nuevo que viendo el schema de OpenCode hace evidente".

**Recomendación para siguiente tramo de trabajo**: el primer lote de "config exposure" (items 1, 2, 3 anteriores) se puede hacer en una sesión de código, cierra 3 ítems del backlog v5 + 2 de v4, y desbloquea ablationes que faltan para el paper (el propósito declarado del proyecto). Los items 5 (permission por-patrón) y 9 (hook surface) son estructuralmente más caros pero cada uno resuelve una clase entera de hallazgos — No postergarlos más allá del Paquete 2 del roadmap v5.

## Refs

- docs/AUDITORIA-2026-07-v5.md (auditoría reciente)
- docs/AUDITORIA-2026-07-v4.md (backlog P0-P2)
- docs/AUDITORIA-2026-07-v3.md (backlog Grupos O-S)
- docs/SOTA-2026-07.md (adenda OpenCode, Aider, Qwen Code, Goose)
- docs/H-1-cierre-cache-tokens.md (próximo paso Paquete 1)
- https://opencode.ai/config.json (schema authoring)
- ~/.config/opencode/opencode.jsonc (config personalizada del usuario — 3 subagents, 13 commands, 3 references, 3 MCP locales)
- skill `customize-opencode` (contract authoring docs)