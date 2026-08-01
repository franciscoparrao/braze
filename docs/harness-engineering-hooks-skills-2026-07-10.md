# Estudio consolidado: harness engineering para braze — Codex, OpenCode y Claude Code

Fecha: 2026-07-10 (consolidado; v2 de este archivo)
Procedencia: este documento consolida los tres estudios del ciclo "cada agente estudia su propio harness y propone qué le serviría a braze":

1. **OpenCode** (glm-5.2, 2026-07-08) — `docs/opencode-a-braze.md`, que se conserva como fuente de detalle; acá solo se recogen sus dictámenes vigentes.
2. **Codex** (2026-07-10) — la versión 1 de este mismo archivo: harness determinista, propuesta de hooks y skills en profundidad. Sus secciones se conservan abajo (Partes II y III) con el estado actualizado.
3. **Claude Code** (Fable 5, 2026-07-10) — introspección del propio harness en vivo; sus propuestas forman la Parte I y absorben lo que era `docs/claude-code-a-braze.md` (eliminado al consolidar).

Estado de braze de referencia: HEAD `e16143e` (post-Paquete 4 de v6). **Importante**: la v1 de este archivo recomendaba implementar `references` como primer paquete; eso se cerró hoy (opencode-10, commit `e16143e`) — el roadmap de abajo ya lo refleja.

## Resumen ejecutivo

Los tres estudios convergen en la misma tesis desde ángulos distintos: **braze no debe copiar un agente frontier, sino convertir más responsabilidades en harness determinista** — para modelos pequeños, las mejoras útiles no son "más texto en el prompt" sino andamiaje que el modelo chico no puede improvisar.

Cada estudio aporta una capa distinta:

- **OpenCode** aportó la capa de **configuración**: knobs expuestos en vez de constantes (compaction, steps, permisos declarativos, references). De sus 10 propuestas + 7 ítems de backlog, lo dictaminado HACER ya está cerrado (steps/P0.2, ablations, pricing, references); el resto quedó diferido con dictamen explícito.
- **Codex** aportó la capa de **extensibilidad disciplinada**: hooks por niveles de riesgo (audit-only primero), skills como memoria procedural con disclosure progresivo y router determinista, y la regla rectora "si una extensión no puede demostrarse con una suite y una ablation, no debe convertirse en default".
- **Claude Code** aportó la capa de **canal harness→modelo**: el harness no solo ejecuta y observa — le habla al modelo (avisos de presupuesto antes de cortar, contratos de compactación explicados, recordatorios en el momento del loop, resultados con receta de recuperación). braze nació copiando los principios de primera generación de Claude Code (carga diferida, compactación diferencial, push notifications, permisos de dos capas); lo que queda por copiar es esta segunda generación conversacional — y un SLM la necesita más que un frontier, no menos.

Prioridad consolidada (detalle en § Roadmap): primero las palancas conversacionales baratas y medibles hoy (recovery hint del colapso ACI, `HarnessNote` de budget/loops, `search_tools` de dos niveles), después hooks audit-only, después skills locales explicit-first.

## Principios SLM-first (del estudio de Codex, vigentes para todo el documento)

1. El harness debe hacer lo que el modelo chico hace mal: clasificar estados, preservar invariantes, recordar presupuestos, aplicar validaciones y decidir cuándo reintentar.
2. El modelo debe recibir instrucciones pequeñas, oportunas y específicas. El contexto es presupuesto, no almacenamiento general.
3. Toda palanca debe ser medible en `braze-bench`: baseline, variante, ablation, costo, rondas, tokens y causa de fallo.
4. Una mejora opcional debe degradar sin matar el turno, pero debe emitir un evento persistido si falla.
5. Las decisiones del harness deben ser tipadas, no inferidas desde strings renderizados.
6. Las habilidades deben ser retrieval objects, no una excusa para inflar el system prompt.

Adenda del estudio de Claude Code: **7. El harness debe avisar antes de actuar** — un corte (budget, dedup, compactación) que el modelo no vio venir es un fallo silencioso para un SLM; el mismo corte anunciado una ronda antes es una oportunidad de convergencia.

## Estado actual de braze (post-`e16143e`)

Piezas correctas que ya existen:

- `ToolRegistry` con stubs baratos y resolución de schema bajo demanda.
- `AgentEvent` persistido, usado por session, TUI/observer y bench; eventos de palancas SLM (H-3: rescates, escalaciones, compaction, summary fallback).
- `braze-bench` con suites TOML, sandbox por tarea, timeout, repeticiones, métricas de rondas/tokens/cache/costo (`estimated_cost_usd`, Paquete 3) y causas de fallo tipadas.
- Palancas SLM: rescate textual por familia (Qwen/Qwen3-coder/GLM), hints proactivos por nombre de modelo (I-4), schema repair, compactación diferencial + colapso ACI, best-of-n paralelo (P1.4), planner opt-in, lead/worker con knobs alcanzables (I-1) y atribución causal medida (apertura proactiva ≫ escalación reactiva, `docs/sweep-lead-3brazos-2026-07-10.md`), post-edit check, preflight destructivo de `write_file` (P0.3), TurnBudget por tokens y costo (P0.2/Paquete 3).
- Matriz de ablations completa por fila de sweep (`no-rescue`, `no-post-edit-check`, `strict-edit`, `best-of-n`, `tactical-*`, `full-observations`, `no-caching`, `no-prune`, `no-planner`, `no-lead`, `lead-*`).
- **`references` con descriptions (opencode-10): implementado** — `Config::references` entra al `WorkdirAllowlist` y se anuncia en el system prompt; verificado en vivo.

Gaps que este documento ataca:

- El canal harness→modelo mid-turn es ad hoc (post-edit check dentro del tool result); los cortes duros (TurnBudget, D4, `MAX_TURN_ITERATIONS`) ocurren sin aviso previo al modelo.
- La carga diferida es de un solo nivel: todos los nombres+summaries entran al contexto; no escala a providers MCP grandes (el gateway GIS del usuario: 1.500+ tools contra `num_ctx=8192`).
- `braze-skills` y `braze-hooks` siguen diferidos; no hay loader ni eventos de carga.
- `TurnObserver` es un mirror pasivo; no resuelve extensiones que deben intervenir.
- Sin retry/backoff en cloud (v5 H-19): un 429 mata el turno y contamina la medición.

---

# Parte I — El canal harness→modelo (estudio de Claude Code)

Método: introspección del propio harness en ejecución — system prompt (bloque de entorno, contrato de compactación, sección de memoria), inventario de tools (`ToolSearch`, `TaskCreate`/`TaskUpdate`, `AskUserQuestion`, subagentes tipados, `Monitor`), y comportamiento observado en la sesión misma (system-reminders inyectados, task-notifications re-invocando al agente, avisos de archivo-modificado tras cada edición externa).

Lo que braze ya tiene y el estudio valida (sin re-proponer): carga diferida de schemas, compactación diferencial, `TaskNotifier` push, permisos de dos capas con replay por sesión, post-edit feedback en el tool result (la versión braze de "hook output as feedback").

### I.1 Contrato de compactación explicado al modelo + recovery hint en el colapso ACI

**Claude Code**: el system prompt explica qué pasa cuando el contexto se llena ("se resume; seguí trabajando, no cierres antes") y los resultados truncados traen la receta exacta de recuperación (`read_file` con `offset`).

**braze hoy**: mitad y mitad. `read_file` pagina con nota de recuperación (bien). El colapso ACI marca `[old observation collapsed: N chars omitted; ...]` — dice QUÉ pasó, no QUÉ HACER; y el system prompt no menciona que las observaciones viejas se colapsan.

**Arreglo (dos strings)**: (a) extender el marcador: `...shown in full. Re-run the tool or read_file the path if you need the omitted content]`; (b) una línea en el system prompt. Medible hoy: `+ablate:no-prune` ya existe; un tercer brazo "prune + recovery hint" aísla el efecto sobre `error_recovery`/`multi_step` con ventana táctica chica.

### I.2 `HarnessNote` — presupuesto, loops y compactación avisados ANTES de cortar

**Claude Code**: inyecta `<system-reminder>` operacionales en el momento accionable ("lista de tareas stale", "archivo modificado por linter", "contexto por compactarse").

**braze hoy**: `TurnBudget` intenta una ronda de summary y aborta con `TurnBudgetExhausted` — el modelo nunca supo que tenía presupuesto; D4 filtra la tool call duplicada en silencio; `MAX_TURN_ITERATIONS` corta sin countdown.

**Por qué SLM-first**: un frontier infiere "llevo muchas rondas, cierro"; un 3B no — explora hasta que el harness lo mata y el turno cuenta como fallo. Un aviso único al 80% del budget ("quedan ~N tokens; responde ahora con lo que tienes") convierte aborts en respuestas degradadas-pero-útiles.

**Arreglo**: tipo `HarnessNote { kind, text }` anexado al último tool result de la ronda (mismo mecanismo que el post-edit check, generalizado y tipado — no un rol nuevo de mensaje, que algunos wire formats no soportan). Tres emisores iniciales: budget al 80% (una vez), D4 al filtrar duplicado, aviso pre-compactación. Cada emisión persiste como `AgentEvent`. Medible: ¿el aviso convierte `TurnBudgetExhausted` en turnos convergidos? Nota de diseño: los `HarnessNote` son el caso de uso concreto del punto de enganche `after_tool_dispatch` de la Parte II — pueden implementarse directo en el engine primero y migrar a hook H1 cuando esa surface exista.

### I.3 `search_tools` — herramientas diferidas en DOS niveles

**Claude Code**: las tools diferidas no pesan NADA en el prompt (ni el nombre); un meta-tool `ToolSearch` acepta `select:` o keywords y carga schemas bajo demanda, con guía explícita de batch.

**braze hoy**: `list_stubs()` pone todos los nombres+summaries en contexto. Correcto para 6 tools locales; no escala al caso real del usuario: gateway MCP con 1.500+ tools GIS (su CLAUDE.md global documenta que lo redujo a mano a 6 meta-tools — braze debería tenerlo nativo).

**Por qué SLM-first**: es el argumento del colapso ACI aplicado al inventario. `distractor_selection` mide la debilidad con 2-3 tools de ruido; con cientos, el problema es estructural.

**Arreglo**: umbral en config (`deferred_tool_names_threshold`, default ~40): si los stubs de un provider lo superan, no se listan — se registra `search_tools(query)` que rankea nombre+summary y devuelve top-K stubs (invocables vía `resolve_schema` normal). Medible: suite con provider sintético de N tools de ruido, brazos con/sin search.

### I.4 Lista de tareas tipada — el plan como estado, no como prosa

**Claude Code**: tasks con estados/dependencias vía tools; el harness la muestra al usuario y **se la recuerda al modelo** cuando está stale. El plan sobrevive compactaciones.

**braze hoy**: el plan del split planner/executor es prosa (`PlanCreated`), y el A/B dio negativo — una hipótesis anotada: el plan-en-prosa no guía al executor chico.

**Arreglo**: tools `task_add`/`task_update` sobre estado en el engine + re-inyección compacta por ronda ("hecho: 1,2; en curso: 3"). Vehículo del A/B: la iteración pre-registrada del planner (PLAN.md) — planner→tasks vs planner→prosa vs sin planner, sobre `multi_step`.

### I.5 `ask_user` — clarificación estructurada de opción múltiple

**Claude Code**: ante una decisión genuinamente del usuario, pregunta tipada con 2-4 opciones renderizada como picker; guía estricta de no abusar.

**braze hoy**: un modelo inseguro divaga o adivina; la TUI ya tiene overlays (approval, `/model`).

**Por qué SLM-first**: adivinar mal cuesta un turno de tools + arreglo; preguntar cuesta ~100 tokens. Convertir "editó el archivo equivocado" en "preguntó cuál" es degradar fallo destructivo a fricción — la filosofía de P0.3. Solo sesiones interactivas (bench y `run` no la exponen); verificación pty, no bench.

### I.6 Bloque de entorno opt-in en el system prompt

**Claude Code**: snapshot al inicio (branch, status, últimos commits, OS, fecha) — el modelo no gasta rondas en orientarse.

**braze hoy**: cwd + reglas + hint de familia + references. Un modelo local gasta 1-2 rondas de `shell_exec` en orientarse (~2-6s/ronda en Nitro).

**Arreglo**: `Config::environment_block: bool` (default off por `num_ctx`); el composition root genera el snapshot recortado (branch + `git status -s | head -10` + fecha). El bench lo deja off (sandbox sin git; N-36 exige paridad con el prompt de producción si se promueve a default).

### I.7 Explorador de contexto aislado — la inversión del lead (diseño del A/B que v6 pidió)

**Claude Code**: además de subagentes más capaces, ofrece `Explore` — read-only y barato — al que delega búsqueda amplia para **proteger el contexto del agente principal**: el explorador quema su propia ventana leyendo 30 archivos y devuelve tres líneas.

**braze hoy**: multi-agente es Fase 2; v6 dictaminó "subagent isolation: diseñar A/B primero". La escalación existente va chico→grande; no existe la dirección aislamiento-de-exploración.

**Diseño del A/B (no implementación)**: tool `explore(question)` que instancia un Engine hijo (mismo backend 3B, tools read-only, `max_turn_iterations` bajo, sin session store) y devuelve su texto final como tool result. Suite de búsqueda-amplia (respuesta en 1 de 15 archivos de ruido); brazos: baseline / +explore / +explore+`no-prune` (¿el aislamiento subsume al colapso?). Positivo → recién ahí diseñar la isolation seria de Fase 2.

### I.8 `braze permissions suggest` — minar los session logs para proponer la allowlist

**Claude Code**: un skill lee transcripts pasados, encuentra las tool calls read-only que más confirmaciones pidieron y propone la allowlist priorizada.

**braze hoy**: replay de `PermissionKey` dentro de la sesión; cada sesión nueva parte de cero. El dictamen de opencode-5 (permisos declarativos, M-L) fue tibio; esto lo reordena: primero la evidencia (los session logs ya persisten los eventos), después el formato declarativo mínimo que esa evidencia pida.

**Arreglo**: subcomando que agrega sobre los session logs, imprime top-N de confirmaciones por (tool, patrón) y el snippet de config propuesto. Sin cambios en el guard hasta ver la evidencia.

### I.9 Retry con backoff en el wire cloud (v5 H-19, confirmado desde el otro lado)

**Claude Code**: los errores transitorios (429/5xx/stream) se reintentan dentro del harness; el agente solo ve el error terminal.

**braze hoy**: H-19 abierto — un 429 aborta el turno. El A/B de 3 brazos de hoy mostró 2 fallos `model_backend_error` transitorios contaminando `error_recovery`. Valor principal: fidelidad de medición (F5: fallos de harness ≠ fallos de modelo). Arreglo: el ya especificado en H-19 + evento persistido por retry.

### Ideas evaluadas y descartadas (para no re-litigar)

| Idea | Por qué no |
|---|---|
| Memoria persistente escrita por el agente entre sesiones | Riesgo de auto-envenenamiento para un SLM (memorias erróneas re-inyectadas); `references` cubre la variante curada-por-humano, que es la segura. Reevaluar en Fase 2 con validación. |
| Plan mode (fase read-only gateada por el usuario) | El split automático ya dio A/B negativo; la variante user-gated es UX de supervisión (`--supervised` cubre), no palanca SLM. |
| Wakeups auto-programados con economía de cache TTL | No hay loop autónomo desatendido donde amortizarlo. |

---

# Parte II — Hooks (estudio de Codex)

### Características útiles del harness de Codex

| Característica | Valor para modelos pequeños | Adaptación a braze |
|---|---|---|
| Sandbox y permisos por acción | El modelo no necesita razonar perfectamente sobre seguridad. | Extender `braze-permissions` con permisos para skills y hooks transformadores. |
| Aprobaciones con justificación | Hace visible por qué una acción requiere permiso. | Persistir `HookPermissionRequested`/`SkillLoadRequested` o reutilizar `PermissionRequested` con `PermissionKey` nuevo. |
| Edición por patch | Reduce riesgo de sobrescribir archivos grandes. | `edit_file` como ruta preferente (P0.3 ya bloquea el overwrite destructivo); hooks con transformaciones acotadas, no escrituras libres. |
| Disclosure progresivo de skills | Solo metadata hasta que la tarea necesita instrucciones. | `SkillStub` siempre barato; `SkillBody` solo por invocación explícita o router confiable. |
| Tool discovery diferido | Nombres/resumen visibles, schema al dispatch. | Ya existe; la Parte I (I.3) propone el segundo nivel. |
| Plan de trabajo observable | Reduce drift en tareas largas. | `PlanCreated` existe; falta `planner_rounds` y la variante tipada (I.4). |
| Observer separado de persistencia | UI mira sin ser fuente de verdad. | Un hook puede influir → necesita permisos, timeout y eventos; no confundir con `TurnObserver`. |
| Comandos con timeout y cap de salida | Evita que una tool ahogue el contexto. | Existe en tools/bench; hooks/skills heredan caps por defecto. |
| No pisar worktree sucio | Evita destruir trabajo humano. | Preflight de tasks self-improvement: registrar estado git y exigir confirmación si hay archivos no relacionados. |
| Verificación como parte del flujo | El agente no declara éxito sin checks. | Skills declaran checks recomendados; hooks sugieren, no ejecutan sin permiso. |
| Logs/eventos tipados | Distinguen fallo de modelo/harness/aserción. | `AgentEvent` como contrato; eventos pequeños para skills/hooks. |

### Separación conceptual y niveles

`TurnObserver` sigue siendo vista pasiva. Un hook puede observar, bloquear, transformar o enriquecer — mezclarlos volvería ambiguo el source of truth.

| Nivel | Tipo | Puede mutar | Riesgo | Implementar |
|---|---|---:|---|---|
| H0 | audit-only | No | Bajo | Primero |
| H1 | enrich/annotate | Solo metadata/eventos | Bajo-medio | Segundo |
| H2 | transform | Mensajes, tool args, tool result | Medio-alto | Después de bench |
| H3 | authority | Permisos, provider routing, abort | Alto | Solo con policy clara |

### Surface mínimo recomendado

```rust
#[async_trait::async_trait]
pub trait EngineHook: Send + Sync {
    async fn on_event(&self, _ctx: &HookContext<'_>, _event: &AgentEvent) -> HookResult {
        HookResult::Continue
    }

    async fn before_model_request(
        &self,
        _ctx: &HookContext<'_>,
        _request: &mut CompletionRequest,
    ) -> HookResult {
        HookResult::Continue
    }

    async fn before_tool_dispatch(
        &self,
        _ctx: &HookContext<'_>,
        _call: &mut ToolCall,
        _schema: &ToolSchema,
    ) -> HookResult {
        HookResult::Continue
    }

    async fn after_tool_dispatch(
        &self,
        _ctx: &HookContext<'_>,
        _call: &ToolCall,
        _result: &mut ToolResult,
    ) -> HookResult {
        HookResult::Continue
    }

    async fn before_compaction(&self, _ctx: &HookContext<'_>, _events: &[AgentEvent]) -> HookResult {
        HookResult::Continue
    }
}

pub enum HookResult {
    Continue,
    AbortTurn { reason: String },
    DisableHook { reason: String },
}
```

Cada hook: `id` estable, timeout corto, failure policy (`warn_and_continue`/`disable_hook`/`abort_turn`), contadores de latencia/errores, permiso para mutar separado del de observar.

### Eventos nuevos sugeridos

- `HookErrored { id, point, policy }`
- `HookModifiedRequest { id, point, summary }`
- `HookAbortedTurn { id, reason }`

(`HookInvoked` omitido por ruido en H0.) En bench: `hook_errors`, `hook_modifications`, `hook_latency_ms`.

### Hooks útiles para modelos pequeños

| Hook | Nivel | Utilidad |
|---|---|---|
| `FailureKindHook` | H1 | Clasifica fallos sobre eventos crudos, no strings colapsados (I-3/F3 — I-3 ya cerrado; F3 abierto). |
| `ToolResultNormalizerHook` | H2 | Preserva markers importantes antes de truncar/colapsar. |
| `PromptBudgetAuditHook` | H0 | Bytes/tokens por system, tools, history, skill y references. |
| `SkillSelectionAuditHook` | H0 | Skills consideradas/cargadas y tokens agregados. |
| `HarnessNoteHook` | H1 | Los avisos de I.2 (budget/D4/compaction) como hook cuando la surface exista. |
| `OtelExportHook` | H0 | Exporta eventos sin tocar `engine.rs`. |

(El `TurnBudgetHook` H3 de la v1 quedó obsoleto: P0.2 se cerró dentro del engine — Paquete 3.)

### Dónde enganchar sin romper el motor

1. Después de `append_and_notify`: `on_event`.
2. Antes de `complete_once`/`complete_with_best_of_n`: `before_model_request`.
3. Después de `RoundOutcome`: `after_model_round` (audit-only inicialmente).
4. En `dispatch_tool_calls`, tras resolver schema y antes de spawn: `before_tool_dispatch`.
5. Después de recibir `ToolResult`: `after_tool_dispatch`.
6. En el punto de decisión de compaction: `before_compaction`/`after_compaction`.

Orden estable y probado; si dos hooks transforman el mismo objeto, el orden viene de config, no del discovery.

### Config inicial

```json
{
  "hooks": {
    "enabled": true,
    "failure_policy": "warn_and_continue",
    "timeout_ms": 250,
    "audit_only": true,
    "plugins": []
  }
}
```

Primera versión sin plugins dinámicos: solo hooks compilados o construidos por `braze-cli`/`braze-bench`. La API pública se estabiliza antes de cargar código externo.

---

# Parte III — Skills (estudio de Codex)

### Diagnóstico

Una skill no debería ser "más system prompt": es una unidad de memoria procedural — metadata barata siempre indexable, body cargado solo si es relevante, recursos con disclosure progresivo, scripts nunca ejecutados implícitamente, eventos para saber si ayudó. Coincide con el patrón de Codex (`SKILL.md` con frontmatter + recursos); lo que cambia para braze es la **selección**: un frontier decide bien qué skill pedir; un SLM necesita ayuda del harness.

### No cargar las 116 skills del entorno tal cual

v6 acierta al diferir un loader general: muchas skills existentes son largas, densas y escritas para modelos fuertes. En un SLM consumen contexto, aumentan indecisión, introducen instrucciones incompatibles, o hacen que el modelo imite procedimientos que no puede ejecutar. La ruta correcta: soportar el formato, empezar con una allowlist de skills SLM-native (cuerpos chicos, triggers claros, ejemplos cortos, checks medibles).

### Modelo de datos

```rust
pub struct SkillStub {
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    pub estimated_tokens: u32,
}

pub struct SkillBody {
    pub name: String,
    pub body_markdown: String,
    pub resources: SkillResources,
}

pub enum SkillTrigger {
    ExplicitMention,
    SlashCommand,
    RouterMatch,
    BenchFixture,
}
```

Eventos: `SkillLoaded { name, source, trigger, estimated_tokens }`, `SkillLoadSkipped { name, reason }`, `SkillSelectionFailed { reason }`. No persistir `SkillIndexed` por turno (ruido).

### Loader local (v1)

- `skills.paths`: directorios locales; discovery por `**/SKILL.md` con límites de profundidad y bytes.
- Parser mínimo de frontmatter (`name`, `description`); nombre normalizado; duplicados por prioridad de path con warning.
- Sin URLs remotas en v1 (requieren cache, checksum, permisos, política de actualización).

```json
{
  "skills": {
    "enabled": true,
    "paths": [".braze/skills"],
    "mode": "explicit_first",
    "max_loaded_per_turn": 2,
    "max_body_tokens": 1200,
    "allow_remote": false
  }
}
```

`mode`: `off` / `explicit_only` / `explicit_first` (recomendado para SLMs) / `auto` (solo post-bench).

### Selección: router determinista, no el modelo

1. Mención explícita (`$testing`, `/skill testing`) gana.
2. Sin mención: ranking lexical/BM25 sobre `name + description`.
3. Penalizar skills largas y re-cargas sin progreso.
4. Máximo 1-2 por turno; evento con trigger y tokens.

El modelo puede ver una lista pequeña de skills disponibles, pero el harness decide qué cuerpos entran al prompt.

### Inyección al prompt

Addendum estructurado al system prompt del siguiente request (`Loaded skill: testing` + body capado). No persistir el body como `UserMessage` (ensucia historial y compaction): persistir `SkillLoaded` y reconstruir desde el registry; si el replay exacto lo exige, persistir hash+versión.

### Skills como ToolProvider vs SkillRegistry

| Opción | Ventaja | Problema |
|---|---|---|
| Skills como `ToolProvider` | Reusa tool-calling y permisos. | Un SLM puede no pedir la skill correcta antes de fallar; la carga llega tarde. |
| `SkillRegistry` first-class | El harness carga antes del primer round. | Crate y wiring nuevos. |

Recomendación: `SkillRegistry` first-class + tool opcional `load_skill` para cargas explícitas mid-turn. Lo importante para SLMs: cargar la guía **antes** del primer error del executor.

### Permisos de skills

Lectura de archivos, no ejecución. `skill.load: allow|ask|deny`; `skill.remote.load: deny` default; scripts dentro de una skill pasan por las tools y permisos normales; paths fuera de allowlist requieren config explícita.

### Bench para skills

`TaskDef`: `expect_skill_loaded`, `expect_no_skill_loaded`, `expect_max_skill_tokens`. `TaskResult`: `skills_loaded`, `skill_tokens`, `skill_selection_failures`, `expected_skill_loaded`. Ablations: `+ablate:no-skills`, `skill-mode=explicit_only`, `skill-max-body-tokens=N`. Tareas: `skill_explicit_testing`, `skill_irrelevant_rejected`, `skill_guided_edit`, `skill_budget_cap`.

---

# Parte IV — Lo vigente del estudio de OpenCode

Detalle completo en `docs/opencode-a-braze.md`. Estado de sus propuestas a `e16143e`:

| Propuesta | Estado |
|---|---|
| #1 steps / max_turn_iterations configurable | **Cerrado** (v4 P0.2 mitad rondas; Paquete 3 cerró la mitad costo/tokens) |
| #2 knobs de compaction + prune ablatable | **Cerrado** (I-2 caps proporcionales; opencode-2 `no-prune` en 912fedb) |
| #8 pricing table / costo | **Cerrado** (Paquete 3: `estimated_cost_usd` + enforcement) |
| #10 references con descriptions | **Cerrado** (opencode-10 en `e16143e`) |
| #5 permisos declarativos por patrón | Diferido condicionado — I.8 (`permissions suggest`) genera la evidencia que lo desbloquearía |
| #6 chunkTimeout | BAJA (I-7); absorber al cerrar H-19 (I.9) si conviene |
| #7 skills loader | Diferido → Parte III es el plan cuando se retome |
| #9 hook surface | Fase 2 → Parte II es el plan cuando se retome |

---

# Roadmap consolidado

Reemplaza los paquetes A-E de la v1 (el Paquete A — references — ya está cerrado) e integra las prioridades de la Parte I.

### Paquete A′ — canal harness→modelo mínimo (S, valor SLM alto, medible hoy)

1. **I.1** recovery hint del colapso ACI + línea de contrato de compactación en el system prompt (dos strings; ablation `no-prune` ya lista para el A/B de tres brazos).
2. **I.2** `HarnessNote` con tres emisores (budget 80%, D4, pre-compactación), directo en el engine; migra a hook H1 cuando exista la surface.
3. **I.9** retry/backoff cloud (H-19, spec lista) + evento por retry — limpia la medición de los sweeps cloud del paper.

### Paquete B′ — hooks audit-only (M, valor de investigación alto)

`EngineHook` H0/H1 (`on_event` + `before_model_request` read-only), registro en Engine, timeout/failure policy, eventos de error/modificación/abort, `PromptBudgetAuditHook`, tests de orden y degradación. (Como estaba en la v1, menos el `TurnBudgetHook` obsoleto.)

### Paquete C′ — escala de inventario y plan tipado (M)

1. **I.3** `search_tools` de dos niveles con umbral en config + suite de distractores masivos.
2. **I.4** task list tipada como vehículo de la iteración pre-registrada del planner.

### Paquete D′ — skills locales explicit-first (M-L, como Paquete C de la v1)

`braze-skills` con discovery local, config, detección `$skill`/`/skill`, inyección capada, eventos, bench con `expect_skill_loaded`. Después (gate por A/B): router automático (Paquete D v1).

### Paquete E′ — interactivo y Fase 2

1. **I.5** `ask_user` (TUI overlay; verificación pty).
2. **I.6** bloque de entorno opt-in.
3. **I.8** `braze permissions suggest`.
4. **I.7** A/B del explorador aislado — el gate que v6 pidió antes de diseñar subagent isolation.
5. Hooks transformadores (H2/H3) y plugins remotos (Paquete E v1) — después de todo lo anterior.

Nada de esto bloquea la matriz del paper (en curso) ni los diferidos explícitos de v6 (P1.1 split de engine.rs antes de Fase 2).

## Criterios de aceptación

Para el canal harness→modelo (Paquete A′):

- El aviso de budget se emite UNA vez, al cruzar el umbral, y persiste como evento; el bench puede contar cuántos `TurnBudgetExhausted` se convirtieron en turnos convergidos.
- El marcador de colapso extendido no rompe la clasificación de fallos (I-3 la hizo robusta a render — verificar que siga).
- Un retry de red no cuenta como fallo del modelo en `TaskResult` (F5).

Para hooks (sin cambios respecto de la v1):

- Hook audit-only no cambia resultados del turno; hook que falla con `warn_and_continue` no mata el turno y emite evento; timeout desactiva o registra según policy; orden estable y testeado; bench reporta latencia/errores.

Para skills (sin cambios respecto de la v1):

- Discovery encuentra `SKILL.md` válido y rechaza frontmatter incompleto; duplicados deterministas; mención explícita carga la correcta; `max_body_tokens` se respeta; `no-skills` deja la tarea sin eventos de skill; una mini-skill mejora al menos una tarea o reduce rondas/tokens frente a baseline.

## Riesgos

- Cargar skills largas puede empeorar el rendimiento de SLMs (por eso allowlist + caps).
- Hooks transformadores pueden volver irreproducible el bench si no se registran como metadata.
- Plugins dinámicos abren superficie de seguridad y versionado.
- Un router automático (de skills O de tools — I.3) puede esconder un problema de prompt si no hay ablations.
- Avisos del harness (`HarnessNote`) mal calibrados son ruido que compite con el presupuesto de prompt — cada emisor nace con su ablation.
- Inflar `AgentEvent` engorda el rollout log; persistir solo lo necesario para reproducibilidad y métricas.

## Decisión recomendada

Con `references` ya cerrado, el orden es: **Paquete A′ primero** (tres palancas chicas, medibles con la infraestructura de ablations que ya existe — es la continuación natural del ciclo v6 y alimenta directamente el paper), luego hooks audit-only (B′) para observabilidad, luego la escala de inventario (C′) cuando haya un caso MCP real conectado, y skills locales (D′) solo con cuerpos SLM-native y allowlist.

El criterio rector no cambia: **si una extensión no puede demostrarse con una suite y una ablation, no debe convertirse en default.** Y su corolario del tercer estudio: si el harness va a cortar algo, que lo avise primero — los cortes silenciosos son fallos de modelo solo en apariencia.
