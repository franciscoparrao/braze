# Lineas futuras de investigacion para `braze`

Fecha: 2026-07-16  
Estado: notas de investigacion, no compromiso de implementacion  
Contexto: ideas surgidas despues del reencuadre del paper actual hacia "el harness enruta capacidad"

Marco disciplinario asociado: `docs/research-discipline-framework-2026-07-16.md`.

## Posicion respecto del paper actual

Estas ideas no deberian entrar como contribuciones centrales del manuscrito EMSE actual. El paper vigente ya tiene una tesis cerrada: evaluar como un harness agentico permite que modelos locales resuelvan tareas mediante composicion, validacion, tool-use, escalacion y medicion controlada.

Las lineas de abajo abren preguntas cientificas nuevas. Conviene mencionarlas en `Future Work`, pero tratarlas como material para trabajos posteriores.

## Linea 1: memoria procedimental destilada desde escalaciones cloud

Documentos tecnicos asociados:

- `docs/learning-mode-development-plan.md`
- `docs/hypothesis-2026-07-16-memory-distillation.md`
- `docs/paper2-memory-distillation-protocol-2026-07-16.md`

### Idea

Cuando el modelo local falla, `braze` escala a un modelo superior via OpenRouter. En vez de usar esa intervencion solo para resolver la tarea actual, el harness le pide al modelo superior que destile una metodologia reutilizable para el modelo local. Esa metodologia se guarda como un `LearnedPlaybook` versionado, validado y recuperable en tareas futuras.

Formulacion:

> Puede un harness local-first reducir escalaciones futuras convirtiendo intervenciones cloud en memoria procedimental verificable?

### Por que es un segundo paper

Esto ya no estudia solamente "routing de capacidad" en una tarea aislada. Estudia transferencia procedimental amortizada entre sesiones:

- tarea A falla y genera un playbook candidato;
- tarea B, relacionada pero no identica, se intenta localmente con ese playbook;
- se mide si baja la necesidad de escalar, el costo cloud y las rondas hasta exito.

### Hipotesis

1. Playbooks aprobados aumentan el success rate del modelo local en familias de tareas repetibles.
2. La primera escalacion cloud puede amortizarse si evita escalaciones posteriores.
3. El beneficio sera mayor en tareas con verificacion objetiva: compilacion, tests, schema validation, asserts del bench.
4. Candidatos no validados pueden danar por sobre-especificidad o ruido de contexto.

### Riesgos cientificos

- Auto-envenenamiento de memoria.
- Falsos positivos de retrieval.
- Playbooks demasiado especificos al episodio original.
- Exfiltracion de contexto a OpenRouter.
- Benchmark dificil: requiere suites multi-sesion.

### Artefacto esperable

Un sistema de `LearnedPlaybook` con lifecycle:

```text
candidate -> approved -> validated -> trusted
          \-> retired
```

Y un benchmark que compare:

```text
local
lead-fallback
learning-candidate
learning-approved
human-playbook
```

## Linea 2: aprendizaje por refuerzo para politicas del harness

### Idea

`braze` no deberia usar RL inicialmente para generar codigo. La aplicacion natural es aprender politicas de orquestacion: decidir cuando escalar, que playbooks inyectar, cuanto contexto conservar, cuando verificar y que modelo usar.

Formulacion:

> Puede un agente local-first aprender politicas de orquestacion que optimicen exito, costo, latencia, tokens y riesgo?

### Acciones candidatas

```text
seguir local
escalar a lead
pedir planificador
invocar tutor OpenRouter
inyectar playbook
correr check
correr test
pedir aclaracion al usuario
responder ahora
```

### Estado observable

```text
familia de tarea
backend local
errores recientes
schema failures
tool failures
tokens usados
rondas usadas
playbooks disponibles
historial de escalaciones
checks disponibles
riesgo de privacidad
```

### Reward inicial

```text
reward =
  success
  - cost_usd
  - latency_penalty
  - token_penalty
  - privacy_penalty
  - unnecessary_escalation_penalty
```

El reward debe depender de verificaciones objetivas siempre que sea posible. Si se basa en un grader LLM, existe riesgo fuerte de reward hacking y sobreajuste.

### Metodo recomendado

Empezar con contextual bandits u offline RL sobre logs de `braze-bench`, no con RL profundo online.

Razones:

- `braze` ya produce eventos y metricas ricas.
- Las acciones son discretas.
- El costo de explorar online con modelos cloud puede ser alto.
- La distribucion de tareas del bench permite evaluacion off-policy controlada.

### Preguntas de investigacion

1. El routing aprendido supera reglas manuales como `lead_threshold=2`?
2. Aprende a evitar escalaciones innecesarias sin reducir el success rate?
3. Generaliza entre familias de tareas o solo memoriza la suite?
4. Que senales predicen mejor el momento de escalar?

### Por que puede ser un tercer paper

Esta linea cambia el foco desde memoria procedimental hacia control adaptativo. El objeto de estudio no es el playbook sino la politica del harness.

## Linea 3: metaheuristicas para calibracion multiobjetivo del harness

### Idea

Antes de RL, `braze` puede beneficiarse de metaheuristicas para buscar configuraciones de harness. Esto es mas simple, mas controlable y probablemente mas util en el corto plazo.

Formulacion:

> Puede la calibracion multiobjetivo del harness encontrar fronteras de Pareto entre exito, costo, latencia y tokens para modelos locales?

### Espacio de busqueda

Knobs candidatos:

```text
tactical_window
tactical_compaction_threshold
full_observations
best_of_n
lead_turns
lead_threshold
lead_window
planner_max_tokens
ollama_temperature
ollama_top_p
ollama_top_k
ollama_num_ctx
tool_search_threshold
playbook_budget_tokens
max_turn_iterations
```

### Objetivos

```text
maximizar success_rate
minimizar cost_usd
minimizar latency
minimizar total_tokens
minimizar leader_escalations
minimizar tool_errors
```

### Algoritmos candidatos

| Metodo | Uso razonable |
|---|---|
| NSGA-II | Frontera multiobjetivo exito/costo/latencia/tokens. |
| Simulated annealing | Busqueda discreta sobre knobs con presupuesto limitado. |
| Genetic algorithms | Subsets de playbooks/skills o combinaciones de knobs. |
| Bayesian optimization | Knobs continuos/casi-continuos con runs caros. |
| Beam search | Secuencias de acciones o checks en tareas largas. |
| Greedy knapsack | Seleccion de playbooks bajo presupuesto de tokens. |

### Aplicacion 1: `braze-bench tune`

Subcomando posible:

```bash
braze-bench tune \
  --suite suites/default.toml \
  --backend ollama:gemma4:e4b \
  --search nsga2 \
  --budget 200-runs \
  --objectives success,cost,latency,tokens
```

Salida esperada:

```text
config A: 83% success, $0.00, 1.2x latency
config B: 89% success, $0.04, 1.8x latency
config C: 93% success, $0.18, 2.6x latency
```

### Aplicacion 2: seleccion de playbooks

Con muchos playbooks, el problema se vuelve un knapsack:

```text
seleccionar playbooks <= budget_tokens
maximizar probabilidad de ayudar
minimizar distractores
```

Esto puede implementarse primero con heuristica greedy y luego compararse contra busqueda local o genetica.

### Aplicacion 3: diseno de prompts

Se pueden evolucionar variantes cortas de system prompt o harness notes, midiendo:

```text
pass_rate - token_cost - tool_error_rate - escalation_rate
```

Riesgo: sobreajuste a la suite. Mitigacion: train/eval split por familias de tarea y holdout temporal.

### Ventaja frente a RL

Las metaheuristicas optimizan configuraciones offline. No necesitan aprender una politica estable ni explorar durante sesiones reales. Para `braze`, son un paso previo mas seguro y posiblemente publicable antes de RL.

## Relacion entre las tres lineas

| Linea | Pregunta | Horizonte |
|---|---|---|
| Memoria procedimental | Como reutilizar una escalacion cloud? | Paper 2 |
| Metaheuristicas | Como calibrar el harness bajo multiples objetivos? | Trabajo intermedio o Paper 2.5 |
| RL/bandits | Como aprender politicas de orquestacion? | Paper 3 |

Dependencias recomendadas:

1. Primero construir bench multi-sesion y playbooks manuales/aprobados.
2. Luego agregar `braze-bench tune` para calibrar knobs y presupuestos.
3. Solo despues evaluar bandits/RL con logs suficientes.

## Como mencionarlo en el paper actual

Texto breve posible para `Future Work`:

> Beyond one-shot capability routing, future work should examine whether cloud escalations can be amortized across sessions. A local-first harness could distill successful lead-model interventions into validated procedural playbooks, retrieve them before future escalations, and measure whether this reduces cloud calls without sacrificing task success. A complementary direction is to optimize harness policies themselves, either offline through multi-objective metaheuristics over configuration knobs or, with sufficient logs, through contextual bandits that learn when to escalate, verify, or inject procedural memory.

## Proximos pasos minimos

1. Mantener esta linea fuera del scope del manuscrito EMSE actual salvo `Future Work`.
2. Cerrar primero el plan tecnico de `LearnedPlaybook`.
3. Disenar una suite multi-sesion pequena.
4. Agregar un prototipo offline de `braze-bench tune` antes de RL.
5. Empezar a guardar features de estado suficientes en resultados de bench para permitir bandits offline mas adelante.
