# Re-runs limpios de celdas contaminadas — diseño pre-registrado (2026-07-18)

**Estado**: comprometido antes de correr. Sin cláusula de adopción: son
re-mediciones para reemplazar exclusiones analíticas por datos limpios.
Precedente en el propio paper: el re-run del brazo 3B task-list del
planner-ab (58 de 62 fallos eran de red; se re-corrió y se reportó el
re-run disclosando el intento contaminado).

## Qué se re-corre y por qué

### Bloque 1 — `1B +plan+lead` (celda de la curva, hallazgo nuevo)

`docs/curve-transport-audit-2026-07-18.md` § Hallazgo 1: 30 de 95 runs
murieron por transporte (0/30 pass), arrastrando la celda de 89.2%
(58/65 limpio) a los **61.1%** que el paper publica. La claim derivada
—"a 1B la composición sigue por debajo del lead solo, el daño
persiste"— es un artefacto.

**Anclaje within-sweep**: no basta re-correr la celda sola, porque
compararla contra el `+lead` del sweep original (commit `e9b841e`,
2026-07-10) sería una comparación cross-sweep y cross-commit que la
propia disciplina del paper (§setup) prohíbe. Se re-corre **junto a su
celda de comparación**, en un solo sweep de dos brazos:

- `ollama:llama3.2:1b+plan:ollama:gemma4:e4b+lead:ollama:gemma4:e4b`
- `ollama:llama3.2:1b+lead:ollama:gemma4:e4b` (ancla)

95 runs cada uno = **190 runs**. Mismo precedente que el re-run
controlado del fix de escalación reactiva que el paper ya reporta
("190 runs, including an anchor cell").

### Bloque 2 — brazos coder del planner-ab

`docs/emse-r2-analysis-2026-07-17.md` § 4: 10, 2 y 8 fallos de
transporte en baseline, user-role y task-list del coder. Hoy el paper
los reporta con **exclusión analítica**; datos limpios son más fuertes
y eliminan la crítica del reviewer blind sobre criterios post-hoc
aplicados a datos ya vistos (Issue 3c).

- `ollama:qwen3.5-coder`
- `ollama:qwen3.5-coder+plan:ollama:gemma4:e4b`
- `ollama:qwen3.5-coder+plan:ollama:gemma4:e4b+ablate:task-list`

95 runs cada uno = **285 runs**, un solo sweep de tres brazos
(comparaciones within-sweep por construcción).

## Protocolo (idéntico para ambos bloques)

- Binario: worktree `bfcl-anchor` en su HEAD (incluye el retry de
  transporte, commit `4334ce4`). **No es el binario original de los
  sweeps que se corrigen** — por eso cada bloque se corre con su propia
  ancla y solo se citan deltas within-sweep; los niveles absolutos no se
  comparan contra los sweeps viejos.
- `BRAZE_OLLAMA_TRANSPORT_RETRIES=6` (la lección del 2026-07-18).
- **Sin** `--no-ollama-stop`: la suma de residentes de estos brazos
  supera los 16GB de Nitro si quedan todos cargados (causa raíz del
  primer sweep BFCL arruinado).
- `--repetitions 5`, temp 0.2, suite `default.toml`, timeout 180s,
  un sweep a la vez, `BRAZE_BENCH_KEEP_SESSIONS=1`.
- Verificación obligatoria post-sweep antes de citar cualquier número:
  conteo de transporte por brazo (criterio de la auditoría). Si algún
  brazo supera **2%** de runs de transporte, el sweep se descarta y se
  re-corre; no se aplica exclusión analítica sobre datos ya vistos.

## Lecturas pre-declaradas

**Bloque 1.** El paper adoptará el número del re-run para la celda y
reescribirá la claim según el delta `composición − lead` medido en ese
sweep:

| Resultado | Qué escribe el paper |
|---|---|
| Delta cruza cero (esperado, ~0pp) | El lead **rescata completamente** el daño del planner a todas las escalas medidas; se elimina la lectura de "capacidad finita de recuperación" y se corrige la Fig. 1 |
| Delta negativo fuera de ruido | La claim original se sostiene sobre datos limpios; se reporta el re-run y se disclosa que el número previo estaba inflado por transporte |
| Delta positivo fuera de ruido | Resultado nuevo (la composición supera al lead solo a 1B); se reporta con la misma prominencia y se revisa §discussion |

**Bloque 2.** Reemplaza los números corregidos-por-exclusión de
§planner. Se espera que reproduzcan la lectura actual (el ceiling
recupera el colapso sin superar demostrablemente su baseline); cualquier
divergencia se reporta como tal, y en todo caso el paper pasa de citar
"datos con exclusión post-hoc" a "datos limpios más el intento
contaminado disclosado".

En ambos bloques el intento contaminado original queda commiteado y
citado; nada se sustituye en silencio.

## Adenda 2026-07-19 — primer intento del Bloque 2 INVALIDADO (regla del 2%)

El primer intento del Bloque 2 (corrido 2026-07-19, preservado en
`docs/sweep-rerun-block2-coder-planner-2026-07-19.contaminated-breaker-probe.json`)
se descarta por la regla pre-registrada: baseline coder 4/95 = 4.2% de
transporte (> 2%), y los brazos 2-3 fueron destruidos por un **bug del
circuit breaker** que el propio sweep expuso: tras abrirse por 5 fallos
de transporte consecutivos, el probe half-open fue cancelado por el
timeout por-tarea del runner (que dropea el future de `run_turn`) y el
slot del probe quedó reclamado hasta el PROBE_TIMEOUT de 600s —
durante esa ventana cada tarea falló instantáneo con "a probe call is
already in flight" (140 filas harness_error; el brazo task-list quedó
0/95). Fix: `Guard` del breaker ahora libera el slot del probe en su
`Drop` cuando nunca reportó outcome (con token de claim para no liberar
el slot de un reclamante posterior), + 3 tests de regresión. El re-run
del Bloque 2 se lanza con el binario arreglado Y con
`BRAZE_CIRCUIT_BREAKER=off` (kill-switch documentado): en contexto de
sweep, el retry de transporte + el timeout por tarea ya contabilizan el
transporte, y un trip del breaker solo puede cascadear — la palanca es
para uso interactivo. Se disclosa aquí en vez de re-litigar el
protocolo: la regla de validez y las lecturas pre-declaradas no cambian.
