# Framework de disciplina cientifica para `braze`

Fecha: 2026-07-16  
Estado: marco operativo vivo  
Proposito: mantener `braze` como laboratorio cientifico disciplinado, no como acumulacion de features

## Principio rector

`braze` solo produce conocimiento cientifico si cada idea se convierte en una hipotesis medible.

La secuencia obligatoria es:

```text
idea -> pregunta -> hipotesis -> implementacion minima -> ablation -> benchmark -> analisis -> decision -> paper/log
```

Si una idea no puede pasar por esa secuencia, puede quedar como nota especulativa, pero no debe entrar al core del proyecto ni al argumento de un paper.

## Taxonomia de ideas

Cada idea nueva debe clasificarse antes de implementarse.

| Tipo | Pregunta | Ejemplos | Tratamiento |
|---|---|---|---|
| Harness | Mejora el andamiaje que usa el modelo? | tool search, task list, compaction, checks | A/B obligatorio |
| Modelo | Cambia backend, sampling o routing? | Ollama, OpenRouter, lead, planner | Sweep controlado |
| Memoria | Transfiere informacion entre turnos/sesiones? | ProjectMemory, playbooks | Bench multi-sesion |
| Control | Decide politicas del agente? | escalar, verificar, pedir usuario | Logs + policy eval |
| Seguridad | Reduce riesgo o exfiltracion? | permisos, redaccion, sandbox | Tests adversariales |
| UX | Cambia como el usuario supervisa? | TUI, approvals, learn CLI | Validacion manual + logs |
| Paper | No es feature; es argumento cientifico | framing, related work, threats | Documento y evidencia |

## Gate 0: definicion de la pregunta

Antes de escribir codigo, completar:

```text
Idea:
Pregunta cientifica:
Hipotesis principal:
Hipotesis nula:
Unidad experimental:
Metricas primarias:
Metricas secundarias:
Riesgo de confounding:
Costo esperado:
Decision posible si falla:
```

Ejemplo:

```text
Idea: LearnedPlaybook desde escalaciones OpenRouter.
Pregunta cientifica: Puede una escalacion cloud amortizarse en tareas futuras?
Hipotesis principal: Un playbook aprobado aumenta success_rate local en tareas B.
Hipotesis nula: El playbook no cambia success_rate o solo agrega tokens.
Unidad experimental: familia de tareas multi-sesion A->B.
Metricas primarias: success_rate, leader_escalations, cost_usd.
Metricas secundarias: tokens, latency, tool_errors.
Riesgo de confounding: tarea B demasiado parecida a tarea A.
Costo esperado: llamadas tutor + runs bench.
Decision posible si falla: mantener como CLI manual, no auto-inyectar.
```

## Gate 1: criterio de implementacion minima

La primera version debe ser la menor que pueda falsar la hipotesis.

Reglas:

1. No implementar UX completa antes de tener una medicion minima.
2. No generalizar el schema hasta que exista un segundo caso de uso.
3. No agregar defaults globales sin ablation.
4. No mezclar dos palancas en la misma prueba si se pueden aislar.
5. No promover una feature por plausibilidad; solo por evidencia.

Checklist:

```text
[ ] Tiene flag/config off por default.
[ ] Tiene ablation o brazo equivalente.
[ ] Emite eventos observables.
[ ] Tiene metricas en bench o plan explicito para agregarlas.
[ ] Tiene failure mode documentado.
[ ] Tiene rollback simple.
```

## Gate 2: diseno experimental

Toda palanca necesita al menos:

```text
baseline
variant
negative/control ablation
```

Para features de aprendizaje o memoria:

```text
session A: crear/adquirir informacion
session B: reutilizar informacion en tarea relacionada no identica
holdout: tarea de otra familia donde no deberia ayudar
```

Para metaheuristicas:

```text
train suite: busca configuracion
validation suite: selecciona configuracion
test suite: reporta resultado
```

Para RL/bandits:

```text
logged policy
candidate policy
off-policy estimate
online smoke pequeno
```

## Gate 3: metricas canonicas

Metricas primarias de `braze`:

```text
success_rate
task_success
leader_escalations
estimated_cost_usd
latency_ms
input_tokens
output_tokens
total_tokens
tool_execution_failures
schema_validation_failures
turns_to_success
```

Metricas de memoria/aprendizaje:

```text
playbooks_matched
playbooks_injected
playbook_tokens
tutor_calls
candidate_playbooks_created
validated_playbook_hits
false_positive_playbook_hits
post_learning_success_delta
cloud_calls_avoided_estimate
```

Metricas de seguridad:

```text
permission_prompts
denied_actions
outside_workdir_attempts
redaction_hits
cloud_context_bytes_sent
sensitive_pattern_blocks
```

Metricas de calidad del harness:

```text
compactions
collapsed_observations
harness_notes_emitted
textual_rescue_applied
repeated_tool_calls_blocked
task_list_updates
```

## Gate 4: matriz de decision

Despues de medir, decidir explicitamente.

| Resultado | Decision |
|---|---|
| Mejora primaria clara, costo aceptable, sin riesgos nuevos | Promover a feature opt-in estable |
| Mejora solo en subset identificable | Mantener bajo router/condicion |
| Mejora pequena pero costo alto | Mantener como experimental |
| Sin mejora | Retirar o archivar |
| Empeora pero revela mecanismo | Documentar como hallazgo negativo |
| Inconcluso | Redisenar bench, no promover |

Formato obligatorio de decision:

```text
Decision:
Evidencia:
Metricas:
Scope donde aplica:
Scope donde no aplica:
Riesgos:
Estado nuevo: promoted | experimental | archived | retired
```

## Registro de hipotesis

Mantener un archivo por linea o experimento en `docs/`.

Convencion de nombres:

```text
docs/hypothesis-YYYY-MM-DD-short-name.md
docs/sweep-short-name-YYYY-MM-DD.md
docs/sweep-short-name-YYYY-MM-DD.json
docs/decision-short-name-YYYY-MM-DD.md
```

Template:

```markdown
# Hipotesis: <nombre>

Fecha:
Estado: proposed | running | analyzed | decided | retired
Linea: paper1 | paper2-learning | round-economics | metaheuristics | rl-policy | security | ux

## Pregunta

## Hipotesis

## Hipotesis nula

## Diseno experimental

## Metricas

## Resultados

## Decision

## Lecciones
```

## Lineas de investigacion activas

Estados sincronizados con la evidencia en `docs/` el **2026-07-29** (la tabla
anterior decia "propuesta" para memoria procedimental cuando ya tenia 25
sweeps y una sintesis cerrada — ver nota abajo).

| Linea | Pregunta madre | Estado | Paper |
|---|---|---|---|
| Harness local-first | Cuando el harness enruta capacidad local suficiente? | **en submission** | Paper 1 |
| Memoria procedimental | Puede una escalacion cloud amortizarse en sesiones futuras? | **medida, condicion identificada** | Paper 2 |
| Economia de la ronda | El eje que manda es la escala del modelo o el precio de reintentar? | pre-registrada, piloto de contexto corrido | intermedio |
| Metaheuristicas | Puede calibrarse el harness con optimizacion multiobjetivo? | propuesta, **bloqueada** | intermedio/Paper 2.5 |
| RL/bandits | Puede aprenderse una politica de orquestacion? | futura, sin trabajo | Paper 3 |
| Privacidad local-cloud | Que informacion debe salir del entorno local? | futura, sin trabajo | transversal |

**Memoria procedimental — que se sabe ya** (no es "propuesta"):
`docs/sweep-memory-distillation-3taskB-synthesis-2026-07-17.md`, estado
CERRADO, 140 corridas sobre 3 tareas B independientes de la misma familia.
El playbook ahorra rondas **solo en la tarea que el modelo ya tiene
memorizada** (la unica con `net_token_delta` negativo, -304); en las dos
tareas frescas **cuesta** tokens netos (+1076, +1132), porque el
`round_reduction` que produce (+0.15, +0.35) no paga el costo fijo de
~200-270 tokens/ronda que agrega en cada turno. La linea sigue viva pero la
pregunta cambio: ya no es "sirve?" sino **"cual es la condicion bajo la cual
amortiza?"**, y esa condicion esta identificada y es restrictiva.

> **Nota de nomenclatura (2026-07-29)**: el pre-registro
> `docs/hypothesis-2026-07-28-round-economics.md` nacio etiquetado
> `Linea: paper3-round-economics`, lo que chocaba con el Paper 3 que esta
> tabla ya tenia asignado a RL/bandits. Se renombra la linea a
> `round-economics`, sin numero: el numero de paper es un slot que se
> reordena, el nombre de la linea no deberia.

## Ordenamiento entre lineas (agregado 2026-07-29)

Las lineas de la tabla no son independientes: dos de ellas atacan el mismo
problema con distinto instrumento, y correrlas en el orden equivocado es caro.

### Economia de la ronda ANTES que metaheuristicas

Las dos preguntan por la configuracion optima del harness. La diferencia es
que una pregunta **si el eje existe** y la otra **donde esta el optimo sobre
ese eje**:

| | Economia de la ronda | Metaheuristicas |
|---|---|---|
| Instrumento | factorial de 3 configuraciones fijadas a mano (avara / derrochadora / solo-contexto) x 2 precios de ronda | NSGA-II y afines sobre ~15 knobs continuos y discretos |
| Responde | existe interaccion entre precio de ronda y configuracion? | cual es la frontera de Pareto exito/costo/latencia/tokens? |
| Costo | decenas de corridas | cientos o miles |

**La dependencia**: si el factorial da negativo —no hay interaccion, la
configuracion optima no depende del precio de la ronda— entonces buscar la
frontera de Pareto sigue siendo legitimo, pero pierde su hipotesis
organizadora y se vuelve un ejercicio de tuning, no un resultado. Y si da
positivo, el factorial ademas **entrega el eje sobre el cual vale la pena
buscar**, lo que reduce el espacio de 15 knobs a los que efectivamente
interactuan.

Correr NSGA-II sobre 15 knobs con corridas de ~20 minutos cada una, antes de
saber si el eje manda, es el gasto mas grande que este proyecto podria hacer
sin una pregunta que lo justifique. Regla anti-deriva 1 aplicada a una linea
entera: *una busqueda sin hipotesis es tuning, no investigacion*.

**Gate explicito**: no abrir la linea de metaheuristicas hasta que el piloto
de costo de `docs/hypothesis-2026-07-28-round-economics.md` haya decidido. Si
decide "no medible con el poder disponible" (su salida 3), metaheuristicas
queda tambien bloqueada por la misma razon —seria optimizar sobre una
diferencia que no sabemos distinguir del ruido— y lo que corresponde no es
correrla igual sino resolver primero el problema de poder.

### El resto de la cadena no cambia

Las dependencias que ya declaraba `docs/future-research-lines-2026-07-16.md`
siguen vigentes, con la nueva intercalada:

1. Bench multi-sesion + playbooks manuales/aprobados (memoria procedimental).
2. **Economia de la ronda** — factorial, decide si el eje existe.
3. `braze-bench tune` / metaheuristicas — solo si 2 dio positivo.
4. Bandits/RL — solo con logs suficientes, y despues de 3.

## Reglas anti-deriva

1. Una feature sin metrica es deuda, no investigacion.
2. Un resultado negativo se conserva si falsifica una hipotesis razonable.
3. Un sweep sin analisis no cuenta como evidencia.
4. Una mejora no generaliza hasta probarse en holdout.
5. Un benchmark contaminado se repite o se descarta.
6. Un modelo superior resolviendo una tarea no prueba que el harness ayude.
7. Una memoria que aumenta tokens sin mejorar exito es ruido.
8. Un paper nuevo requiere una pregunta nueva, no solo mas features.

## Cadencia recomendada

### Semanal

- Revisar ideas nuevas y clasificarlas.
- Archivar ideas sin pregunta medible.
- Elegir maximo una hipotesis para implementacion minima.

### Por experimento

1. Escribir `hypothesis-*.md`.
2. Implementar feature off-by-default.
3. Agregar eventos/metricas.
4. Correr sweep pequeno.
5. Analizar.
6. Decidir.

### Por paper

- Congelar contribuciones.
- Separar future work.
- Asegurar que cada claim tenga evidencia directa.
- Mover ideas nuevas al backlog de investigacion, no al manuscrito.

## Criterios para abrir un nuevo paper

Abrir un paper separado si se cumplen al menos tres:

```text
[ ] La pregunta cientifica cambia.
[ ] Requiere benchmark nuevo.
[ ] Requiere metricas nuevas.
[ ] La contribucion no cabe como extension directa del paper actual.
[ ] Tiene riesgos/metodologia propios.
[ ] Puede producir resultados negativos interpretables.
```

Aplicacion actual:

- `LearnedPlaybook`: si, Paper 2.
- Economia de la ronda: paper intermedio si el termino de interaccion sobrevive
  al piso de ruido. Gatea a metaheuristicas (ver "Ordenamiento entre lineas").
- Metaheuristicas: posible paper intermedio si produce frontera Pareto fuerte,
  **bloqueada** hasta que el piloto de costo de economia de la ronda decida.
- RL/bandits: si, Paper 3 cuando existan logs suficientes.

## Backlog disciplinado

| Idea | Gate actual | Proximo paso |
|---|---|---|
| `LearnedPlaybook` | Gate 0/1 | Implementar store + renderer manual |
| Bench multi-sesion | Gate 0 | Disenar suite A->B + holdout |
| `braze-bench tune` | Gate 0 | Definir knobs y objetivos |
| RL/bandits | Pre-Gate | Guardar features en logs primero |
| Playbook selection | Pre-Gate | Esperar a tener >=10 playbooks |
| Prompt evolution | Pre-Gate | Solo despues de split train/test |

## Pregunta final antes de implementar cualquier cosa

```text
Si esta idea falla, que aprendemos?
```

Si la respuesta es "nada claro", la idea no esta lista.
