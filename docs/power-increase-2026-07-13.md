# Adenda: aumento de potencia sobre los tres brazos indistinguibles

Fecha: 2026-07-13
Estado: **DISEÑO — plan fijado antes de correr las repeticiones
adicionales.**

Origen: al cerrar la Fase 3 (`docs/external-harness-baseline-design.md`),
las tres mediciones independientes sobre la escala 1B —
`gemma4:e4b` solo (87/95, 91.6%), compuesto completo de `braze`
(85/95, 89.5%), y el loop bare lead+executor (84/95, 88.4%) — resultaron
mutuamente indistinguibles, con CIs de Wilson anchos (~±6-7pp cada uno).
El usuario decidió angostar los intervalos antes de decidir el framing
del título/thesis del paper, en vez de decidir sobre el nulo ancho de
$n{=}95$.

## Plan (fijado antes de correr)

Agregar **10 repeticiones más** a cada uno de los tres brazos —
$n{=}285$ total por brazo (95 ya corridos + 190 nuevos), el mismo
tamaño que ya usa `lead-3brazos` (el sweep de aislamiento de mecanismo
del paper, `\S\ref{sec:mechanism}`) — no es un número arbitrario nuevo,
es el precedente ya establecido en este mismo paper para "necesito más
potencia que el n=95 default."

| Brazo | Comando | n nuevo | n total (pooled) |
|---|---|---|---|
| `gemma4:e4b` solo | `--backends "ollama:gemma4:e4b" --repetitions 10` | 190 | 285 |
| Compuesto `braze` (1B+lead) | `--backends "ollama:llama3.2:1b+lead:ollama:gemma4:e4b" --repetitions 10` | 190 | 285 |
| Loop bare | `--external "bare-lead:ollama:llama3.2:1b+lead:ollama:gemma4:e4b" --repetitions 10` | 190 | 285 |

Mismos parámetros que los sweeps originales: suite
`crates/braze-bench/suites/default.toml`, temp 0.2, Nitro,
`--no-ollama-stop`, un sweep a la vez. Sin `--seed` explícito (igual que
los sweeps originales) — cada repetición ya es una muestra nueva del
sampling no determinista de Ollama, así que no hay colisión con las
repeticiones ya corridas pese a que la numeración de repetición se
reinicia en 0 en cada invocación nueva (no importa para el pooling de
conteos agregados).

**No es un criterio nuevo** — el criterio adopt/reject de
`docs/gemma4-e4b-solo-baseline-design.md` y
`docs/external-harness-baseline-design.md` no cambia. Esto es la misma
pregunta con más potencia estadística, no una hipótesis distinta.

## Expectativa pre-declarada (antes de ver los números nuevos)

Con $n{=}285$ y los puntos estimados actuales (~88-92%), el semiancho
de Wilson baja de ~6-7pp a ~3.2-3.8pp. Si los verdaderos pass rates
subyacentes son tan parecidos como sugieren los puntos estimados
actuales, es plausible que el resultado siga siendo un nulo — pero un
nulo más fuerte: en vez de "compatible con hasta ~10pp de diferencia,"
pasaría a "compatible con hasta ~5pp de diferencia," lo cual sigue
siendo informativo (descarta efectos grandes) aunque no resuelva la
pregunta en una dirección definitiva. Se declara esto ANTES de correr
para no reencuadrar después un resultado que confirma el nulo como si
hubiera sido predicho con más precisión de la que realmente hubo.

## Nota abierta: `gpt-oss:20b` queda fuera de esta ronda a propósito

Durante esta adenda el usuario preguntó si `gpt-oss:20b` — el modelo
local recomendado del proyecto desde `docs/sweep-capacity-hardware-2026-07-13.md`
(sesión anterior a esta) — participó en algo de esto. Respuesta: no, en
ningún sweep del paper ni de las Fases 1-3. Dato relevante encontrado al
revisar: `gpt-oss:20b` solo, sin lead, ya alcanza **98.9%** (94/95) en
la misma suite `default.toml`, y **6/6 (100%)** en `g10-weak-skills`
(`error_recovery`+`distractor_selection`) — más alto que cualquiera de
los tres brazos de esta adenda (88-92%), sin ninguna de las palancas
del harness. Es una versión todavía más fuerte del hallazgo que este
documento ya está midiendo: hay un modelo disponible que resuelve la
suite casi al techo sin lead, sin rescate, sin compactación.

**Decisión (usuario, 2026-07-13)**: dejarlo explícitamente fuera de
esta ronda de power-increase — los tres sweeps en curso ya responden la
pregunta para la que fueron diseñados (`gemma4:e4b` solo vs. compuesto
`braze` vs. loop bare), y agregar un cuarto brazo a mitad de la corrida
cambiaría el alcance sin necesidad. Caveats honestos por si se retoma
después: `gpt-oss:20b` es un modelo bastante más grande nominalmente
(20B, MoE ~3.6B activos) que `gemma4:e4b` — mismo tipo de confound de
familia/escala que ya está anotado en Threats to Validity para la
mezcla Llama/Qwen — y esa medición es baseline puro, nunca se probó
como lead de un executor más chico. Candidato natural para una
iteración futura del paper (posible ítem de Fase 4/5, o una escala
adicional en la curva).

## Resultado

Los tres sweeps de +10 repeticiones corrieron limpios. Conteos pooled
(original $n{=}95$ + nuevo $n{=}190$ = $n{=}285$ por brazo):

| Brazo | $n{=}285$ | Pass rate | Wilson 95% CI | Semiancho |
|---|---|---|---|---|
| `gemma4:e4b` solo | 260/285 | 91.2% | [87.4, 94.0] | 3.3pp |
| Compuesto `braze` (1B+lead) | 253/285 | 88.8% | [84.6, 91.9] | 3.7pp |
| Loop bare lead+executor | 249/285 | 87.4% | [83.0, 90.7] | 3.9pp |

Deltas Newcombe 95% (dentro del mismo grupo de mediciones, no cross-sweep):

- Compuesto − bare = $+1.4$pp $[-4.0, +6.8]$ — cruza cero
- Bare − `gemma4:e4b` solo = $-3.9$pp $[-9.0, +1.3]$ — cruza cero (el más cercano a significativo de los tres, pero no lo alcanza)
- Compuesto − `gemma4:e4b` solo = $-2.5$pp $[-7.5, +2.5]$ — cruza cero

**Veredicto**: la expectativa pre-declarada se cumplió — sigue siendo un
nulo, pero uno mucho más fuerte. El semiancho bajó de ~6-7pp ($n{=}95$)
a ~3.3-3.9pp ($n{=}285$), y los deltas pasaron de "compatibles con
hasta ~10pp de diferencia real" a **"compatibles con hasta ~5-9pp de
diferencia real, ya no más"**. Los tres puntos estimados se mantuvieron
estables entre las dos rondas (91.6%→91.2%, 89.5%→88.8%, 88.4%→87.4% —
ningún cambio de más de 1pp), lo cual es en sí mismo una señal de que
no había un efecto grande escondido que la primera ronda simplemente no
tenía potencia para ver. **LA COMPOSICIÓN BASTA** (el veredicto de Fase
1/3) queda confirmado con más precisión, no revertido.

**Implicación para el paper**: los números de `\S\ref{sec:curve}` y
`\S\ref{sec:external}` se actualizan de $n{=}95$ a los pooled
$n{=}285$ — más precisos, mismo veredicto cualitativo. Sweeps crudos de
esta adenda:
`docs/sweep-gemma4-e4b-solo-power-2026-07-13.json`,
`docs/sweep-braze-composite-power-2026-07-13.json`,
`docs/sweep-external-bare-lead-power-2026-07-13.json`.
