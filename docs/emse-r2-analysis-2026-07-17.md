# Análisis R2 EMSE — homogeneidad, clustering, costos y contaminación del planner-ab (2026-07-17)

Fuente: segunda ronda de pre-submission review
(`~/vault/journals/emse/reviews-generated/2026-07-17_19-00_braze-harness-paper.md`).
Este documento contiene los análisis nuevos que respaldan las ediciones del
manuscrito de esa ronda. Script: reproducible desde los JSONs commiteados en
`docs/` (los comandos exactos están inline abajo; todos operan solo sobre
`passed`, `task_id`, `failure_cause`, `run_error`, `wall_time_ms`,
`input_tokens`, `output_tokens`).

## 1. Homogeneidad por slice de los brazos pooled (Issue 1 de R2)

Los tres brazos de n=285 del nulo de tres vías se poolean desde dos slices
cada uno (n=95 original + n=190 power). Pass rates por slice y test exacto
de Fisher bilateral sobre la tabla pass/fail:

| Brazo | Slice 1 | Slice 2 | Fisher p |
|---|---|---|---|
| composite 1B+lead | 85/95 (89.5%) — curva, commit `e9b841e` | 168/190 (88.4%) — power, `ec61f5e` | 0.845 |
| gemma4:e4b solo | 87/95 (91.6%) — `ec61f5e` | 173/190 (91.1%) — `ec61f5e` | 1.000 |
| bare loop | 84/95 (88.4%) — `ec61f5e` | 165/190 (86.8%) — `ec61f5e` | 0.850 |

Ningún par de slices muestra heterogeneidad detectable. **Solo el brazo
composite cruza commits** (e9b841e → ec61f5e, la frontera de hardening);
sensibilidad same-commit:

- composite−solo **pooled** (como en el paper): −2.5pp Newcombe [−7.5, +2.5]
- composite−solo **same-commit** (composite = solo los 190 de `ec61f5e`):
  −2.8pp [−8.8, +2.6]
- composite−bare same-commit: +1.1pp [−5.3, +6.8] (pooled: +1.4 [−4.0, +6.8])

El veredicto cualitativo es idéntico en ambas bases; los point estimates se
mueven <0.5pp.

## 2. CIs cluster-robust por tarea (Issue 2 de R2)

n=95 = 19 tareas × 5 reps (n=285 = ×15): las corridas no son i.i.d. —
clustean por tarea. Bootstrap por tarea (resampleo de las 19 tareas con
reemplazo, B=20.000, seed 42; para deltas el resampleo de tareas es
conjunto entre brazos):

**Niveles** (mucho más anchos que Wilson — cuantifican generalización a una
población de tareas, no precisión sobre la suite fija):

| Brazo | Rate | Wilson 95% | Cluster-boot 95% |
|---|---|---|---|
| solo pooled n=285 | 91.2% | [87.4, 94.0] | [79.3, 98.6] |
| composite pooled n=285 | 88.8% | [84.6, 91.9] | [76.8, 96.5] |
| bare pooled n=285 | 87.4% | [83.0, 90.7] | [74.4, 97.2] |
| 1B baseline n=95 | 18.9% | [12.3, 28.0] | [6.3, 33.7] |

**Deltas** (aquí la dificultad compartida por tarea cancela en pares con
patrón de fallo similar):

| Delta | Point | Newcombe | Cluster-boot |
|---|---|---|---|
| composite−solo pooled | −2.5pp | [−7.5, +2.5] | **[−7.4, +2.1]** — robusto |
| bare−solo pooled | −3.9pp | [−9.0, +1.3] | [−17.5, +11.2] — NO robusto |
| composite−bare pooled | +1.4pp | [−4.0, +6.8] | [−13.3, +15.8] — NO robusto |
| lead vs baseline 1B (n=95) | +70.5pp | — | [+54.7, +84.2] — robusto |

Lectura: el nulo composite−solo (y el efecto +70pp del lead a 1B) son
robustos al clustering. Las comparaciones que involucran al **bare loop
pierden mucha precisión** bajo clustering porque sus fallos se concentran
en tareas específicas (multi_step/distractor) — el claim "descarta un
efecto oculto mayor a ~8pp" solo es defendible para composite−solo, no
para los pares con el bare.

## 3. Costos composite vs solo vs 7B (Issue 3 de R2)

Promedios por run. Los tres power sweeps (mismo día, mismo commit
`ec61f5e`, mismo nodo, un sweep a la vez) son comparables entre sí:

| Brazo (power, n=190) | input tok | output tok | wall | timeouts |
|---|---|---|---|---|
| solo gemma4:e4b | 2.910 | 371 | 25.2s | 7 |
| composite 1B+lead | 2.779 | 342 | 23.9s | 5 |
| bare loop | (no instrumenta tokens) | — | 16.9s | 0 |

**composite/solo: input ×0.95, output ×0.92, wall ×0.95** — el composite
cuesta lo mismo que el lead solo. No hay caso de costo para la composición
en esta suite.

Referencia de la curva (cross-sweep, solo orden de magnitud):

| Brazo (curva, n=95) | input | output | wall |
|---|---|---|---|
| 1B baseline | 2.046 | 159 | 3.6s |
| 3B baseline | 2.458 | 78 | 2.3s |
| 7B baseline | 2.401 | 75 | 5.1s |
| composite 1B+lead | 2.776 | 373 | 12.8s |

**composite vs 7B baseline: input ×1.16, output ×4.96, wall ×2.52** — el
claim del abstract "at a fraction of the 7B's inference cost" es **falso**
en tokens y wall-time medidos; se elimina del paper. (El único sentido en
que podría sostenerse — footprint de memoria de modelos más chicos — no se
midió y no se afirma.)

## 4. Contaminación de infraestructura en el sweep planner-ab (hallazgo nuevo)

Al investigar el swing del baseline del coder entre sweeps (97.9% curva vs
86.3% planner-ab — issue menor de R2), se encontró que el evento de red
transitorio ya disclosed para el brazo 3B task-list (58 fallos) **también
alcanzó a los brazos del coder del mismo sweep**, de forma asimétrica.

Criterio "infra-like": `failure_cause == model_backend_error` **y**
(`wall_time_ms < 1s` — el request nunca llegó — o `run_error` contiene
"stream"/"request to model backend failed"). Los empty-response genuinos
("model's response had no text") **no** califican.

| Brazo (planner-ab, coder) | Raw | Infra-like | Excluyendo infra |
|---|---|---|---|
| baseline | 82/95 (86.3%) | 10 de 13 fallos | 82/85 (96.5%) |
| +plan user-role | 91/95 (95.8%) | 2 de 4 | 91/93 (97.8%) |
| +plan task-list | 86/95 (90.5%) | 8 de 9 | 86/87 (98.9%) |

Los brazos 3B del mismo sweep: 0 fallos infra-like (baseline y user-role);
el task-list 3B es el re-run limpio ya disclosed (75/95, 0 infra).

**Consecuencia para el claim del rescate en el ejecutor fuerte:**

| Delta coder vs baseline | Raw | Infra-excluido |
|---|---|---|
| user-role | **+9.5pp [+1.2, +18.2]** | **+1.4pp [−4.5, +7.9]** |
| task-list | +4.2pp [−5.1, +13.6] | +2.4pp [−3.2, +8.8] |

El "+10pp on the strongest executor" del paper era en gran parte artefacto
de los 10 fallos de transporte que deprimieron el baseline. Con la
exclusión, la historia honesta del coder es: **la entrega corregida
elimina el colapso (49.5% → ~98%) pero no supera demostrablemente su
baseline**. Lo que sí sobrevive: `error_recovery` 13/15 → 15/15 (0 infra
en ambos lados, direccional a n=15) y el **+11.6pp del 3B task-list**
(75/95 = 78.9% vs 64/95 = 67.4%, ambos brazos limpios).

Nota: el swing 97.9→86.3 del baseline del coder queda así explicado
(infra, no hardening de asserts): excluyendo infra ambos sweeps dan
96.5–97.9%, consistentes.

## 5. Decisiones editoriales derivadas

1. §setup: definir qué licencia "within-sweep", con la homogeneidad de §1 y
   la sensibilidad same-commit como respaldo del pooling.
2. §setup + §curve/§external: reportar cluster-boot junto a Wilson en los
   claims headline; restringir "rules out >8pp" a composite−solo.
3. §discussion (Practical implication) + abstract: eliminar "fraction of
   the 7B's inference cost"; recomendación pasa a "corre el lead solo"
   con los números de §3.
4. §curve/abstract/contribuciones: pinned-ceiling como lectura primaria;
   el decay del delta es corolario mecánico (baseline sube, composite
   clavado en el techo del lead).
5. §planner + Fig. 2 + abstract/contribución 4/conclusión: números del
   coder con exclusión infra; el rescate del ceiling se reporta como
   "harm eliminated", no "+10pp"; regenerar Fig. 2.
6. Fig. 1: banda del solo actualizada al pooled n=285 (91.2% [87.4, 94.0]).
7. Threats: extender el disclosure del evento de red a los brazos coder
   del planner-ab + describir el criterio de exclusión.

## 6. Adenda ronda BLIND (2026-07-17, noche) — análisis nuevos

Fuente: review blind b1
(`~/vault/journals/emse/reviews-generated/blind/2026-07-17_23-11_braze-harness-paper_b1.md`,
ejecutada por subagente fresco sin contexto de sesión).

### 6.1 Evaluación LITERAL del criterio pre-registrado del planner (3B)

Por skill (baseline / user-role / task-list re-run):
no_tool 15/15 / 15/15 / 15/15 · single_tool 26/35 / 25/35 / 31/35 ·
multi_step 9/15 / 4/15 / 7/15 · error_recovery 3/15 / 6/15 / 10/15 ·
distractor 11/15 / 10/15 / 12/15.

- **Target del criterio** (multi_step+error_recovery combinado):
  baseline 12/30 (40.0%), Wilson [24.6, 57.7]; task-list 17/30
  (56.7%) — **dentro** del intervalo del baseline (56.7 < 57.7);
  user-role 10/30 (33.3%) — peor que baseline.
- **Delta agregado task-list**: +11.6pp Newcombe **[−1.0, +23.7]** —
  cruza cero marginalmente. El "one demonstrable gain" de la versión
  anterior era insostenible; corregido en paper y Fig. 2.
- **Guardrail** (no degradar los otros 3 skills): pasa.
- **Cláusula de token cost**: task-list 5.352 in / 338 out por run vs
  baseline 2.635 / 86 — ~2× input, ~4× output; cuenta en contra.
- **Veredicto literal: ningún delivery se adopta.** El paper ahora lo
  reporta así, con nota de desviación fechada (se mantiene el lever
  opt-in en vez de removerlo, porque el hallazgo diagnóstico es la
  contribución de la sección).

### 6.2 Probe token-level del empty-response (mecanismo)

Fallos con firma "no text and no tool calls" en la curva:
- coder +plan: 35 empty de 48 fallos; output_tokens min 44 / p50 74 /
  max 619 (17/35 con >100).
- 1B +plan: 37 empty de 95; output_tokens min 47 / p50 272 / max 594
  (28/37 con >100).

Lectura: los turnos "vacíos" **no son silencio literal** — el modelo
genera 44–619 tokens que no emergen en ningún canal usable. El 1B no
tiene canal de razonamiento → reasoning-budget no explica ambos
extremos; pero una contribución de serving/template (mishandling del
mensaje assistant-role inyectado) no queda excluida, y el fix
(user-role) también arreglaría ese artefacto. El paper baja el
mecanismo a "consistent with" y nombra el experimento discriminante
como future work.

### 6.3 Suite noisy y outlier de Fig. 3a

`tool-search.toml`: 6 tareas (noisy_read_file/grep/write_file/no_tool/
multi_step/distractor) × 15 reps = 90 por brazo — la descripción "same
19 tasks augmented" del paper era incorrecta; corregida. Outliers
~48K input: runs full-inventory 3B con 6 rondas y 5 compactaciones
(re-pagan el catálogo de 206 tools por ronda); explicado en caption.

### 6.4 Tabla de modelos (consultada al server Ollama de Nitro, /api/show)

llama3.2:1b 1.2B Q8_0 · qwen2.5:3b 3.1B Q4_K_M · qwen2.5:7b 7.6B
Q4_K_M · qwen3.5-coder 9.7B Q4_K_M · gemma4:e4b 8.0B total (~4B
activos, MoE) Q4_K_M · gemma4:e2b 5.1B Q4_K_M · gemma3:1b 1.0B Q4_K_M.
Disclosure clave: **el lead es comparable en tamaño total al ejecutor
7B**. `num_ctx=8192` se pide explícitamente en cada request
(DEFAULT_NUM_CTX de braze-model/ollama.rs); el engine compacta por
debajo de ese límite → los prompts full-inventory (~7.5K/request)
caben sin truncación silenciosa.
