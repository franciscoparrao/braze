# Matriz del paper: executor / +planner / +lead / +planner+lead

Fecha: 2026-07-10
Contexto: el eje que faltaba de la matriz (anotado desde el A/B de SI-2 del
09 y en los pendientes del proyecto). Los cuatro brazos corren en UN solo
sweep, con UN solo binario (`braze_git_commit: e16143e`, post-Paquete 4) y
un solo ambiente — a diferencia de los sweeps previos, acá baseline y
`+lead` son anclas internas, no comparaciones cross-sweep.
Estado: **CERRADO**. Datos crudos en
`docs/sweep-matriz-4brazos-2026-07-10.json`/`.log`. (Nota operativa: el
primer intento murió por un kill externo de la sesión a 373/380 corridas
sin JSON — braze-bench escribe el output solo al final; el relanzamiento
corrió `setsid`/detached. Log del intento muerto en `.log.killed`.)

## Diseño

`default.toml` (19 tareas, 5 skills) × 4 brazos × 5 reps = 380 corridas
contra Nitro, temp 0.2, sin seed, `--no-ollama-stop`. gemma4:e4b como
planner Y como lead (el candidato validado por el A/B de 3 brazos).
`estimated_cost_usd: 0.0` presente en las 380 filas — primer sweep
completo con el pricing del Paquete 3 activo.

## Resultados

| Brazo | Pass rate (IC 95%) | single_tool | no_tool | multi_step | error_recovery | distractor | wall prom |
|---|---|---|---|---|---|---|---|
| executor solo | 68/95 (71.6%) [61.8, 79.7] | 26/35 | 15/15 | 11/15 | 2/15 | 14/15 | 4.5s |
| **+planner** | **47/95 (49.5%) [39.6, 59.4]** | 30/35 | **6/15** | **2/15** | 2/15 | **7/15** | 13.0s |
| +lead | 88/95 (92.6%) [85.6, 96.4] | 30/35 | 15/15 | 14/15 | 15/15 | 14/15 | 13.1s |
| +planner+lead | 89/95 (93.7%) [86.9, 97.1] | 31/35 | 15/15 | 14/15 | 15/15 | 14/15 | 18.7s |

Escalaciones reactivas: 0 baseline, 0 +planner, 1 +lead, **9 +planner+lead**.
`planned: 95/95` en ambos brazos con planner (la ronda de planning corrió
siempre).

## Hallazgos

1. **El planner solo no es neutro: es dañino — -22pp vs baseline** (71.6%
   → 49.5%, ICs sin solape). El veredicto A/B negativo previo (PLAN.md,
   "queda opt-in") queda corto: con gemma4:e4b como planner y qwen2.5:3b
   como executor, el plan-en-prosa destruye exactamente las skills que
   el baseline tenía sanas: `no_tool` 15/15 → 6/15, `multi_step` 11/15 →
   2/15, `distractor_selection` 14/15 → 7/15. Lo único que mejora es
   `single_tool` (26 → 30/35) — la única categoría donde "seguir el plan
   al pie de la letra" alcanza.

2. **El modo de falla del planner es degeneración, no desobediencia.**
   Las 9 fallas de `no_tool` bajo `+plan` son TODAS
   `model_backend_error` en la ronda 2 (respuesta vacía: sin texto y sin
   tool calls — la clase de fallo que el A/B de 3 brazos ya había visto
   en `shell_exec_basic`). El patrón: ronda 1 = planning, ronda 2 = el
   executor recibe el plan renderizado y **se queda mudo** en tareas
   cuya respuesta correcta era texto plano sin tools. En `multi_step`
   dominan `assertion_files` (10/13): ejecuta pasos del plan pero
   produce los archivos equivocados. Esto respalda la iteración
   pre-registrada del planner (PLAN.md): descartar planes de un solo
   paso y/o cambiar el render — y si no mueve, remoción completa.

3. **`+lead` replica exacto: 92.6%, el mismo número del A/B de 3 brazos**
   (88/95 en ambos, sweeps independientes, binarios distintos). La
   palanca es robusta a re-medición; la latencia acá (13.1s) coincide
   con el sweep del 09 (13.9s), confirmando que los 32.2s del sweep
   nocturno fueron condición de ambiente, no regresión.

4. **La composición no compone: `+planner+lead` = `+lead` a +43% de
   latencia.** 93.7% vs 92.6% — indistinguibles (89 vs 88 passes). Lo
   que sí muestra el brazo combinado es al lead **rescatando el daño del
   planner**: 9 escalaciones reactivas (vs 1 en `+lead` solo), 6 de
   ellas en `multi_step` — la skill que el planner solo hunde a 2/15.
   Con la apertura proactiva del lead + la escalación limpiando los
   tropiezos inducidos por el plan, el resultado neto vuelve al nivel de
   `+lead`... pagando la ronda de planning entera (18.7s vs 13.1s) para
   no ganar nada.

5. **Para el paper, la matriz queda así**: la curva harness-vs-escala
   tiene UNA palanca dominante (lead proactivo, +21pp) cuyo mecanismo
   está aislado experimentalmente (apertura proactiva, no escalación —
   sweep de 3 brazos), una palanca dañina en su forma actual (planner
   -22pp, mecanismo: degeneración por plan-en-prosa) y una composición
   que demuestra que el lead domina al planner (el combinado converge al
   valor del lead). El contraste planner-negativo refuerza la tesis: no
   todo andamiaje ayuda a un SLM — el que agrega contexto (plan-prosa)
   resta; el que agrega capacidad en los rounds tempranos (lead) suma.

## Limitaciones

- n=15 por celda de skill; los extremos (2/15 vs 15/15) son
  concluyentes, los matices (14 vs 15) no.
- Un solo par (planner=lead=gemma4:e4b, executor=qwen2.5:3b) — el
  resultado del planner podría cambiar con otro render/otro planner
  (esa es exactamente la iteración pre-registrada; este sweep la vuelve
  urgente o la convierte en remoción).
- Sin seed; `model_backend_error` de respuesta vacía cuenta como fallo
  del brazo (correcto para comparar harnesses — la degeneración la
  induce el plan — pero mezcla clases de fallo; el desglose del punto 2
  lo separa).

## Cómo reproducir

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/default.toml \
  --backends "ollama:qwen2.5:3b,ollama:qwen2.5:3b+plan:ollama:gemma4:e4b,ollama:qwen2.5:3b+lead:ollama:gemma4:e4b,ollama:qwen2.5:3b+plan:ollama:gemma4:e4b+lead:ollama:gemma4:e4b" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-matriz-4brazos-<fecha>.json
```

## Próximo paso

- La iteración pre-registrada del planner pasa de "opcional" a **decisiva**:
  con -22pp medidos, o el render nuevo lo rescata o se remueve (PLAN.md
  ya fija el criterio pre-registrado).
- La matriz del paper está completa: baseline / +planner / +lead /
  +ambos, todo a `e16143e`, con la atribución causal del lead en
  `docs/sweep-lead-3brazos-2026-07-10.md`.
