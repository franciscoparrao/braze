# Plan de desarrollo: modo aprendizaje para `braze`

Fecha: 2026-07-16  
Estado: propuesta de arquitectura, no implementado  
Nombre de trabajo: `learning mode` / memoria procedimental destilada

Relacionado: `docs/future-research-lines-2026-07-16.md` situa este modo como una linea de investigacion posterior al paper actual, junto con metaheuristicas y aprendizaje por refuerzo para politicas del harness.

## Tesis

El modo aprendizaje no debe intentar que el modelo local "aprenda" en sus pesos. Debe convertir una escalacion puntual a un modelo superior en una unidad reutilizable de memoria procedimental: un `playbook` estructurado, verificable, versionado y recuperable por el modelo local en tareas futuras.

La formulacion fuerte es:

> cuando el modelo local falla, `braze` no solo consulta al modelo superior para resolver la tarea; le pide que explique que metodologia deberia haber seguido el modelo local, guarda esa metodologia como candidato y la valida antes de confiar en ella.

Esto encaja con la tesis ya emergente del paper: el harness no fabrica capacidad; enruta capacidad, captura la intervencion costosa y la reutiliza para reducir escalaciones futuras.

## Principios de diseno

1. **Procedimientos, no notas libres.** La unidad persistida debe ser un `LearnedPlaybook` con campos tipados: aplicabilidad, senales de fallo, pasos, verificaciones y condiciones de re-escalacion.
2. **Off por default.** La feature entra como palanca experimental, medible por `braze-bench`, no como comportamiento base.
3. **Candidatos antes que verdad.** Lo producido por OpenRouter nace como `candidate`; solo se inyecta automaticamente cuando fue aprobado o validado por ejecuciones posteriores.
4. **Privacidad explicita.** Enviar transcript, paths o contenido a OpenRouter requiere politica y redaccion; nunca se deben incluir credenciales, `.env`, claves, caches ni datos sensibles.
5. **Retrieval antes de escalacion.** En una tarea futura, `braze` debe buscar playbooks relevantes y darselos al modelo local antes de volver a pagar una escalacion.
6. **Validacion objetiva primero.** El MVP debe limitarse a tareas con checks claros: `cargo check`, tests, schema validation, task success del bench o aserciones de archivos.
7. **Token budget fijo.** Los playbooks se renderizan con presupuesto acotado y truncado por lineas completas, igual que `ProjectMemory`.
8. **No auto-envenenamiento.** Un modelo local no escribe memoria procedimental confiable sobre si mismo. El tutor cloud puede proponer; el harness y/o el usuario aprueban.

## Arquitectura actual que se debe reutilizar

`braze` ya tiene varias piezas que hacen que esta feature sea una extension natural:

| Pieza existente | Uso en modo aprendizaje |
|---|---|
| `braze-model::OpenRouterBackend` | Modelo tutor superior. |
| `braze-model::EscalatingBackend` | Deteccion y enrutamiento reactivo lead/worker ya implementados. |
| `braze-memory::ProjectMemory` | Patron de memoria de proyecto: archivo JSON pequeno, estable y presupuestado. |
| `braze-engine::EngineHook` | Observacion de eventos y requests con timeout y degradacion segura. |
| `braze-engine::ProjectMemoryHook` | Ejemplo de captura deterministica desde `AgentEvent`. |
| `braze-skills` | Precedente conceptual: instrucciones procedimentales con disclosure progresivo. |
| `braze-bench` + `+ablate:*` | Lugar correcto para medir si reduce fallos, rondas, tokens y costo. |

La feature no deberia crear un segundo sistema de memoria general. Debe extender `braze-memory` con una capa procedimental separada de `ProjectMemory`.

## Unidad central: `LearnedPlaybook`

Formato propuesto, inicialmente JSON estable:

```json
{
  "schema_version": 1,
  "id": "rust-borrow-checker-restructure-before-clone",
  "title": "Resolver errores de borrow checker reestructurando ownership antes de clonar",
  "lifecycle": "candidate",
  "task_family": "rust_edit_compile_fix",
  "applies_when": [
    "La tarea modifica codigo Rust",
    "cargo check reporta E0499, E0502 o E0382",
    "El mismo error persiste tras dos intentos locales"
  ],
  "failure_signals": [
    "ToolResult contiene error de compilacion repetido",
    "El modelo propone clones sin inspeccionar el limite de ownership"
  ],
  "preconditions": [
    "Leer la funcion completa antes de editar",
    "Ejecutar cargo check con salida corta despues de cada cambio"
  ],
  "method_steps": [
    "Identificar quien posee el valor y donde se presta mutable/inmutablemente",
    "Reducir el scope del borrow antes de introducir clones",
    "Preferir mover calculos fuera del borrow activo",
    "Aplicar el cambio minimo y verificar"
  ],
  "verification": [
    "cargo check --quiet --message-format=short",
    "test especifico del modulo si existe"
  ],
  "avoid": [
    "Agregar clone como primer reflejo",
    "Editar multiples funciones antes del primer check"
  ],
  "escalate_if": [
    "El mismo error sigue tras dos ediciones verificadas",
    "La correccion exige cambiar API publica"
  ],
  "source": {
    "session_id": "uuid",
    "origin_event": "learning_triggered",
    "tutor_backend": "openrouter",
    "tutor_model": "anthropic/claude-sonnet-5",
    "created_at": "2026-07-16T00:00:00Z"
  },
  "evidence": {
    "created_from_failure": true,
    "validated_runs": 0,
    "failed_runs": 0,
    "last_used_at": null
  }
}
```

### Ciclo de vida

| Estado | Significado | Se inyecta automaticamente |
|---|---|---:|
| `candidate` | Propuesto por tutor, no aprobado ni probado. | No |
| `approved` | Revisado por usuario o comando explicito. | Si, con baja prioridad |
| `validated` | Ayudo en al menos una tarea posterior con verificacion objetiva. | Si |
| `trusted` | Varias validaciones, sin fallos recientes. | Si, prioridad alta |
| `retired` | Obsoleto, erroneo o demasiado especifico. | No |

El MVP debe poder crear `candidate`, listar, mostrar, aprobar, retirar e inyectar solo `approved+`.

## Flujo operacional

### 1. Tarea nueva

1. Usuario pide una tarea.
2. `LearningRetriever` clasifica la tarea con matching deterministico inicial: tags, palabras clave, herramientas esperadas, extension de archivos, errores recientes si existen.
3. Se renderizan hasta `N` playbooks `approved|validated|trusted`, con presupuesto fijo.
4. El modelo local intenta resolver usando esos procedimientos.

### 2. Fallo del modelo local

Un `LearningTrigger` se activa si aparece una combinacion de senales:

- `schema_validation_failed` repetido para la misma tool.
- `ToolResult.is_error = true` repetido con patron similar.
- `max_turn_iterations` alcanzado.
- `TurnBudgetExhausted`.
- escalacion reactiva de `EscalatingBackend`.
- fallo objetivo del bench.
- mismo check externo falla despues de dos ediciones.

El trigger no debe depender de texto libre solamente; puede usar texto para agrupar, pero la decision debe partir de eventos tipados.

### 3. Tutor cloud

Si la politica lo permite, `LearningController` construye un paquete redacted:

- objetivo del usuario;
- ultimos eventos relevantes;
- tool calls fallidas;
- salida de checks;
- archivos tocados, no el repo completo;
- playbooks ya intentados;
- modelo local usado y parametros principales.

El prompt al tutor exige dos salidas separadas:

1. **Intervencion inmediata**: como destrabar la tarea actual.
2. **Playbook generalizado**: metodologia reutilizable en JSON estricto.

### 4. Persistencia

El playbook del tutor se valida contra schema JSON. Si pasa:

- se guarda como `candidate` en `.braze/playbooks/candidates/<id>.json`;
- se emite `AgentEvent::PlaybookCandidateSaved`;
- se muestra al usuario o queda para revision posterior.

Si no pasa:

- se guarda un evento de fallo de destilacion;
- no se inyecta nada en futuras sesiones.

### 5. Futuras tareas

Si una tarea posterior coincide:

- `braze` recupera el playbook;
- lo inyecta como seccion pequena de metodologia;
- registra `AgentEvent::PlaybookMatched` y `AgentEvent::PlaybookInjected`;
- si la tarea se resuelve con verificacion objetiva, incrementa `validated_runs`.

## Componentes nuevos

### `braze-memory`

Agregar modulo procedimental:

- `LearnedPlaybook`
- `PlaybookLifecycle`
- `PlaybookSource`
- `PlaybookEvidence`
- `PlaybookStore`
- `FilePlaybookStore`
- `render_playbook_section`

Persistencia recomendada:

```text
.braze/
  memory.json
  playbooks/
    candidates/
      <id>.json
    approved/
      <id>.json
    retired/
      <id>.json
```

Razon: evita que `memory.json` mezcle memoria descriptiva de proyecto con memoria procedimental; permite revisar candidatos por diff; mantiene cada playbook pequeno.

### `braze-engine`

Agregar una capa de coordinacion:

- `LearningController`: orquesta retrieval, trigger, tutor call y persistencia.
- `LearningTrigger`: clasifica fallos desde eventos tipados.
- `LearningContextBuilder`: empaqueta evidencia y aplica redaccion.
- `PlaybookRetriever`: recupera playbooks candidatos por tarea.
- `PlaybookUsageTracker`: actualiza evidencia tras exito/fallo.

Eventos nuevos en `braze-events`:

- `PlaybookMatched { ids }`
- `PlaybookInjected { ids, tokens_estimate }`
- `LearningTriggered { reason }`
- `TutorDistillationStarted { backend, model }`
- `TutorDistillationCompleted { playbook_id }`
- `TutorDistillationFailed { reason }`
- `PlaybookCandidateSaved { id, path }`
- `PlaybookLifecycleChanged { id, from, to }`

### `braze-model`

No hace falta un backend nuevo si `OpenRouterBackend` ya cubre al tutor. Si conviene, agregar un wrapper pequeno:

- `TutorBackend`: trait interno o helper que llama a un `ModelBackend` y espera JSON completo.
- `TutorPromptBuilder`: prompt especializado para destilacion.

No mezclar esto con `EscalatingBackend`: la escalacion resuelve una ronda; el tutor de aprendizaje produce artefactos persistentes.

### `braze-config`

Campos propuestos:

```toml
[learning]
enabled = false
mode = "suggest" # off | suggest | approved_only | auto_candidate
tutor_backend = "openrouter"
tutor_model = "anthropic/claude-sonnet-5"
max_tutor_calls_per_turn = 1
max_cost_usd_per_turn = 0.25
inject_max_playbooks = 2
inject_budget_tokens = 500
allow_cloud_context = "ask" # never | ask | always
store_candidates = true
auto_inject_lifecycle = ["approved", "validated", "trusted"]
```

Variable de entorno sugerida:

- `BRAZE_LEARNING_ENABLED`
- `BRAZE_LEARNING_MODE`
- `BRAZE_LEARNING_TUTOR_MODEL`
- `BRAZE_LEARNING_ALLOW_CLOUD_CONTEXT`

### `braze-cli`

Subcomandos:

```text
braze learn list
braze learn show <id>
braze learn approve <id>
braze learn retire <id>
braze learn stats
braze learn validate <id> --task <suite-task>
```

En `braze chat`, cuando el modo esta en `suggest`, una escalacion deberia terminar con un aviso breve:

```text
Se guardo un playbook candidato: rust-borrow-checker-restructure-before-clone
Usa: braze learn show rust-borrow-checker-restructure-before-clone
```

### `braze-bench`

Agregar ablations:

- `+ablate:learning`
- `+ablate:no-learning`
- `+ablate:learning-candidates`
- `+ablate:learning-approved-only`
- `+ablate:learning-no-inject`

Metricas nuevas:

- `playbooks_matched`
- `playbooks_injected`
- `playbook_tokens`
- `learning_triggers`
- `tutor_calls`
- `candidate_playbooks_created`
- `validated_playbook_hits`
- `false_positive_playbook_hits`
- `cost_saved_estimate_usd`

Suite necesaria: multi-sesion. Una tarea A debe generar el candidato; una tarea B, de la misma familia pero no identica, mide si el modelo local resuelve sin tutor gracias al playbook.

## Plan por fases

### Fase 0: congelar alcance del experimento

Objetivo: escribir el contrato de la feature antes de tocar el engine.

Entregables:

- Documento de diseno aceptado.
- JSON Schema de `LearnedPlaybook`.
- Politica de privacidad/redaccion.
- Definicion de metricas del bench.

Checks:

- Ninguna llamada cloud automatica.
- Ningun playbook inyectado automaticamente.

### Fase 1: store y renderer sin modelo

Objetivo: persistir y renderizar playbooks manuales.

Cambios:

- `braze-memory/src/playbook.rs`
- `braze-memory/src/playbook_store.rs`
- `braze-memory/src/playbook_render.rs`
- tests de roundtrip, caps, lifecycle y render token-budgeted.

Aceptacion:

- `cargo test -p braze-memory`
- un playbook manual en `.braze/playbooks/approved/` se renderiza completo dentro de presupuesto.

### Fase 2: retrieval e inyeccion controlada

Objetivo: que el modelo local pueda recibir playbooks aprobados, sin tutor.

Cambios:

- `PlaybookRetriever` con matching deterministico simple.
- wiring en `braze-cli` y `braze-bench`.
- eventos `PlaybookMatched` / `PlaybookInjected`.

Aceptacion:

- una tarea Rust con playbook aprobado inyecta el procedimiento correcto;
- `+ablate:no-learning` mantiene el baseline limpio;
- si no hay match, no se inyecta nada.

### Fase 3: deteccion de fallos y paquete de aprendizaje

Objetivo: detectar cuando una tarea merece tutor, pero sin llamar todavia a OpenRouter.

Cambios:

- `LearningTrigger` sobre `AgentEvent`.
- `LearningContextBuilder` con redaccion.
- eventos `LearningTriggered`.

Aceptacion:

- tests sinteticos para schema failure, tool failure repetido, max iterations y bench failure;
- snapshot redacted no contiene API keys, `.env`, ni contenido fuera de allowlist.

### Fase 4: destilacion con tutor OpenRouter

Objetivo: generar `candidate` playbooks desde fallos reales.

Cambios:

- `TutorPromptBuilder`.
- llamada a `OpenRouterBackend` o `ModelBackend` configurado como tutor.
- parseo JSON estricto + validacion schema.
- persistencia en `.braze/playbooks/candidates/`.

Aceptacion:

- fake OpenRouter en tests produce candidato valido;
- respuesta malformada no se guarda como playbook;
- maximo una llamada tutor por turno;
- sin permiso/politica cloud, no se llama al tutor.

### Fase 5: CLI de revision y lifecycle

Objetivo: impedir que candidatos entren solos al prompt.

Cambios:

- `braze learn list/show/approve/retire/stats`.
- movimiento atomico entre `candidates/`, `approved/`, `retired/`.
- stats por playbook.

Aceptacion:

- aprobar cambia lifecycle y ubicacion;
- retirar impide inyeccion futura;
- `stats` muestra hits, validaciones y fallos.

### Fase 6: bench multi-sesion

Objetivo: medir si el aprendizaje reduce escalaciones futuras.

Diseno de brazos:

| Brazo | Descripcion |
|---|---|
| `local` | modelo local sin tutor ni playbooks |
| `lead-fallback` | escalacion reactiva, sin persistir metodologia |
| `learning-candidate` | tutor crea candidato pero no se inyecta |
| `learning-approved` | playbook aprobado se inyecta en tarea posterior |
| `human-playbook` | playbook escrito manualmente como techo practico |

Metricas primarias:

- success rate del local en tarea B;
- reduccion de `leader_escalations`;
- tokens totales;
- costo cloud total;
- rondas hasta exito.

Aceptacion:

- la suite distingue "resolver por tutor cada vez" de "resolver localmente despues de destilar";
- los resultados no mezclan candidatos no aprobados con playbooks validados.

### Fase 7: promocion controlada

Objetivo: permitir que `validated` y `trusted` se usen automaticamente.

Reglas:

- `candidate` nunca se auto-inyecta.
- `approved` se inyecta con baja prioridad.
- `validated` requiere exito objetivo posterior.
- `trusted` requiere al menos 3 validaciones y 0 fallos recientes.
- cualquier fallo objetivo con playbook inyectado baja confianza o marca para revision.

Aceptacion:

- un playbook que empeora resultados deja de ser inyectado;
- la inyeccion siempre emite eventos trazables.

### Fase 8: export futuro

Fuera del MVP, pero valioso:

- exportar playbooks buenos como `braze-skills`;
- convertir episodios tutor+playbook+exito en dataset de fine-tuning;
- agrupar playbooks por dominio;
- deduplicar playbooks similares con ayuda del tutor, siempre bajo revision.

## Riesgos y mitigaciones

| Riesgo | Mitigacion |
|---|---|
| Playbook demasiado especifico | campos `applies_when` y tests de generalizacion en tarea B |
| Auto-envenenamiento | lifecycle `candidate` + aprobacion/validacion |
| Exfiltracion a OpenRouter | redaccion, allowlist, permiso explicito y modo `allow_cloud_context = "ask"` |
| Sobrecosto cloud | `max_tutor_calls_per_turn`, costo por turno, eventos de costo |
| Inflar contexto del modelo local | `inject_budget_tokens`, top-N, truncado por lineas |
| Falsos positivos de retrieval | registrar `false_positive_playbook_hits` y bajar confianza |
| Confundir escalacion con aprendizaje | `EscalatingBackend` resuelve; `LearningController` destila |
| Bench no mide memoria entre sesiones | suite multi-sesion obligatoria antes de promover |

## MVP recomendado

El primer incremento util y seguro es:

1. `LearnedPlaybook` + store + renderer.
2. Inyeccion de playbooks manualmente aprobados.
3. Deteccion de fallos sin tutor.
4. Tutor OpenRouter que solo escribe `candidate`.
5. CLI `braze learn approve`.
6. Bench multi-sesion pequeno.

No incluir en el MVP:

- auto-aprobacion de candidatos;
- fine-tuning;
- deduplicacion semantica avanzada;
- retrieval vectorial;
- generacion de playbooks por el modelo local;
- llamadas cloud sin permiso o politica explicita.

## Pregunta de investigacion para el paper

La formulacion medible seria:

> Puede un harness local-first reducir escalaciones futuras destilando intervenciones cloud en memoria procedimental validada?

Hipotesis:

1. `learning-approved` supera a `local` en tareas B de la misma familia.
2. `learning-approved` reduce `leader_escalations` frente a `lead-fallback`.
3. El costo cloud por exito baja despues de la primera ocurrencia de una familia de fallos.
4. El beneficio aparece primero en tareas con verificacion objetiva y procedimientos repetibles.

Esa pregunta es mas fuerte que "usar OpenRouter cuando falla", porque mide transferencia procedimental, no solo fallback.
