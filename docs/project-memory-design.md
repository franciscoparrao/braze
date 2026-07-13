# Diseño: memoria de proyecto persistente entre sesiones (`ProjectMemory`)

Fecha: 2026-07-13
Estado: **IMPLEMENTADO — alcance V1 completo, verificado en vivo contra
`gpt-oss:20b` en Nitro (sesión-1 escribe, sesión-2 lee correctamente
desde el prompt inyectado).** Ver `PLAN.md` § "ProjectMemory
implementado" para el detalle de qué se construyó, los 39 tests nuevos,
y los 3 smokes. Ninguna decisión de este documento se revirtió durante
la implementación — el crate `braze-memory`, el hook sobre `EngineHook`,
la inyección vía el parámetro `environment`-hermano de
`default_system_prompt`, y `.braze/memory.json` versionable en el repo
son exactamente lo que § "Arquitectura propuesta" y § "mejor opción
para nuestra configuración" recomendaban. Origen: la pregunta del
usuario sobre si `braze` podía tener el mismo sistema de contexto
persistente que `~/.claude/session_state/` (context_manager.py) usa
para las sesiones de Claude Code en este mismo proyecto.

## Por qué esto NO es lo que `SessionStore` ya hace

`braze` ya persiste sesiones (`SessionStore`, JSONL append-only,
`--resume <id>`) — pero eso es *replay de una conversación puntual*, no
*memoria de proyecto entre conversaciones distintas*. Son capas
diferentes y no se solapan:

| | `SessionStore` (ya existe) | `ProjectMemory` (esto) |
|---|---|---|
| Qué guarda | Cada evento de UNA sesión | Un resumen curado del PROYECTO |
| Cuándo se lee | Al hacer `--resume <id>` (misma conversación) | Al EMPEZAR una sesión nueva (conversación distinta) |
| Forma | Log de eventos, replay exacto | Estado estructurado, capado, con reglas de merge |
| Analogía | El historial de mensajes de este chat | `PLAN.md`/`CLAUDE.md` — lo que este mismo proyecto ya usa como memoria persistida, mantenida a mano |

De hecho, `PLAN.md` y `CLAUDE.md` de este mismo repo **ya son** una
versión manual de exactamente esto — curados por un modelo grande
(yo) sesión a sesión. La pregunta real es: ¿puede un *executor chico
corriendo solo* (`gpt-oss:20b`, sin supervisión) mantener su propio
equivalente, automatizado?

## Por qué copiar mi propio sistema tal cual sería el diseño equivocado

`~/.claude/session_state/context_manager.py` funciona porque quien lo
opera (yo) es un modelo grande que puede: decidir qué es "importante"
sin reglas explícitas, escribir resúmenes de prosa coherentes, y
aplicar juicio de merge no trivial ("¿esto ya está cubierto por una
entrada existente?"). Ese es precisamente el tipo de tarea que este
mismo proyecto ya midió que un executor chico hace mal:

- El plan-en-prosa como texto libre **degenera** en ambos extremos de
  escala (`docs/sweep-planner-ab-2026-07-11.md`) — un executor chico
  (y hasta uno grande, mal renderizado) no maneja bien "acá tenés tu
  propio texto largo, seguí razonando sobre él".
- La lista de tareas **tipada** (estado, no prosa) es la que rescata al
  3B — la estructura gana sobre la prosa libre exactamente cuando el
  modelo es chico.
- A-MAC (`docs/SOTA-2026-07.md` adenda): una señal determinística
  simple para decidir qué compactar supera a señales aprendidas — la
  razón por la que el compactor de `braze-session` es 100%
  determinístico y no usa LLM, decisión ya validada en este proyecto.

Conclusión de diseño: **la parte determinística de la memoria debe
construirse sin pedirle nada al executor**, y la parte que sí requiere
juicio (escribir 2 líneas de resumen, decidir qué es "clave") debe
delegarse al lead/planner si hay uno configurado — no al executor
chico corriendo solo. Copiar mi propio sistema (que asume un modelo
grande operándolo) sería repetir el error que el propio `braze` ya
diagnosticó y corrigió con la task list tipada.

## Arquitectura propuesta

Tres piezas nuevas, componiendo con los tres traits congelados sin
tocarlos.

### 1. `ProjectMemory` — el dato

```rust
// nuevo crate pequeño: braze-memory (o módulo en braze-session)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectMemory {
    pub project_key: String,          // ver § keying abajo
    pub objective: Option<String>,     // 1 línea, escrita una vez
    pub touched_files: Vec<TouchedFile>,   // capado, determinístico
    pub completed_signals: Vec<CompletedSignal>, // capado, determinístico
    pub notes: Option<String>,         // opcional, delegado al lead
    pub meta: MemoryMeta,              // checksum, updated_at, version
}

pub struct TouchedFile { pub path: String, pub last_tool: String, pub at: String }
pub struct CompletedSignal { pub description: String, pub at: String, pub source: SignalSource }
pub enum SignalSource { ToolCallPattern, TaskListCompletion, LeadSummary }
```

Deliberadamente MÁS chico que mi propio esquema (sin
`key_decisions`/`errors_resolved` con juicio narrativo en V1) — esos
campos existen en mi sistema porque yo los puedo llenar bien; acá
empiezan vacíos hasta que el paso 3 (delegación al lead) los habilite.

### 2. `ProjectMemoryHook` — la captura, gratis y determinística

Reutiliza el sistema de hooks ya existente (Paquete B′,
`crates/braze-engine/src/hooks.rs`, `EngineHook::on_event`) — el mismo
patrón que `PromptBudgetAuditHook`, sin tocar el trait ni el engine:

```rust
struct ProjectMemoryHook { store: Arc<dyn ProjectMemoryStore>, key: String }

#[async_trait]
impl EngineHook for ProjectMemoryHook {
    fn id(&self) -> &str { "project-memory" }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), String> {
        match event {
            AgentEvent::ToolCallCompleted { name, arguments, .. }
                if matches!(name.as_str(), "write_file" | "edit_file") =>
            {
                // extraer el path, appendear a touched_files, guardar
            }
            _ => {}
        }
        Ok(())
    }
}
```

Costo real: cero llamadas a modelo. Cada `write_file`/`edit_file`
exitoso ya pasa por `on_event` — solo hay que leer el evento que ya
existe y escribirlo a disco. Mismo espíritu que el compactor
determinístico que A-MAC validó.

**Gap real detectado al diseñar esto**: la task list tipada
(`crates/braze-engine/src/task_list.rs`) vive en memoria del `Engine`
y se resetea por turno (J-4) — sus tareas marcadas `done` NO llegan
al event log como su propio evento, así que un hook no puede leerlas
gratis todavía. Cerrar esto es un cambio chico y aislado: emitir
`AgentEvent::TaskCompleted { description }` cuando `task_update` marca
`done` (el propio módulo ya anota "deliberadamente NO persistido como
eventos propios en v1" — esto sería el v2 que ese comentario anticipa).
Sin ese evento, V1 de `ProjectMemory` solo captura `touched_files`, no
`completed_signals` vía task list — un límite honesto, no bloqueante.

### 3. Inyección — reusa el seam que ya existe, no inventa uno nuevo

`braze_config::default_system_prompt` ya tiene un parámetro
`environment: Option<&str>` que renderiza una sección
`"Environment:\n{snapshot}"` al inicio del prompt — exactamente el
punto para una sección `"Project memory:\n{resumen}"` hermana, no una
API nueva. Renderizado UNA vez al abrir una sesión nueva (no
re-inyectado cada ronda como la task list, porque no es estado que
cambie turno a turno) y con presupuesto de tokens acotado — mismo
espíritu que `ollama_context_budget_tokens` ya aplica al resto del
prompt.

```rust
pub fn render_project_memory_section(memory: &ProjectMemory, budget_tokens: usize) -> String {
    // objective (si existe) + hasta N touched_files más recientes +
    // hasta M completed_signals — capado, trunca por antigüedad, nunca
    // por corte de texto a la mitad
}
```

### 4. `project_key` — mismo problema que mi propio sistema ya resuelve

Orden de detección (idéntico al de `context_manager.py`, adaptado):
1. Raíz de git del cwd (`git rev-parse --show-toplevel`) si existe.
2. Si no hay git, el cwd absoluto tal cual.

## La pregunta de "mejor opción para nuestra configuración"

Tres decisiones de diseño, cada una con su trade-off — la
recomendación es la combinación marcada.

### (a) ¿Dónde vive el archivo?

| Opción | A favor | En contra |
|---|---|---|
| **`.braze/memory.json` dentro del repo** ✅ recomendado | Versionable, compartible entre máquinas/usuarios del mismo repo, mismo patrón que `PLAN.md`/`CLAUDE.md` — que este mismo proyecto ya usa y valida | Puede filtrar notas del agente al historial de git si no se cura; requiere `.gitignore` opt-out para quien no lo quiera versionado |
| Global (`~/.local/share/braze/memory/<hash>.json`, como `session_dir`) | Privado por default, cero riesgo de filtrar al repo | Se pierde si cambiás de máquina; no compartible en equipo; menos alineado con la cultura de este proyecto (todo lo importante vive versionado — PLAN.md, docs/sweep-*.md) |

**Por qué (a) para esta configuración específica**: `braze` ya es un
proyecto que trata la documentación persistente como código —
versionada, revisada, citable por commit. Un `.braze/memory.json`
versionado es la extensión natural de esa cultura, no una excepción. Default configurable (mismo patrón que `session_dir` en `Config`) para quien prefiera privado.

### (b) ¿Captura 100% determinística, o con resumen de prosa opcional?

**V1 solo determinística** ✅ recomendado — cero riesgo de que el
executor chico degenere escribiendo su propio resumen (el modo de
falla ya documentado en este proyecto). `notes`/`objective` quedan
`None` hasta que exista un paso explícito, separado, que los llene.

**V2 opcional, delegado al lead** — si hay `--lead` configurado
(ya existe en `braze`), un comando explícito (`braze memory summarize`
o similar, NO automático) le pide al lead — no al executor — que
escriba `objective`/`notes` a partir de los `touched_files` +
`completed_signals` ya acumulados determinísticamente. Sin lead
configurado, ese comando simplemente no está disponible — no hay
fallback al executor chico escribiendo prosa libre, por la misma razón
que el plan-en-prosa daña cuando el modelo no está a la altura.

### (c) ¿Palanca del bench desde el día uno, o feature de producción primero?

**Palanca del bench desde el día uno** ✅ — mismo patrón que
`task-list`/`no-lead`/etc.: `+ablate:project-memory` (enabling key,
off por default, mismo caso documentado que `task-list`). Sin esto, la
pregunta "¿esta memoria realmente ayuda a un executor chico, o es
overhead de tokens sin payoff?" queda sin medir — exactamente el tipo
de asunción que este proyecto ya evitó dos veces este año (planner,
tool deferral).

**Complicación real, anotada**: medir el VALOR de memoria
*entre sesiones* necesita un suite multi-turno/multi-sesión que
`braze-bench` hoy no tiene — cada tarea es una sesión aislada. Esto ya
está anotado como brecha independiente en el roadmap v7 (Paquete 2-4,
"multi-turno"). `ProjectMemory` no puede medirse en serio hasta que esa
brecha se cierre, o hasta diseñar un suite ad-hoc de 2+ sesiones
encadenadas sobre el mismo repo — sea el A/B específico de esta
palanca, sea compartido con el trabajo de multi-turno ya pendiente.

## Alcance V1 (a congelar al arrancar)

Dentro: `ProjectMemory` (struct + store de archivo, `.braze/memory.json`
versionable) + `ProjectMemoryHook` (captura determinística de
`touched_files` vía `EngineHook::on_event`, sin tocar el trait) +
inyección vía el parámetro `environment` ya existente de
`default_system_prompt` + `+ablate:project-memory` en `braze-bench`.

Fuera de V1: `completed_signals` vía task list (requiere el evento
`TaskCompleted` nuevo, anotado arriba como v2 chico); resumen de prosa
delegado al lead (`objective`/`notes`, requiere su propio comando
explícito); medición empírica de valor (requiere el suite multi-turno
del roadmap v7).

## Riesgos / caveats honestos

- Sin el evento `TaskCompleted`, V1 mide "qué archivos se tocaron" pero
  no "qué se logró" — útil pero parcial. Anotarlo así en la
  documentación de la feature, no prometer más.
- `.braze/memory.json` versionado significa que cambia en cada sesión
  con actividad — puede generar ruido de diffs en PRs si no se cura
  (mismo problema que cualquier archivo de estado versionado; mitigar
  con un formato estable/ordenado, no con timestamps de escritura
  ruidosos).
- El presupuesto de tokens de la sección inyectada compite con el
  resto del prompt (tools, historia) — debe medirse con
  `ollama_context_budget_tokens` real, no asumirse gratis.

## Cómo retomar

1. Congelar el alcance V1 de arriba (sin extenderlo).
2. `TaskCompleted` como primer paso chico y aislado en
   `crates/braze-engine/src/task_list.rs` — habilita `completed_signals`
   sin esperar al resto.
3. `braze-memory` (o módulo en `braze-session`): `ProjectMemory` +
   `ProjectMemoryStore` trait + `FileProjectMemoryStore` (mismo patrón
   que `FileSessionStore`, mucho más chico).
4. `ProjectMemoryHook` sobre `EngineHook` — solo `on_event`, sin tocar
   `before_model_request`.
5. `render_project_memory_section` + wiring al parámetro `environment`
   existente en `braze-cli`'s composition root.
6. `+ablate:project-memory` en `braze-bench` — sin medición seria hasta
   que exista el suite multi-turno (anotado como bloqueante para
   *medir*, no para *construir*).
