# Auditoría v4: braze como motor agéntico SLM-first

Fecha: 2026-07-08  
Proyecto: `braze`  
Base observada: commit `69a4527`, con worktree sucio y cambios locales previos no normalizados.  
Verificación ejecutada: `cargo test --workspace` pasó en el árbol actual.

## Resumen ejecutivo

`braze` ya no está en estado MVP. El motor tiene una orientación clara hacia modelos pequeños: rescates textuales para tool calls imperfectos, paginación de archivos, compacción táctica, post-check de ediciones Rust, permisos conservadores, planner opcional, backend líder en CLI, prompt caching en OpenRouter y un harness de benchmarks con ablations.

La brecha principal para convertirlo en "el mejor software agéntico especializado en modelos pequeños" ya no es añadir otro parser o otro prompt. La brecha está en tres capas:

1. **Gobernanza de ejecución**: presupuestos por turno, coste, latencia, iteraciones, checkpoints y rollback.
2. **Medición realista**: el bench todavía no puede representar la arquitectura completa que el CLI sí usa, especialmente `+lead:`, y sus tareas son demasiado pequeñas para medir auto-mejora real.
3. **Arquitectura editable por agentes**: `engine.rs` concentra demasiadas responsabilidades, lo que hace que cada cambio serio sea caro para modelos pequeños y arriesgado para humanos.

La recomendación es priorizar primero medición y seguridad, luego especialización por familia de modelo, y finalmente refactor estructural. Refactorizar antes de poder medir podría esconder regresiones.

## Alcance de la auditoría

Se revisaron:

- Estructura del workspace y crates.
- Estado persistido del proyecto.
- Documentos previos de auditoría y auto-mejora.
- Loop agéntico en `braze-engine`.
- Superficie de herramientas locales.
- Backends de modelo y wire formats.
- Configuración por defecto.
- Sistema de permisos.
- Harness `braze-bench`.
- Evidencia de pruebas recientes.

No se corrigió código en esta auditoría. Este documento queda como especificación de trabajo para las siguientes rondas de desarrollo.

## Estado actual del motor

### Fortalezas ya presentes

El motor ya tiene varias decisiones correctas para modelos pequeños:

- `read_file` pagina por defecto y devuelve instrucciones de continuación.
- `write_file` advierte cuando una escritura reduce mucho el tamaño del archivo.
- `edit_file` tiene comportamiento tolerante con coincidencias no exactas.
- Hay rescates para llamadas textuales de herramientas en formatos tipo Qwen tagged, Qwen3 XML, GLM arg tags y formas pythonic.
- Hay prompts específicos para la familia Qwen.
- La compacción táctica escala cuando no hay presupuesto explícito de contexto.
- OpenRouter recoge tokens de cache write/read.
- `EscalatingBackend` existe en el CLI para combinar backend pequeño y backend líder.
- El CLI expone `--planner`, `--lead` y `--supervised`.
- `braze-bench` ya mide rondas, llamadas a herramientas, fallos de schema, fallos de ejecución, denegaciones, planificación y pass/fail.
- Los permisos son conservadores: shell deny-by-default, MCP irreversible por defecto, modo supervisado.

### Riesgos persistentes

Las fallas restantes son de producto de investigación serio, no de acabado superficial:

- El bench no puede reproducir la configuración `lead + executor`, por lo que no mide el modo más prometedor para modelos pequeños.
- El motor acumula tokens y uso, pero no impone circuit breakers por coste, tokens o rondas.
- Algunos consumos degradados no quedan contados como uso.
- Una escritura destructiva de archivo completo puede ocurrir antes de que el modelo vea la advertencia.
- Las tareas del bench son demasiado sintéticas para medir auto-mejora multiarchivo.
- `engine.rs` supera las 7.000 líneas y mezcla loop, planificación, rescate, dispatch, presupuestos, compacción, parsing y tests.

## Hallazgos priorizados

### P0.1 - `braze-bench` no soporta `+lead:` y no puede medir el modo más importante

**Evidencia**

- `crates/braze-bench/src/backend_spec.rs` tiene campo `planner`, pero no campo `lead`.
- El parser reconoce `+plan:`, no `+lead:`.
- `display_name()` formatea planner, no líder.
- `runner.rs` construye executor y planner, pero no compone `EscalatingBackend`.
- `docs/self-improvement-exercises.md` define SI-2 como el ejercicio pendiente: añadir `+lead:` al bench.
- `docs/usability-log-2026-07-07-si2.md` registra varios intentos fallidos de modelos pequeños sobre esta tarea.

**Impacto**

El CLI ya tiene una capacidad central para SLM-first: usar un ejecutor pequeño con un líder más fuerte. Pero el bench no la puede evaluar. Esto bloquea comparaciones A/B entre:

- ejecutor pequeño solo,
- ejecutor pequeño + planner,
- ejecutor pequeño + líder,
- ejecutor pequeño + planner + líder.

Sin esa medición, no se puede optimizar la arquitectura agéntica de forma empírica.

**Arreglo recomendado**

Implementar `+lead:<BackendSpec>` en `BackendSpec` con simetría respecto a `+plan:`:

- Añadir `lead: Option<Box<BackendSpec>>`.
- Parsear sufijo `+lead:` anidado.
- Incluirlo en `display_name()`.
- Añadir `build_lead()`.
- En `runner.rs`, envolver el executor con `EscalatingBackend` cuando exista lead.
- Rechazar combinaciones ambiguas con errores claros.
- Añadir tests de parseo, display y ejecución.

**Criterio de aceptación**

Debe funcionar una invocación tipo:

```text
openrouter/model-ejecutor+lead:openrouter/model-lider
```

y el nombre mostrado debe preservar ambos backends.

### P0.2 - Falta un presupuesto duro por turno: coste, tokens, rondas y tiempo

**Evidencia**

- `crates/braze-engine/src/engine.rs` define `MAX_TURN_ITERATIONS = 20`.
- El motor emite `AgentEvent::Usage`, pero no detiene por coste acumulado.
- `crates/braze-events/src/event.rs` registra tokens, cache write/read y stop reason, pero no coste normalizado ni fase.
- En el fallback `attempt_tools_free_summary_round`, el comentario indica que `Usage` se omite para esa ronda degradada.
- `CLAUDE.md` documenta una sesión de investigación con cientos de miles de tokens y menciona la idea de un `maxCost`.

**Impacto**

Los modelos pequeños pueden entrar en ciclos largos de lectura, reintento y rescate. Sin presupuesto duro, el motor puede:

- gastar demasiado con backends cloud,
- degradar benchmarks por outliers,
- ocultar rondas degradadas en métricas,
- dificultar la comparación entre configuraciones,
- repetir herramientas sin que exista una política de parada explícita.

**Arreglo recomendado**

Crear un `TurnBudget` explícito:

- `max_rounds`,
- `max_input_tokens`,
- `max_output_tokens`,
- `max_estimated_cost_usd`,
- `max_wall_time`,
- `max_tool_calls`,
- `max_repeated_tool_calls`.

Integrarlo en:

- `EngineBuilder`,
- config TOML,
- CLI flags,
- `braze-bench`,
- eventos de fin de turno.

El presupuesto debe cortar con una respuesta honesta y útil, no con pánico interno.

**Criterio de aceptación**

Un turno que supere el presupuesto debe terminar con un evento estructurado, un mensaje final claro y métricas completas de lo consumido hasta el corte.

### P0.3 - Las escrituras destructivas de archivo completo se advierten después de escribir

**Evidencia**

- `crates/braze-tools-local/src/write_file.rs` escribe el contenido y luego añade una advertencia si la nueva versión reduce mucho el tamaño.
- El umbral `SHRINK_WARNING_THRESHOLD_BYTES` está en 500 bytes.
- `docs/usability-log-2026-07-07-si2.md` registra una reescritura destructiva de `backend_spec.rs` durante SI-2.

**Impacto**

Para modelos pequeños, el patrón de fallo más peligroso no es equivocarse en un resumen. Es hacer un overwrite total con una versión incompleta. La advertencia posterior ayuda al modelo a corregir, pero el daño ya ocurrió.

**Arreglo recomendado**

Convertir escrituras riesgosas en preflight:

- Si el archivo existe y la escritura reduce más de N bytes o N%, devolver un error recuperable antes de escribir.
- Exigir un campo explícito como `allow_shrink: true` o `expected_previous_sha256`.
- En modo supervisado, mostrar diff antes de ejecutar.
- Crear checkpoint automático antes de escrituras destructivas.
- Preferir `edit_file` cuando el modelo haya leído una región acotada.

**Criterio de aceptación**

Una llamada accidental a `write_file` que reemplace un archivo grande por un stub debe fallar antes de modificar disco, salvo confirmación explícita.

### P0.4 - El bench no mide tareas de auto-mejora reales

**Evidencia**

- `crates/braze-bench/src/task.rs` permite prompts, archivos de setup y asserts simples.
- Las suites actuales contienen tareas pequeñas, mayormente de un archivo.
- SI-2 sigue fuera del flujo normal de benchmark.
- Las métricas actuales no capturan rescates, escalaciones, cache hits, coste ni solapamiento de lecturas.

**Impacto**

Un modelo puede verse bueno en tareas pequeñas y fallar en cambios reales multiarchivo. Para especialización SLM-first, el benchmark debe medir precisamente:

- navegación en código grande,
- edición incremental,
- preservación de APIs,
- uso correcto de planner/líder,
- capacidad de recuperarse de schema/tool-call imperfecto,
- coste por tarea,
- número de rondas hasta converger.

**Arreglo recomendado**

Crear una suite `self_improvement.toml` o `coding_realistic.toml` con tareas como:

- SI-2 `+lead:` en `braze-bench`,
- añadir métrica de cache tokens,
- añadir parser de familia de modelo,
- refactor pequeño de `engine.rs`,
- edición multiarchivo con tests obligatorios,
- tarea con archivo grande que exige paginación correcta.

Extender `TaskDef` con:

- `expect_command_success`,
- `expect_file_not_contains`,
- `expect_max_rounds`,
- `expect_max_tokens`,
- `expect_max_cost_usd`,
- `expect_metric_at_least`,
- `expect_metric_at_most`.

**Criterio de aceptación**

El bench debe poder decir que una configuración es mejor no solo porque pasa, sino porque pasa con menos tokens, menos rondas, menos rescates o menor coste.

### P1.1 - `engine.rs` es el cuello de botella principal para mantenibilidad y auto-edición

**Evidencia**

- `crates/braze-engine/src/engine.rs` tiene más de 7.000 líneas.
- Contiene loop principal, streaming, rescates, best-of-n, planner, dispatch, validación, compacción, presupuestos, parsing auxiliar y tests.
- Modelos pequeños ya tuvieron dificultades en SI-2, que era mucho más pequeño que `engine.rs`.

**Impacto**

El archivo concentra demasiada carga cognitiva. Para humanos retrasa revisión; para modelos pequeños aumenta:

- lecturas redundantes,
- ediciones por overwrite,
- pérdida de contexto local,
- probabilidad de romper invariantes,
- dificultad para construir patches mínimos.

**Arreglo recomendado**

Refactorizar por módulos internos, manteniendo API pública:

- `turn_loop.rs`: `run_turn`, convergencia y fallback.
- `completion.rs`: streaming, usage, truncation, best-of-n.
- `rescue.rs`: extracción textual de tool calls.
- `dispatch.rs`: validación, coerción, ejecución y timeouts.
- `budget.rs`: estimación y enforcement de presupuestos.
- `planning.rs`: planner round y prompt de planificación.
- `memory.rs` o conservar `history.rs` para compacción y carga.

No hacer este refactor como primer cambio. Primero añadir medición para detectar regresiones.

### P1.2 - La especialización por familia de modelo está partida entre prompts y rescates

**Evidencia**

- `braze-config/src/prompt.rs` tiene hints explícitos para Qwen/Qwen3.
- El rescate de `engine.rs` reconoce también GLM y Llama-like pythonic calls.
- La selección de rescue ladder es universal, no declarativa por familia.

**Impacto**

El motor es reactivo: intenta rescatar formatos después de que el modelo falla. Para modelos pequeños conviene ser proactivo:

- prompt específico por familia,
- ejemplos mínimos por familia,
- schema verbosity adaptativa,
- parser preferente por familia,
- thresholds de compacción y salida por familia.

**Arreglo recomendado**

Crear un registro de familias de modelo:

```rust
enum ModelFamily {
    Qwen,
    Qwen3Coder,
    GLM,
    Llama,
    Mistral,
    Generic,
}
```

Cada familia debería definir:

- prompt hints,
- formato preferido de tool call,
- rescue parsers habilitados y ordenados,
- valores por defecto de `max_tokens`,
- tolerancia a schema repair,
- recomendación de planner/lead.

**Criterio de aceptación**

Un backend debe declarar o inferir su familia una sola vez, y esa decisión debe afectar tanto prompts como parsing y métricas.

### P1.3 - El fallback de resumen consume tokens sin contabilidad completa

**Evidencia**

- `attempt_tools_free_summary_round` llama al modelo sin herramientas cuando el turno no converge.
- El código omite registrar `Usage` para esa ronda.
- Usa `self.max_tokens` completo para el resumen degradado.

**Impacto**

Esto subestima coste y tokens justo en los casos problemáticos. Además, una ronda de resumen no debería poder consumir tantos tokens como una ronda normal de trabajo.

**Arreglo recomendado**

- Registrar `Usage` también para summary fallback.
- Añadir `phase` o evento específico para distinguir `work`, `planner`, `summary`, `rescue`.
- Limitar `max_tokens` de summary a un valor menor, por ejemplo `min(max_tokens, 768)` configurable.

### P1.4 - `best_of_n` es secuencial y puede multiplicar latencia

**Evidencia**

- `complete_with_best_of_n` itera candidatos en un bucle secuencial.
- El CLI advierte cuando se usa con Ollama, pero el coste de latencia sigue siendo estructural.

**Impacto**

`best_of_n` puede ser útil para modelos pequeños, pero si se ejecuta secuencialmente convierte una técnica de robustez en una penalización fuerte de latencia.

**Arreglo recomendado**

- Definir política por backend: secuencial para local si se quiere ahorrar memoria, concurrente acotado para cloud si se quiere menor latencia.
- Añadir `best_of_n_concurrency`.
- Medir coste y pass rate por candidato.
- Emitir métrica de candidato ganador.

### P1.5 - No existe snapshot explícito del catálogo de herramientas por ronda

**Evidencia**

- `ToolRegistry::all_stubs_lossy()` reconstruye stubs.
- `dispatch()` vuelve a resolver provider/schema.
- Los stubs MCP pueden no tener schema resuelto inicialmente.

**Impacto**

El modelo ve un catálogo; el dispatch resuelve de nuevo. Eso funciona, pero para auditoría, replay y benchmarks conviene que cada ronda tenga un `ToolCatalog` explícito, con:

- herramienta,
- owner/provider,
- schema usado,
- tamaño en tokens/bytes,
- clasificación de efecto.

**Arreglo recomendado**

Crear un snapshot por ronda y pasarlo tanto al modelo como al dispatch. Esto facilita:

- reproducibilidad,
- métricas de tool surface,
- cache de schemas,
- detección de colisiones,
- reducción de tokens por herramientas.

### P1.6 - El post-edit check es Rust-only

**Evidencia**

- `post_edit_check.rs` ejecuta `cargo check` solo para archivos `.rs` dentro de proyectos Cargo.

**Impacto**

Muy útil para este repo, pero limitado como motor agéntico general. Un agente SLM-first necesita feedback barato y automático para cualquier stack.

**Arreglo recomendado**

Añadir validadores configurables por repositorio:

- `cargo check`,
- `cargo test -q`,
- `npm test`,
- `pnpm lint`,
- `pytest`,
- `ruff`,
- comandos definidos en config.

Debe tener timeout, límite de salida y permiso claro.

### P2.1 - `read_file` pagina la salida, pero lee el archivo completo en memoria

**Evidencia**

- `read_file.rs` usa lectura completa antes de seleccionar líneas.

**Impacto**

No es crítico para repos normales, pero puede ser malo para logs, datasets o archivos generados grandes.

**Arreglo recomendado**

Implementar lectura streaming por líneas o límite de bytes inicial. Mantener la interfaz actual.

### P2.2 - Defaults locales no están alineados con el perfil SLM-first

**Evidencia**

- La config por defecto usa Ollama con `llama3.1`.
- `ollama_num_ctx` default es 8192.
- `max_tokens` default es 4096.
- Hay validación que advierte cuando `max_tokens` puede dejar poco presupuesto para prompt.

**Impacto**

Un default que dispara advertencias no es ideal como experiencia inicial. Además, `llama3.1` no necesariamente representa el mejor modelo local para tool use/coding.

**Arreglo recomendado**

Introducir perfiles:

- `small-local-safe`,
- `small-local-coding`,
- `cloud-leader`,
- `bench-slm`,
- `research-expensive`.

Para local SLM-first:

- reducir `max_tokens` por defecto,
- recomendar familias Qwen coder cuando estén disponibles,
- habilitar hints por familia,
- sugerir planner/lead cuando el modelo base sea débil.

### P2.3 - MCP se clasifica como irreversible por defecto, pero falta taxonomía fina

**Evidencia**

- La política actual trata MCP como irreversible por seguridad.

**Impacto**

Es una buena postura inicial. Pero para un motor de alta calidad, conviene distinguir:

- lectura pura,
- red,
- escritura,
- shell,
- acción irreversible,
- acción costosa.

**Arreglo recomendado**

Mantener unknown MCP como irreversible, pero permitir metadata declarativa para MCPs confiables. Esto reduce fricción sin relajar seguridad por defecto.

### P2.4 - Algunos límites de ejecución son constantes internas

**Evidencia**

- `MAX_TURN_ITERATIONS` está hardcodeado.
- El timeout de herramientas existe, pero no parece estar expuesto de forma completa en config/CLI.

**Impacto**

Los perfiles SLM-first necesitan límites distintos según:

- modelo local,
- backend cloud,
- bench,
- modo supervisado,
- tarea de investigación.

**Arreglo recomendado**

Exponer estos límites en config y bench:

- iteraciones máximas,
- timeout por herramienta,
- timeout total por turno,
- rondas máximas sin progreso,
- repeticiones máximas de una misma llamada.

## Roadmap recomendado

### Fase 1 - Medición y harness

Objetivo: poder evaluar cambios sin intuición.

Tareas:

- Implementar `+lead:` en `braze-bench`.
- Añadir métricas de cache write/read tokens al bench.
- Añadir métricas de rescate textual.
- Añadir métricas de compacción.
- Añadir métricas de escalación a líder.
- Añadir coste estimado cuando el backend tenga pricing conocido.
- Crear suite `self_improvement`.
- Convertir SI-2 en benchmark permanente.

Resultado esperado:

`braze-bench` debe comparar configuraciones SLM-first reales, no solo backends aislados.

### Fase 2 - Seguridad para auto-mejora

Objetivo: permitir que modelos pequeños editen el propio motor con menor riesgo.

Tareas:

- Preflight para `write_file` destructivo.
- Checkpoint antes de cambios grandes.
- Diff preview en modo supervisado.
- `TurnBudget` con corte explícito.
- Validadores configurables por repo.
- Política de "sin progreso" basada en repeticiones de herramientas y observaciones equivalentes.

Resultado esperado:

Un fallo típico de modelo pequeño debe terminar en una salida recuperable, no en overwrite silencioso ni gasto no acotado.

### Fase 3 - Especialización por familias de modelo

Objetivo: pasar de rescate reactivo a orquestación proactiva.

Tareas:

- Crear `ModelFamily` compartido.
- Unificar prompts y rescues por familia.
- Añadir perfiles de config SLM-first.
- Ajustar `max_tokens` por fase y familia.
- Registrar en métricas qué familia y qué rescue se usó.
- Medir Qwen, GLM, Llama y genéricos con las mismas suites.

Resultado esperado:

El motor debe saber cómo hablarle a cada familia pequeña y cómo interpretar sus errores esperables.

### Fase 4 - Refactor de arquitectura

Objetivo: reducir el coste cognitivo de cambios futuros.

Tareas:

- Extraer `completion.rs`.
- Extraer `rescue.rs`.
- Extraer `dispatch.rs`.
- Extraer `budget.rs`.
- Extraer `planning.rs`.
- Mover tests grandes a módulos por responsabilidad.
- Introducir `ToolCatalog` por ronda.

Resultado esperado:

El motor debe ser más fácil de auditar, cambiar y mejorar por agentes pequeños.

### Fase 5 - Investigación y paper trail

Objetivo: convertir las mejoras en evidencia defendible.

Tareas:

- Benchmarks por familia de modelo.
- Ablations: sin rescate, sin planner, sin lead, sin compacción, sin preflight.
- Curvas coste/pass-rate.
- Curvas rondas/pass-rate.
- Registro de fallos cualitativos.
- Reporte reproducible por commit.

Resultado esperado:

El proyecto podrá afirmar de forma empírica qué decisiones hacen mejor a un motor agéntico para modelos pequeños.

## Métricas nuevas recomendadas

Añadir al bench y/o eventos:

- `cache_write_tokens`.
- `cache_read_tokens`.
- `estimated_cost_usd`.
- `rescued_tool_calls`.
- `rescue_parser_name`.
- `tool_schema_bytes`.
- `tool_catalog_count`.
- `compaction_count`.
- `planner_rounds`.
- `leader_escalations`.
- `summary_fallbacks`.
- `turn_budget_exhausted`.
- `repeated_tool_call_count`.
- `read_file_overlap_ratio`.
- `write_file_destructive_preflights`.
- `post_edit_check_failures`.
- `winning_best_of_n_candidate`.

Estas métricas importan porque los modelos pequeños no fallan solo en pass/fail. Fallan por gastar demasiado, repetir lecturas, producir formatos imperfectos, truncarse, o editar demasiado.

## Cambios concretos sugeridos por archivo

### `crates/braze-bench/src/backend_spec.rs`

- Añadir `lead`.
- Parsear `+lead:`.
- Rechazar sufijos mal formados.
- Añadir tests de combinaciones:
  - executor solo,
  - executor + planner,
  - executor + lead,
  - executor + planner + lead,
  - nesting inválido.

### `crates/braze-bench/src/runner.rs`

- Construir backend líder.
- Envolver executor con `EscalatingBackend`.
- Emitir en metadata de benchmark que hay líder activo.

### `crates/braze-bench/src/metrics.rs`

- Incorporar cache tokens.
- Incorporar rescue counters.
- Incorporar compacciones.
- Incorporar coste estimado.
- Separar usage por fase si los eventos lo permiten.

### `crates/braze-engine/src/engine.rs`

- Corto plazo: registrar usage de summary fallback y limitar `max_tokens`.
- Medio plazo: introducir `TurnBudget`.
- Medio plazo: emitir eventos explícitos para rescue, compaction y budget exhaustion.
- Largo plazo: extraer módulos.

### `crates/braze-events/src/event.rs`

- Añadir fase a usage o evento separado:
  - `work`,
  - `planner`,
  - `summary`,
  - `leader`,
  - `rescue`.
- Añadir eventos de presupuesto agotado y rescue aplicado.

### `crates/braze-tools-local/src/write_file.rs`

- Cambiar warning post-hoc por preflight.
- Añadir campo de confirmación explícita para shrink.
- Considerar hash/tamaño observado para evitar overwrites basados en contexto viejo.

### `crates/braze-tools-local/src/read_file.rs`

- Mantener paginación.
- Evaluar streaming para archivos grandes.
- Añadir métrica de rangos leídos para detectar lecturas repetidas.

### `crates/braze-config/src/prompt.rs`

- Generalizar familias más allá de Qwen.
- Separar hints por familia.
- Hacer que el backend exponga familia inferida.

### `crates/braze-config/src/config.rs`

- Añadir perfiles SLM-first.
- Exponer límites de turno.
- Revisar default `max_tokens` para Ollama.

## Primer paquete de trabajo recomendado

El primer paquete debe ser pequeño, medible y directamente conectado con el bloqueo actual:

1. Implementar `+lead:` en `braze-bench`.
2. Convertir SI-2 en benchmark permanente.
3. Añadir métricas mínimas de escalación.
4. Ejecutar matriz:
   - executor pequeño solo,
   - executor pequeño + planner,
   - executor pequeño + lead,
   - executor pequeño + planner + lead.
5. Documentar resultados en `docs/usability-log-2026-07-07-si2.md` o un log nuevo.

Este paquete desbloquea la capacidad de decidir si el liderazgo realmente mejora a modelos pequeños en tareas de código real.

## Segundo paquete de trabajo recomendado

Una vez medido `+lead:`, proteger auto-edición:

1. Preflight de `write_file` destructivo.
2. Checkpoint antes de escrituras grandes.
3. `TurnBudget` básico por rondas y tokens.
4. Usage completo para summary fallback.
5. Validadores repo-level configurables.

Este paquete reduce el riesgo operacional antes de ampliar la autonomía.

## Tercer paquete de trabajo recomendado

Después de seguridad y medición:

1. `ModelFamily` compartido.
2. Prompt hints para GLM y Llama además de Qwen.
3. Rescue ladder condicionada por familia.
4. Métrica de parser usado.
5. Perfiles `small-local-coding` y `bench-slm`.

Este paquete convierte conocimiento empírico en comportamiento del motor.

## Definición de "mejor software agéntico SLM-first"

Para este proyecto, "mejor" debería significar:

- Pasa tareas reales de edición multiarchivo con modelos pequeños.
- Usa modelos líderes solo cuando aportan valor medible.
- Tiene costes y límites explícitos.
- Evita daños destructivos por errores típicos de SLMs.
- Genera métricas suficientes para mejorar por evidencia.
- Mantiene una arquitectura que un agente pequeño puede entender y modificar.
- Soporta familias de modelos con comportamiento diferenciado.
- Es reproducible en benchmarks y seguro en uso interactivo.

## Criterios de salida para la próxima versión fuerte

Una versión candidata debería cumplir:

- `cargo test --workspace` pasa.
- `cargo clippy --workspace --all-targets -- -D warnings` pasa o las excepciones están documentadas.
- `braze-bench` acepta `+lead:`.
- Existe suite de self-improvement con al menos SI-2.
- El bench reporta cache tokens y escalaciones.
- `write_file` no puede reducir drásticamente un archivo existente sin confirmación explícita.
- Hay presupuesto por turno configurable.
- El fallback summary registra usage.
- Existe al menos una matriz publicada comparando modelos pequeños con y sin líder.

## Conclusión

`braze` ya tiene el núcleo defensivo correcto para trabajar con modelos pequeños. La siguiente frontera no es más tolerancia suelta, sino control: presupuestos, medición, benchmarks realistas, prevención de daño y arquitectura modular. Si se ejecuta el roadmap en ese orden, el proyecto puede pasar de "motor que ayuda a SLMs a no romperse" a "plataforma experimental sólida para hacer que SLMs trabajen por encima de su tamaño".
