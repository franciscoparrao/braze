# A/B del explorador aislado (I.7) — resultados y veredicto (2026-07-19)

**Diseño y criterio pre-registrados**:
`docs/explorador-aislado-ab-design.md` (2026-07-11, commit `4517b6d` —
transcrito como texto (2) del Apéndice B del paper). Mecanismo
implementado el 2026-07-19 (commit `3f089c6`); suite
`crates/braze-bench/suites/exploration.toml`. Sweep: 8 brazos × 30
runs = 240, Nitro (Ollama 0.30.7), binario con captura de identidad al
inicio, `BRAZE_CIRCUIT_BREAKER=off`. **Transporte: 0 en los 8 brazos —
válido.** Datos crudos: `docs/sweep-exploration-ab-2026-07-19.json`;
transcripciones preservadas.

## Resultados (5 tareas de exploración, n=25/brazo; control aparte)

| Brazo | exploración | control (sin tools) | tok/run |
|---|---|---|---|
| 3b baseline | 6/25 (24%) | 0/5 | 3.070 |
| 3b **+explore** | 9/25 (36%) | 2/5 | 5.696 |
| 3b +explore;no-prune | 7/25 (28%) | 4/5 | 4.797 |
| 3b +no-prune | 7/25 (28%) | 0/5 | 3.168 |
| 7b baseline | 14/25 (56%) | 5/5 | 5.015 |
| 7b **+explore** | 7/25 (28%) | 5/5 | 5.503 |
| 7b +explore;no-prune | 5/25 (20%) | 5/5 | 4.730 |
| 7b +no-prune | 10/25 (40%) | 3.731 | — |

Deltas Newcombe 95%:
- **3b**: +explore − baseline = **+12pp [−13.0, +35.1]** — cruza cero.
- **7b**: +explore − baseline = **−28pp [−50.1, −0.8]** — FUERA de
  cero, dañino. (Réplica direccional: explore;no-prune − no-prune =
  −20pp [−42.2, +5.3].)
- Tokens 3b: +explore usa **+85%** de tokens por run (5.696 vs 3.070,
  hijo incluido vía el `Usage` agregado).

## Contra el criterio pre-registrado

- **Adoptar vía pass rate** (≥8pp sobre baseline 3b, fuera del ruido):
  +12pp puntual pero [−13, +35] — dentro del ruido. **NO se cumple.**
- **Adoptar vía tokens** (mismo pass con ≥30% MENOS tokens): los
  tokens SUBEN 85%. **NO se cumple.**
- **Cláusula de control** (rechazar si >2/5 reps delegan lo que estaba
  en el prompt): **0 delegaciones en el control en los 8 brazos** — la
  delegación compulsiva NO ocurre. (Los fallos del control en los
  brazos 3b son llamadas a read_file para "verificar", un rasgo del 3B
  independiente de la palanca.)
- **Cláusula de iteración** (una vez, solo si el modo de falla es
  identificable Y atacable — p.ej. el padre ignora la respuesta del
  hijo → bug de render): NO aplica. El modo dominante del daño a 7b
  está identificado pero no es un bug de la palanca: en
  `find_config_value` (5/5 baseline → 1/5 +explore) el modelo NI
  SIQUIERA delega — adivina un filename, recibe el error y se rinde,
  mientras el baseline (sin la tool en el inventario) hace `grep` y
  resuelve en una ronda. Es disrupción conductual por PRESENCIA de la
  tool extra — exactamente el riesgo que la postura off-by-default
  anticipó — no un mecanismo reparable de la palanca.

## VEREDICTO: RECHAZAR (cerrar I.7 por esta vía)

Ninguna vía de adopción se cumple; a 7b hay daño fuera de cero. La
palanca queda implementada y opt-in (`+ablate:explore`,
`enable_exploration`) como aparato medido-y-rechazado — mismo posture
que constrained decoding: el mecanismo sirve para re-medir si cambian
las condiciones (otros modelos, otro prompt de delegación), pero NO se
promueve ni se recomienda.

## Lecturas laterales (reportables)

1. La predicción alternativa del diseño ("el modelo chico no sabe
   formular la delegación") NO es lo observado: el 3b SÍ delega
   (5/5 reps en 3 de 5 tareas) y mejora puntualmente — el problema es
   que la mejora no sale del ruido y cuesta +85% de tokens. Lo no
   anticipado es la dirección del 7b: la tool extra desplaza una
   estrategia propia que ya era buena (grep directo). "Not all
   scaffolding helps" otra vez, ahora con la palanca inspirada en el
   subagente `explore` de Kimi Code — que la envía como feature core
   sin medirla.
2. Kimi Code (validación de mercado que subió la prioridad de este
   A/B) envía exactamente esta palanca por default. Este resultado
   sugiere que a escalas 3-8B eso puede estar costando pass rate.
3. El control negativo resultó imposible para el 3b baseline (0/5 sin
   ninguna palanca): el 3B no resiste "verificar" un dato que ya está
   en el prompt — hallazgo de suite, útil para calibrar futuros
   controles.
