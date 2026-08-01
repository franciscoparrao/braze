# Protocolo Paper 2: memoria procedimental destilada para agentes local-first

Fecha: 2026-07-16 (actualizado 2026-07-17 con el hallazgo del piloto M1 sobre la
familia `rust_compile_repair`, confirmado con una tercera tarea B independiente el
mismo día)  
Estado: piloto M1 corrido y analizado para `human-playbook` vs `none` (3 tareas B,
1 holdout, n=20 cada una); `summary`/`procedural`/`lead-fallback` siguen sin
pilotear. La condición de amortización (el playbook solo ahorra tokens netos si
reduce rondas) se confirmó como la norma, no un caso atípico: 2 de 3 tareas B
cuestan tokens netos. Ver `docs/decision-memory-distillation-pilot-2026-07-16.md`,
`docs/sweep-memory-distillation-loopB-2026-07-17.md` y
`docs/sweep-memory-distillation-3taskB-synthesis-2026-07-17.md` para los resultados
completos.  
Documento relacionado: `docs/hypothesis-2026-07-16-memory-distillation.md`
Schema inicial: `docs/learned-playbook-v1.schema.json`

## Titulo tentativo

Opciones:

1. **Amortizing Cloud Escalations as Procedural Memory for Local-First Coding Agents**
2. **From Escalation to Playbook: Distilling Cloud Interventions for Local Agent Reuse**
3. **Procedural Memory Distillation for Local-First Agentic Coding Systems**

Titulo recomendado por ahora:

> From Escalation to Playbook: Distilling Cloud Interventions for Local Agent Reuse

## Claim central

Los sistemas local-first no tienen que elegir entre resolver siempre localmente o escalar repetidamente a modelos cloud. Una tercera opcion es amortizar la escalacion: usar el modelo cloud una vez como tutor, destilar su intervencion en memoria procedimental verificable y reutilizar esa metodologia en tareas futuras relacionadas.

## Contribuciones esperadas

1. Definir `procedural memory distillation` para agentes de codigo local-first.
2. Proponer `LearnedPlaybook`, una unidad tipada de memoria procedimental con lifecycle y evidencia.
3. Introducir un benchmark multi-sesion `A -> B -> H` para medir transferencia procedimental.
4. Comparar memoria procedimental contra no memoria, fallback cloud, memoria episodica, resumen libre y playbooks humanos.
5. Medir exito, costo, escalaciones, tokens, falsos positivos y amortizacion de cloud.

## Modelo conceptual

```text
Session A:
  local model attempts task
  local model fails under objective check
  cloud tutor solves/explains
  harness distills a candidate playbook

Session B:
  related task appears
  harness retrieves playbook
  local model attempts task with procedure
  objective check determines success

Holdout H:
  unrelated task appears
  harness should not inject the playbook
  false positives are measured
```

## Diferencia entre memoria episodica y procedimental

| Tipo | Que guarda | Riesgo | Hipotesis |
|---|---|---|---|
| Episodica | Lo que paso en la sesion A | Mucho ruido, datos especificos | Ayuda si B se parece mucho a A |
| Resumen libre | Sintesis narrativa de A | Ambiguedad, omisiones | Puede ayudar, pero no fuerza verificaciones |
| Procedimental | Metodo reusable + checks + limites | Sobre-generalizacion | Deberia transferir mejor entre tareas no identicas |

La comparacion bajo igual presupuesto de tokens es central. Sin ella, no se sabra si el beneficio viene de "recordar mas cosas" o de "recordar la forma correcta".

## Hallazgo del piloto M1: la condicion de amortizacion

Dos tareas B de la misma familia (`rust_compile_repair`, mismo playbook humano
generico) dieron resultados de eficiencia opuestos, y la causa quedo diagnosticada con
sesiones preservadas (`docs/sweep-memory-distillation-loopB-2026-07-17.md`):

- Tarea B original (bug canonico, saturada en pass rate): el playbook acorta la
  trayectoria en 1 ronda completa (7.00 -> 6.00), y eso compensa de sobra el costo fijo
  de ~250-300 tokens/ronda que agrega estar presente en el contexto de cada turno --
  el total de tokens de entrada *baja* (9953 -> 9682).
- Tarea B "loop" (bug menos canonico, con headroom real en pass rate): el playbook NO
  acorta la trayectoria (5.35 -> 5.55 rounds, sin diferencia estadistica), asi que el
  mismo costo fijo por ronda queda sin nada que lo compense -- el total de tokens de
  entrada *sube* 27% (7097 -> 9041) y el wall time sube con el, significativamente
  (p=0.008), en la direccion contraria a lo esperado.

**La memoria procedimental no es gratis solo por ser aplicable.** Su costo es
proporcional a `tokens_por_playbook x rondas_del_turno` (se reenvia completo en cada
ronda junto con el resto del contexto, no una sola vez). Para que el balance de tokens
sea neto positivo, el playbook tiene que reducir rondas lo suficiente para pagar ese
costo acumulado -- no basta con que su `applies_when` matchee la tarea, ni con que el
modelo efectivamente use su consejo sin desviarse (se verifico que asi fue en la tarea
loop: el modelo aplico el fix correcto, no se confundio por el playbook, simplemente no
lo necesito para llegar ahi mas rapido).

Esto tiene una implicacion de diseno directa para el resto del protocolo: **el criterio
de exito de una condicion de memoria no deberia ser solo `transfer_gain > 0`
(success_rate) sino tambien reportar si `turns_to_success` bajo lo suficiente para
justificar el costo de contexto** -- una condicion medible por tarea, no un supuesto
general sobre "la memoria ayuda". Ver la seccion `Metricas` (`Derived`) mas abajo para
las cantidades derivadas que hacen esto reportable.

### Actualizacion 2026-07-17: confirmado con una tercera tarea B independiente

Se piloteo una tercera tarea B (`move`, bug E0382 "use of moved value" -- distinto de
los dos E0502 anteriores, tambien cubierto por el `applies_when` del playbook) para
responder si la ausencia de `round_reduction` en `loop` era la norma o un caso
atipico. Resultado a n=20 (`docs/sweep-memory-distillation-3taskB-synthesis-
2026-07-17.md`, 140 corridas totales entre las 3 tareas): **es la norma**.
`round_reduction` es grande solo en la tarea original saturada (+0.95 rondas); en
`loop` y `move` es marginal (+0.15, +0.35) y no alcanza a pagar el costo fijo de
~200-270 tokens/ronda -- `net_token_delta` es positivo (cuesta tokens netos) en 2 de
3 tareas. Tampoco hay senal de `success_rate` en ninguna de las 3 (Fisher p=0.72,
1.00, 0.41; direccion del punto estimado negativa en 2 de 3). El unico resultado
positivo de todo el piloto M1 sigue siendo la eficiencia en la tarea original, y
ahora hay evidencia de que NO generaliza dentro de la misma familia de bugs, no solo
de una segunda tarea aislada.

## Artefacto tecnico: `LearnedPlaybook`

Campos minimos para el paper:

```text
id
title
lifecycle
task_family
applies_when
failure_signals
preconditions
method_steps
verification
avoid
escalate_if
source
evidence
```

Lifecycle:

```text
candidate -> approved -> validated -> trusted
          \-> retired
```

Para el primer experimento, `candidate` puede aprobarse por protocolo si:

1. pasa JSON Schema;
2. no contiene datos sensibles;
3. no menciona archivos concretos de `A` salvo como patrones generalizados;
4. declara checks objetivos;
5. declara `escalate_if`.

No debe editarse semanticamente a mano en el experimento principal, porque eso mezclaria tutor-destillation con human-authored playbooks. La condicion `human-playbook` cubre ese techo.

## Suite minima inicial

### Familia 1: Rust compile repair

Objetivo: corregir un fallo de compilacion relacionado con ownership, lifetimes o traits.

Sesion `A`:

- introducir una tarea que produzca error repetible;
- local model falla o entra en loop;
- tutor produce playbook de metodologia.

Sesion `B`:

- error de la misma clase, distinto modulo y nombres;
- verificar con `cargo check`.

Holdout `H`:

- tarea Rust que no involucra esa clase de error;
- el playbook no deberia inyectarse.

### Familia 2: Tool schema repair

Objetivo: resolver llamadas a tools con argumentos incorrectos.

Checks:

- `schema_validation_failures`;
- tool result exitoso;
- task success.

Transferencia esperada:

- el playbook debe instruir inspeccion de schema, reparacion una vez y escalacion si se repite.

### Familia 3: Multi-file edit with verification

Objetivo: cambiar comportamiento que requiere leer varios archivos y verificar con test.

Checks:

- test especifico pasa;
- no modifica archivos irrelevantes;
- no aumenta tool errors.

Transferencia esperada:

- el playbook debe guiar estrategia: mapear call graph minimo, editar poco, verificar pronto.

## Condiciones experimentales

| Condicion | Inyeccion en B | Cloud permitido en B |
|---|---|---|
| `none` | Ninguna | No |
| `lead-fallback` | Ninguna | Si |
| `episodic` | Episodio A truncado | No |
| `summary` | Resumen libre de A | No |
| `procedural` | LearnedPlaybook | No |
| `procedural+fallback` | LearnedPlaybook | Si |
| `human-playbook` | Playbook manual | No |

Separar `procedural` y `procedural+fallback` es importante: uno mide transferencia local pura; el otro mide si el playbook reduce la frecuencia de fallback en un sistema realista.

## Presupuesto de tokens

Usar un presupuesto fijo por condicion de memoria:

```text
memory_budget_tokens = 500
```

Reglas:

- `episodic` se trunca por eventos completos.
- `summary` se trunca por lineas completas.
- `procedural` se trunca por secciones completas en este orden:
  1. applies_when
  2. method_steps
  3. verification
  4. avoid
  5. escalate_if

## Metricas

Primarias:

```text
success_rate
leader_escalations
estimated_cost_usd
turns_to_success
```

Secundarias:

```text
input_tokens
output_tokens
latency_ms
tool_execution_failures
schema_validation_failures
compactions
harness_notes_emitted
playbook_tokens
false_positive_playbook_hits
```

Derived:

```text
amortized_cloud_cost = (cost_A_tutor + cost_B) / successes_B
cloud_calls_avoided = escalations_B(lead-fallback) - escalations_B(procedural+fallback)
transfer_gain = success_B(procedural) - success_B(none)
procedure_advantage = success_B(procedural) - max(success_B(episodic), success_B(summary))

# Agregadas 2026-07-17 tras el hallazgo del piloto M1 (ver seccion arriba):
# hacen reportable la condicion de amortizacion por tarea, no solo el signo
# de transfer_gain/eficiencia agregada.
round_reduction = turns_to_success(none) - turns_to_success(condicion_de_memoria)
token_tax_per_round = mean(input_tokens_per_round(condicion_de_memoria)) - mean(input_tokens_per_round(none))
net_token_delta = input_tokens_total(condicion_de_memoria) - input_tokens_total(none)
# net_token_delta < 0 solo si round_reduction * tokens_por_ronda_del_turno >
# token_tax_per_round * rounds(condicion_de_memoria) -- reportar los tres
# numeros juntos, no solo el resultado neto, para que un lector distinga "el
# playbook ahorro rondas" de "el playbook no costo nada" de "el playbook costo
# pero el turno era corto igual".
```

## Implementacion recomendada para el piloto

No empezar con integracion live completa. Primero hacer pipeline offline-controlado.

### Paso 1: fixtures y transcripts

- correr tarea `A` con local;
- guardar transcript JSONL;
- etiquetar fallo objetivo.

### Paso 2: destilacion offline

Crear comando experimental o script:

```text
braze-bench distill-playbook \
  --transcript docs/transcripts/family-a.jsonl \
  --tutor openrouter:anthropic/claude-sonnet-5 \
  --out docs/playbooks/family-a.playbook.json
```

Si el comando aun no existe, hacerlo como script interno primero.

### Paso 3: inyeccion en bench

Agregar condicion en task runner:

```text
memory_condition = "procedural"
memory_file = "docs/playbooks/family-a.playbook.json"
memory_budget_tokens = 500
```

### Paso 4: comparacion

Ejecutar suite:

```bash
braze-bench run \
  --suite suites/memory-distillation.toml \
  --backends "ollama:gemma4:e4b" \
  --reps 5 \
  --out docs/sweep-memory-distillation-pilot-2026-07-XX.json
```

## Cambios minimos de codigo

Orden sugerido:

1. `braze-memory`: `LearnedPlaybook` + renderer.
2. `braze-bench`: permitir `memory_file` y `memory_condition` por task.
3. `braze-bench`: metricas `playbook_tokens`, `memory_condition`, `false_positive_playbook_hits`.
4. Script offline de destilacion desde transcript.
5. Solo despues: integracion live en `braze-engine`.

Razon: el paper necesita inferencia causal limpia antes que UX.

## Threats to validity anticipadas

| Threat | Mitigacion |
|---|---|
| B demasiado parecido a A | Usar nombres, archivos y constantes distintas; reportar distancia cualitativa. |
| Playbook editado por humano | Separar condicion `human-playbook`; en `procedural`, solo safety review. |
| Tutor resuelve con datos especificos | Validar que el playbook no cite archivos concretos salvo como patrones. |
| Token budget desigual | Igualar `memory_budget_tokens`. |
| Bench pequeno | Reportar piloto como exploratorio; aumentar n antes de claims fuertes. |
| Reward/grade subjetivo | Preferir checks objetivos. |
| Overfit a `braze` repo | Incluir al menos una suite sintetica externa. |

## Primer milestone

Milestone `M0`: paper protocol frozen.

Entregables:

- este protocolo;
- hipotesis formal;
- schema inicial de playbook;
- diseno de suite `A -> B -> H`;
- lista de cambios minimos de codigo.

Milestone `M1`: pilot sin tutor live.

Entregables:

- playbooks manuales/humanos para 3 familias;
- bench compara `none`, `summary`, `procedural`, `human-playbook`;
- no hay llamadas OpenRouter en el loop.

**Estado 2026-07-17**: parcialmente corrido. Solo la familia `rust_compile_repair`
tiene playbook humano y bench corrido, y solo las condiciones `none`/`human-playbook`
(mas holdout) -- `summary` y `procedural` siguen sin implementar, y las familias
`tool_schema_repair`/`multi_file_edit` siguen sin playbook ni suite. Dentro de lo
corrido: 2 tareas B piloteadas (la original, saturada; una segunda, "loop", disenada
para tener headroom en pass rate) dieron el hallazgo de la "condicion de amortizacion"
documentado arriba -- ver `docs/decision-memory-distillation-pilot-2026-07-16.md` y
`docs/sweep-memory-distillation-loopB-2026-07-17.md` para los numeros completos
(n=5/10/20 por tarea, sesiones preservadas para el mecanismo).

Milestone `M2`: tutor offline.

Entregables:

- transcript `A`;
- destilacion por OpenRouter;
- playbook candidate reproducible;
- evaluacion en `B`.

Milestone `M3`: integracion live controlada.

Entregables:

- `LearningController` experimental;
- `candidate` only, no auto-inject;
- CLI de approve/retire.

## Decision inmediata

~~El siguiente paso tecnico no es implementar el tutor. Es implementar soporte para
inyectar un playbook manual en `braze-bench` y medir si una metodologia procedimental,
escrita por humano, mejora tareas `B`.~~ Hecho (M1 parcial, ver arriba). El techo
humano SI ayuda, pero condicionalmente (a la reduccion de rondas), no
incondicionalmente -- la pregunta "vale la pena automatizar la destilacion" cambia de
forma en vez de resolverse: ya no es solo "si ni el techo humano ayuda, no vale la
pena", sino **"un playbook auto-destilado necesita, ademas de ser correcto, ser lo
bastante bueno para acortar la trayectoria -- un playbook verboso o generico que el
modelo ya sabe seguir sin ayuda puede ser correcto y sin embargo costar tokens netos"**.
Esto es un criterio de calidad adicional para cualquier `LearnedPlaybook` candidato
(automatico o humano) que el `lifecycle` del schema (`candidate -> approved ->
validated -> trusted`) deberia poder capturar -- por ejemplo, exigiendo
`round_reduction > 0` medido, no solo checks de formato/seguridad, antes de promover de
`candidate` a `validated`.

~~1. Pilotear una tercera tarea B (misma familia o `tool_schema_repair`) para saber si
   la condicion de amortizacion es la norma o esta tarea "loop" fue el caso atipico.~~
Hecho 2026-07-17 (tarea `move`, E0382) -- **es la norma**, no el caso atipico: 2 de 3
tareas B cuestan tokens netos, ver "Actualizacion 2026-07-17" arriba y
`docs/sweep-memory-distillation-3taskB-synthesis-2026-07-17.md`.

Siguientes pasos concretos, en orden de costo creciente:

1. Implementar `summary` (resumen libre) como condicion de comparacion bajo el mismo
   presupuesto de tokens -- sin ella no se puede medir `procedure_advantage`. Ahora con
   una pregunta adicional que antes no tenia sentido plantear: si `summary` tambien
   falla en ahorrar rondas fuera de la tarea memorizada, la condicion de amortizacion
   podria ser una propiedad del *mecanismo* (cualquier memoria de texto reenviada por
   ronda cuesta lo mismo) y no algo especifico de `human-playbook`.
2. Antes de invertir en tutor offline (`M2`): decidir si el criterio de exito de
   `procedural` en el protocolo debe exigir `round_reduction` medido como gate de
   promocion (`candidate -> validated`), dado que ahora hay evidencia de que
   "correcto y aplicable" no implica "rentable en tokens" en 2 de 3 tareas piloteadas.
3. Considerar si vale la pena seguir gastando ciclos en la familia
   `rust_compile_repair` con este playbook generico, o si el proximo dato util viene
   de una familia distinta (`tool_schema_repair`) donde el playbook podria ofrecer un
   tipo de ayuda distinto (evitar reintentos de schema en vez de acortar razonamiento).
