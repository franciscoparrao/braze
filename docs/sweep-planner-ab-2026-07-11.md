# A/B del planner ITERADO: la degeneración era un bug de render, no del planner

Fecha: 2026-07-11
Contexto: la iteración pre-registrada del planner (PLAN.md § "Split
planificador/ejecutor") contra su criterio de decisión. La curva
harness-vs-escala (`docs/sweep-curva-multiescala-2026-07-10.md`) había
medido el planner VIEJO (prosa como texto de assistant) colapsando los
dos extremos de la escala a **49%** por degeneración (respuestas
vacías). La iteración (commit `e8c7d3f`) aplicó los dos cambios
registrados; este A/B los mide contra las dos referencias de daño.
Estado: **CERRADO — el planner se RESCATA, no se remueve.** Datos:
`docs/sweep-planner-ab-2026-07-11.json` (5 brazos limpios) +
`docs/sweep-planner-ab-3b-tasklist-rerun-2026-07-11.json` (el 6º brazo,
re-corrido por contaminación de red — ver nota).

## Diseño

3 brazos × 2 executors (las dos referencias de daño de la curva) × 19
tareas × 5 reps, binario en HEAD `28f7a53`, Nitro, temp 0.2:

- **baseline** — sin planner.
- **plan-new** — planner nuevo: `PlanCreated` renderizado con **rol
  user** (no assistant) + descarte de planes de un solo paso numerado
  (`count_numbered_steps < 2`).
- **plan+task-list** — `+ablate:task-list`: el plan **siembra la lista
  tipada** (C′.2) en vez de entrar como prosa; el executor la ve como
  resumen compacto re-inyectado por ronda.

## Resultados

Pass rate (n=95, IC 95% Wilson):

| Executor | baseline | plan-new (prosa user-role) | plan+task-list (planner→tasks) |
|---|---|---|---|
| qwen2.5:3b | 67% [57,76] | 63% [53,72] | **79% [70,86]** |
| qwen3.5-coder | 86% [78,92] | **96% [90,98]** | 91% [83,95] |

Recordatorio de la curva (planner VIEJO, prosa-assistant): 3b y coder
ambos a **49%**.

`error_recovery` (n=15), la skill discriminante:

| Executor | baseline | plan-new | plan+task-list |
|---|---|---|---|
| qwen2.5:3b | 3/15 | 6/15 | **10/15** |
| qwen3.5-coder | 13/15 | **15/15** | 15/15 |

## Hallazgos

1. **La degeneración era un bug de RENDER, no una propiedad del
   planner.** El 49% de la curva estaba dominado por respuestas vacías
   (coder: 35 de 48 fallas). Con el render user-role, esa firma
   desaparece: coder plan-new tiene **1** respuesta vacía. Los dos
   catastróficos 49% suben a 63% (3b) y 96% (coder). El artefacto que la
   matriz y la curva diagnosticaron queda explicado y corregido.

2. **El planner iterado AYUDA en ambas escalas — con distinta entrega
   según el tamaño del executor:**
   - **En el modelo capaz (coder), la prosa user-role gana**: 96% vs 86%
     baseline (+10pp), `error_recovery` 13→15/15, `single_tool`
     28→35/35. El plan bien renderizado encamina al thinking model.
   - **En el modelo chico (3b), el planner→tasks gana**: 79% vs 67%
     baseline (+12pp), `error_recovery` 3→10/15, `single_tool` 26→31/35.
     La lista tipada compacta guía mejor a un 3B que la prosa (plan-new
     3b queda en 63%, apenas bajo baseline, con `multi_step` regresando
     9→4). El plan como ESTADO estructurado, no como texto a interpretar,
     es lo que un modelo con poca capacidad de seguir instrucciones
     largas necesita — exactamente la tesis de C′.2.

3. **Veredicto del criterio pre-registrado: el planner se RESCATA, no se
   remueve.** El criterio era "si no mueve `multi_step`/`error_recovery`
   → remoción completa". Mueve `error_recovery` en las cuatro celdas con
   planner (3→6, 3→10, 13→15, 13→15) y el pass rate agregado sube sobre
   baseline en tres de los cuatro brazos con planner. Sigue **opt-in**
   (no default), pero deja de ser una apuesta fallida: es una palanca de
   ganancia scale-dependent. `multi_step` es el punto débil (regresa en
   3b plan-new y plan+task-list) — anotado, no bloqueante.

4. **La composición ideal por escala invierte la de la curva**: allá el
   planner dañaba y el lead rescataba; acá, con el render arreglado, el
   planner→tasks es la mejor entrega para el executor chico y la
   prosa-user-role para el capaz. Un futuro brazo `+planner+lead` con el
   planner arreglado es el siguiente eslabón (no medido acá).

## Nota de contaminación (por qué se re-corrió un brazo)

La corrida original del brazo `3b+plan+task-list` reportó 35%, pero **58
de sus 62 fallas eran `ollama request failed` en la ronda 0** — fallos
de red/transitorios de Nitro (H-19: el retry con backoff que A′.3 agregó
es solo para backends cloud; Ollama aborta el turno al primer fallo, por
el dictamen v5). Concentrados enteramente en ese brazo (los otros 5
tienen 0-3 fallos de red), eran una ventana de inaccesibilidad de Nitro,
no una propiedad del task-list. El re-run limpio (0 fallos de red) dio
79%. El brazo `coder+plan+task-list` tiene ~8 fallos de red menores, así
que su 91% es un piso levemente subestimado. La lección operativa
refuerza el pendiente de infra Nitro (IP fija, monitoreo) y sugiere
reconsiderar el opt-in de retry para Ollama en sweeps largos.

## Limitaciones

- El baseline de coder acá (86%) es más bajo que en la curva (98%) —
  cross-sweep, distinta sesión térmica de Nitro y varianza n=95. Los
  deltas WITHIN-sweep (plan-new +10pp sobre este baseline) son lo
  comparable, no los valores absolutos entre sweeps.
- El descarte de single-step hace que muchos turnos corran SIN plan
  (plan-new: `planned=31/95` en 3b, `29/95` en coder) — el resto tuvo
  planes de un paso descartados. El brazo mide "planner cuando el plan
  sobrevive el descarte", que es el diseño.
- Un solo planner (gemma4:e4b); n=15 por celda de skill.

## Cómo reproducir

```bash
L="ollama:gemma4:e4b"
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 \
cargo run -p braze-bench -- crates/braze-bench/suites/default.toml \
  --backends "ollama:qwen2.5:3b,ollama:qwen2.5:3b+plan:$L,ollama:qwen2.5:3b+plan:$L+ablate:task-list,ollama:qwen3.5-coder,ollama:qwen3.5-coder+plan:$L,ollama:qwen3.5-coder+plan:$L+ablate:task-list" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-planner-ab-<fecha>.json
```

## Próximo paso

Actualizar PLAN.md: el veredicto A/B del planner pasa de "rechazado, con
iteración pendiente" a "rescatado — opt-in, ganancia scale-dependent
(planner→tasks para SLM, prosa-user-role para capaz)". Siguiente eslabón
opcional: `+planner+lead` con el planner arreglado.
