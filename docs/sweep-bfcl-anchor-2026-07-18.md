# Ancla BFCL — resultados E1-E4 (sweep 2026-07-18/19)

**Qué es**: la validación externa que las 4 rondas de review del Paper 1
pidieron de forma convergente (issue #1): ¿la palanca central del paper
— `+lead:` sobre un worker de 1B — sobrevive fuera de las tareas
autorales? Diseño pre-registrado en
`docs/bfcl-anchor-design-2026-07-18.md` (60 tareas muestreadas
determinísticamente de BFCL con seed 42, 3 categorías × 20; grader
offline AST comprometido ANTES del sweep en
`docs/bfcl-anchor-grader-2026-07-18.py`); lecturas E1-E4 pre-declaradas.

**Ejecución**: 5 brazos × 300 runs = 1.500 corridas contra Nitro
(comando exacto en `docs/bfcl-anchor-RESUME.md`; binario `bc30a80`,
incluye el fix K-11 de `OLLAMA_HOST` — sin él, el `ollama stop` entre
brazos era un no-op remoto y el pico de RAM repetía el OOM del intento
v1). Duración ~9h de pared, dominada por el brazo `qwen3.5-coder`
(thinking model, turnos multi-ronda largos en `irrelevance`).

## Validez (regla pre-registrada del 2%)

| Brazo | Transporte |
|---|---|
| gemma4:e4b | 0/300 (0.0%) |
| llama3.2:1b | 0/300 (0.0%) |
| llama3.2:1b+lead:gemma4:e4b | 0/300 (0.0%) |
| qwen2.5:3b | 3/300 (1.0%) |
| qwen3.5-coder | 2/300 (0.7%) |

**SWEEP VÁLIDO** — ningún brazo supera el 2%. Primer intento limpio de
cuatro: v1 (1296/1500 fallos de transporte, OOM por `--no-ollama-stop`),
v2 (1392/1500, red degradada sin retry), v3 (abortado, Nitro 8× lento
por contención) quedaron preservados en el repo para el disclosure del
paper. Las palancas que lo volvieron corrible:
`BRAZE_OLLAMA_TRANSPORT_RETRIES=6` y K-11.

## Niveles de grading

Tres niveles, del más laxo al más exigente: **online** (identidad de la
tool — el proxy del runner), **offline** (AST de argumentos, el estilo
BFCL real — el nivel que cuenta), y la referencia de la **suite
autoral** del paper.

| Brazo | online | offline [IC 95%] | suite autoral |
|---|---|---|---|
| gemma4:e4b | 95.7% | **92.7%** [89.1, 95.1] | 91.6% |
| llama3.2:1b | 67.7% | **20.7%** [16.5, 25.6] | 18.9% |
| 1b+lead:e4b | 96.0% | **92.7%** [89.1, 95.1] | 89.5% |
| qwen2.5:3b | 75.7% | **71.7%** [66.3, 76.5] | 68.4% |
| qwen3.5-coder | 71.3% | **80.3%** [75.5, 84.4] | 97.9% |

## Lecturas pre-declaradas

**E1 — ¿El ordenamiento de baselines transfiere?** SÍ en offline:
`qwen3.5-coder > qwen2.5:3b > llama3.2:1b`, idéntico a la suite autoral.
(El nivel online invierte coder/3b — artefacto del proxy de identidad
con thinking models, ver E4.) Los niveles absolutos offline quedan
además notablemente cerca de los autorales (Δ ≤ 3.3pp en 4 de 5 brazos);
la excepción es qwen3.5-coder (80.3% vs 97.9% — BFCL le exige formatos
de argumento más variados que la suite autoral).

**E2 — La palanca lead a 1B (el resultado central).** En la suite
autoral: +70.5pp. En el ancla externa, offline: **+72.0pp, Newcombe
[+65.9, +76.9], cluster-bootstrap [+61.0, +82.7] — fuera de cero por
amplio margen y a 1.5pp del efecto autoral.** Online: +28.3pp, también
fuera de cero. La palanca central del paper transfiere a un benchmark
que el autor no escribió.

**E3 — Pinned ceiling (esperado: nulo).** Confirmado: composite vs lead
solo = +0.0pp offline (Newcombe [-4.3, +4.3], cruza cero). El lead fija
el techo; el worker 1B no lo arrastra hacia abajo — coherente con el
mecanismo propuesto en el paper.

**E4 — Gap identidad→argumentos (online − offline).** La historia de la
tesis en un número por brazo: **+47.0pp en llama3.2:1b** — el 1B nombra
la tool correcta pero destroza los argumentos, exactamente la clase de
fallo que las palancas del harness (coerción de schema, rescate,
escalación) existen para compensar. En los modelos competentes el gap
es +3-4pp. Y **-9.0pp en qwen3.5-coder** (offline MEJOR que online):
produce calls correctas que el proxy de identidad pierde — el grading
online subestima a los thinking models. Caveat para § Threats: los
proxies de identidad no son neutrales entre familias de modelos.

## Por categoría (offline)

| Brazo | irrelevance | multiple | simple |
|---|---|---|---|
| gemma4:e4b | 88/100 | 100/100 | 90/100 |
| llama3.2:1b | 18/100 | 19/100 | 25/100 |
| 1b+lead:e4b | 88/100 | 100/100 | 90/100 |
| qwen2.5:3b | 36/100 | 91/100 | 88/100 |
| qwen3.5-coder | 52/100 | 100/100 | 89/100 |

`irrelevance` (declinar la llamada cuando ninguna tool aplica — el eje
que BFCL v4 ahora pondera 10% como "hallucination measurement") es la
categoría discriminante: qwen2.5:3b cae a 36% y qwen3.5-coder a 52%
mientras gemma4:e4b sostiene 88%. Nota: los 1b+lead calcan a e4b en las
tres categorías — consistente con E3 (el techo es del lead).

## Archivos

- `docs/sweep-bfcl-anchor-2026-07-18.json` (crudo, 1.500 filas) +
  `.offline-grades.json` (grader AST).
- Análisis reproducible: `docs/bfcl-anchor-analysis-2026-07-18.py
  --sweep <json> --grades <grades>`.
- Intentos fallidos preservados: `.contaminated-nitro-oom.json`,
  `.contaminated-v2.json`, `.aborted-v3-nitro-contention.log`.
- Sesiones preservadas del sweep (spot-checks): no versionadas, en el
  árbol local del worktree.
