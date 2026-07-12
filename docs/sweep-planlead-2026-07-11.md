# El eslabón final: ¿el planner arreglado compone con el lead?

> **⚠ Nota posterior (2026-07-12)**: la auditoría v7 encontró que en el brazo
> `+plan+lead+ablate:task-list` de ESTE sweep la escalación reactiva del lead
> estaba muerta por construcción (J-2, el resumen de la task list reseteaba la
> racha de fallos). El re-run controlado post-fix
> (`docs/sweep-planlead-taskslead-postfix-2026-07-11.md`) **confirma el
> veredicto** con el mecanismo limpio: tasks+lead 83.2% vs 92.6% del ancla,
> con 9 escalaciones reactivas reales disparando. La observación 2 y la
> recomendación 4 quedan vigentes sin cambios.

Fecha: 2026-07-11
Contexto: tras el rescate del planner (`docs/sweep-planner-ab-2026-07-11.md`),
quedaba la última pregunta del split: si las entregas arregladas del plan
**suman** sobre la palanca dominante (+lead) o si el lead ya satura lo que
el plan aporta. 3 brazos × {qwen2.5:3b, qwen3.5-coder} × 19 tareas × 5
reps = 570 corridas, binario `66db995`, Nitro, cero fallos de red.
Estado: **CERRADO.** Datos: `docs/sweep-planlead-2026-07-11.json`/`.log`.

## Resultados

| Executor | +lead (ancla) | +plan(prosa)+lead | +plan(tasks)+lead |
|---|---|---|---|
| qwen2.5:3b | 92% [84,96] | **95% [88,98]** | 86% [78,92] |
| qwen3.5-coder | 94% [87,97] | 93% [86,96] | 93% [86,96] |

Skills débiles del 3b, brazo `plan(prosa)+lead`: `multi_step` **15/15**,
`error_recovery` **15/15**, `distractor_selection` **15/15** — la única
celda del proyecto con las tres skills discriminantes perfectas en el 3b.
Latencia: 18.4s vs 12.2s del lead solo (+50%).

## Hallazgos

1. **La composición prosa+lead en el 3b es el mejor brazo del proyecto
   para ese modelo (95%), pero la ganancia sobre el lead solo (+3pp) es
   marginal y dentro del ruido** con n=95. La señal por skill sí es
   sugerente (las tres skills débiles perfectas por primera vez), pero
   el claim honesto es "no empeora y puede sumar", no "compone". En el
   techo (coder), nada compone: 94/93/93 — el lead o el propio modelo ya
   saturan.

2. **La entrega óptima del plan SE INVIERTE cuando hay lead presente.**
   Solo, el planner→tasks era la mejor entrega para el 3b (79% vs 63%
   de la prosa); con lead, la prosa gana y el task-list RESTA (86% vs
   92% del lead solo, con `multi_step` cayendo a 9/15). Lectura: la
   lista tipada re-inyectada y la apertura proactiva del lead proveen
   el mismo servicio (anclar el estado del turno) y se interfieren —
   doble andamiaje de estado es peor que uno. Coherente con toda la
   serie: el andamiaje correcto depende no solo de la escala sino de
   QUÉ MÁS hay en el harness.

3. **`3b+lead` replica por cuarta vez en la banda 92-92.6%** (92.6 /
   92.6 / 92 / 92 en cuatro sweeps y tres binarios distintos) — el
   ancla más estable del proyecto.

4. **Recomendación de configuración**: para un executor chico, `+lead`
   solo sigue siendo el sweet spot costo/beneficio (92% a 12s); la
   composición prosa+lead es defendible si el +3pp importa más que el
   +50% de latencia; el task-list NO debe combinarse con lead (sí es
   la mejor entrega sin lead). Para el techo, nada de esto — el modelo
   solo (98% en la curva) o con lead por velocidad.

## Limitaciones

- +3pp con ICs solapados: distinguirlo de ruido pediría n≥3× — se
  reporta como direccional, no como efecto establecido.
- Un solo planner/lead (gemma4:e4b); las interacciones de la
  observación 2 podrían ser específicas del par.

## Cómo reproducir

```bash
L="ollama:gemma4:e4b"
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 \
cargo run -p braze-bench -- crates/braze-bench/suites/default.toml \
  --backends "ollama:qwen2.5:3b+lead:$L,ollama:qwen2.5:3b+plan:$L+lead:$L,ollama:qwen2.5:3b+plan:$L+lead:$L+ablate:task-list,ollama:qwen3.5-coder+lead:$L,ollama:qwen3.5-coder+plan:$L+lead:$L,ollama:qwen3.5-coder+plan:$L+lead:$L+ablate:task-list" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-planlead-<fecha>.json
```

## Cierre de la historia del split

Con esto, la historia del planner queda completa y cerrada para el
paper: A/B negativo → matriz que lo vuelve decisivo → curva que muestra
el mecanismo en ambos extremos → iteración pre-registrada que lo rescata
→ composición medida (marginal con prosa, negativa con tasks, nula en el
techo). No quedan sweeps del split pendientes.
