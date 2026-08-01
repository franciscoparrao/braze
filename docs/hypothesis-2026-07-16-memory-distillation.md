# Hipotesis: destilacion de memoria procedimental desde escalaciones cloud

Fecha: 2026-07-16  
Estado: piloto M1 analizado y decidido (2026-07-16) — ver
`docs/sweep-memory-distillation-pilot-2026-07-16.md` y
`docs/decision-memory-distillation-pilot-2026-07-16.md`. Hipotesis general sigue `proposed` para
las condiciones no piloteadas aun (`procedural`, `episodic`, `summary`, `lead-fallback`).  
Linea: paper2-learning

## Pregunta

Puede un harness local-first reducir escalaciones futuras convirtiendo una intervencion cloud en memoria procedimental verificable para modelos locales?

## Hipotesis principal

Un playbook procedimental destilado desde una escalacion cloud aumenta el success rate del modelo local en tareas futuras relacionadas, reduce `leader_escalations` y baja el costo cloud acumulado frente a un fallback que escala cada vez.

## Hipotesis nula

La memoria procedimental no mejora el success rate local frente a no usar memoria, o su beneficio no supera el costo de tokens/contexto que agrega.

## Hipotesis secundarias

1. La memoria procedimental supera a memoria episodica cruda bajo el mismo presupuesto de tokens.
2. La memoria procedimental supera a un resumen libre del episodio bajo el mismo presupuesto de tokens.
3. El beneficio aparece primero en familias con checks objetivos: compilacion, tests, schema validation y errores de tool-use repetibles.
4. Los playbooks demasiado especificos causan falsos positivos de retrieval y deben retirarse o bajar confianza.

## Unidad experimental

Una familia multi-sesion `A -> B -> H`:

- `A`: tarea de origen donde el modelo local falla y se produce una escalacion cloud.
- `B`: tarea relacionada pero no identica, donde se mide transferencia.
- `H`: holdout de otra familia donde el playbook no deberia inyectarse o no deberia ayudar.

La unidad no es una tarea aislada. Es el par de transferencia entre sesiones.

## Variables independientes

Condicion de memoria en la tarea `B`:

| Condicion | Descripcion |
|---|---|
| `none` | Modelo local sin memoria ni playbook. |
| `lead-fallback` | Escalacion reactiva disponible, pero sin persistir metodologia. |
| `episodic` | Fragmento crudo o casi crudo del episodio `A`, con presupuesto igualado. |
| `summary` | Resumen libre del episodio `A`, con presupuesto igualado. |
| `procedural` | `LearnedPlaybook` destilado desde `A`, con presupuesto igualado. |
| `human-playbook` | Playbook escrito manualmente como techo practico. |

## Variables dependientes

Metricas primarias:

```text
success_rate_B
leader_escalations_B
estimated_cost_usd_total_A_plus_B
turns_to_success_B
```

Metricas secundarias:

```text
input_tokens_B
output_tokens_B
latency_ms_B
tool_execution_failures_B
schema_validation_failures_B
playbook_tokens
playbooks_injected
false_positive_playbook_hits_H
```

## Diseno experimental minimo

1. Seleccionar 3 familias de tareas con checks objetivos.
2. Para cada familia, construir 1 tarea `A`, 2 tareas `B` y 1 holdout `H`.
3. Ejecutar `A` con modelo local hasta fallo o limite.
4. Ejecutar tutor cloud sobre el transcript fallido de `A` para producir:
   - solucion inmediata;
   - resumen libre;
   - playbook procedimental.
5. Ejecutar cada `B` bajo las condiciones de memoria.
6. Ejecutar cada `H` para medir falsos positivos de retrieval.
7. Repetir con `n >= 5` por celda antes de sacar conclusiones.

## Familias candidatas

| Familia | Check objetivo | Por que sirve |
|---|---|---|
| Rust compile repair | `cargo check` | Errores repetibles, metodologia transferible. |
| Tool schema repair | schema validation | La causa de fallo queda tipada. |
| Multi-file edit | tests o asserts de archivo | Requiere procedimiento, no solo recordar contenido. |
| Search/tool discovery | task success + tool usage | Evalua si el playbook ensena estrategia de busqueda. |
| Benchmark harness bugfix | tests unitarios | Cercano a `braze`, con logs ricos. |

## Controles contra confounding

1. Igualar presupuesto de tokens entre `episodic`, `summary` y `procedural`.
2. Separar tareas `A` y `B` por nombres, archivos y constantes diferentes.
3. Usar holdout `H` para detectar inyeccion indebida.
4. Mantener mismo backend local, seed y sampling cuando sea posible.
5. Reportar costo de crear la memoria, no solo costo de usarla.
6. Guardar los outputs del tutor para reproducibilidad.

## Criterio de exito

La condicion `procedural` se considera prometedora si:

```text
success_rate_B(procedural) > success_rate_B(none)
leader_escalations_B(procedural) < leader_escalations_B(lead-fallback)
estimated_cost_usd_total_A_plus_B(procedural) <= lead-fallback repetido
false_positive_playbook_hits_H es bajo
```

La condicion es fuerte si ademas supera a `episodic` y `summary` bajo el mismo presupuesto.

## Criterio de retiro

Retirar o degradar la linea si:

- el playbook no mejora frente a `summary`;
- el costo de tokens reduce rendimiento en modelos locales;
- hay falsos positivos frecuentes en holdout;
- el tutor produce playbooks que requieren demasiada curacion humana;
- los resultados solo funcionan cuando `B` es casi identica a `A`.

## Decision esperada tras el primer piloto

| Resultado | Decision |
|---|---|
| `procedural` gana claramente | Implementar `LearnedPlaybookStore` e inyeccion en bench. |
| `summary` empata o gana | Replantear la unidad de memoria, quizas no necesita estructura fuerte. |
| `episodic` gana | Investigar si el problema era perdida de evidencia, no metodologia. |
| Ninguna memoria ayuda | Archivar como resultado negativo o buscar otra familia. |
| Solo `human-playbook` ayuda | Mantener como skill/manual, no como auto-destilacion. |

## Relacion con paper actual

El paper actual estudia routing/composicion de capacidad en tareas aisladas. Esta hipotesis estudia amortizacion de capacidad cloud entre sesiones. Por eso corresponde a Paper 2.
