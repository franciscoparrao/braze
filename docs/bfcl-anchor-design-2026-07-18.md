# Ancla externa BFCL — diseño pre-registrado (2026-07-18)

**Estado**: comprometido ANTES de correr el sweep que gobierna (misma
disciplina que `docs/gemma4-e4b-solo-baseline-design.md`: medición sin
cláusula de adopción ni de iteración — ningún cambio de harness depende
del resultado, así que el commit git auto-alojado se considera
suficiente y se disclosa como tal).

**Motivación**: las cuatro rondas de review pre-submission (R1 2026-07-13,
R2 y blind 2026-07-17, tex-review 2026-07-18) convergieron en el mismo
issue #1: toda la evidencia del paper vive en una suite de 19
micro-tareas escrita por el mismo autor del harness, sin anclaje en un
benchmark externo, con saturación en el techo y amenaza de circularidad
(la suite fue el target de regresión del desarrollo). Este diseño ancla
los resultados centrales en un subset del Berkeley Function Calling
Leaderboard v4 — tareas que NADIE de este proyecto escribió ni usó
durante el desarrollo del harness.

## Materiales

- **Datos**: BFCL v4 (repo `ShishirPatil/gorilla`, `bfcl_eval/data/`,
  `main` al 2026-07-18), categorías `simple_python` (399 entradas),
  `multiple` (199), `irrelevance` (239). Mapeo a los skills del suite
  default: simple↔single_tool, multiple↔distractor_selection,
  irrelevance↔no_tool. **Sin contraparte BFCL**: multi_step y
  error_recovery — el ancla cubre el eje selección/formato de
  tool-calling, no el de recuperación; esto se declara como límite, no
  se esconde.
- **Muestreo**: determinista, `random.Random(42).sample(entries, 20)`
  por categoría → 60 tareas. Subset completo + ground truths commiteados
  en `docs/bfcl-anchor-data-2026-07-18.json`; conversor en
  `docs/bfcl-anchor-convert-2026-07-18.py`.
- **Adaptación** (`crates/braze-bench/suites/bfcl-anchor.toml`):
  - Tools sintéticas objetivo por tarea (`synthetic_tools`, extensión
    bench-side nueva en `crates/braze-bench/src/synthetic.rs` — NO toca
    el engine ni ninguna palanca bajo evaluación; el schema viaja como
    `parameters_json` para fidelidad byte a byte).
  - Tipos BFCL→JSON Schema: dict→object, float→number, tuple→array.
  - Nombres sanitizados `.`→`_` (BFCL hace lo mismo para modelos API),
    aplicado por igual a defs, `expect_tool_call` y ground truths.
  - Resultado enlatado (BFCL califica el AST de la llamada, no una
    ejecución): `{"status":"ok","note":"call recorded..."}`.

## Calificación en dos capas (pre-declarada)

1. **Online (braze-bench)**: identidad — `expect_tool_call` = función
   del ground truth (simple/multiple); `expect_no_tool_call`
   (irrelevance). Es la métrica que ordena brazos dentro del bench,
   comparable con las métricas del resto del paper.
2. **Offline (Python sobre `BRAZE_BENCH_KEEP_SESSIONS`)**: argumentos
   contra `possible_answer` con semántica AST de BFCL simplificada y
   documentada: nombre exacto; por cada parámetro del ground truth, el
   valor llamado debe pertenecer a su lista de valores admitidos
   (strings: igualdad exacta tras trim; números: igualdad numérica;
   `""` en la lista ⇒ parámetro omitible); sin parámetros inventados
   (todo arg llamado debe existir en el schema). El pass offline ≤
   online por construcción; el offline es el número con sentido frente
   al leaderboard (con el caveat de harness/serving distinto), el
   online es el consistente con el resto del paper.

## Brazos y presupuesto

5 brazos × 60 tareas × 5 repeticiones = **1.500 corridas** (Nitro, un
sweep a la vez, temp 0.2, timeout 180s, `--no-ollama-stop`,
`BRAZE_BENCH_KEEP_SESSIONS=1`):

1. `ollama:llama3.2:1b` (baseline 1B)
2. `ollama:llama3.2:1b+lead:ollama:gemma4:e4b` (composite del headline)
3. `ollama:gemma4:e4b` (lead solo — techo)
4. `ollama:qwen2.5:3b` (baseline 3B)
5. `ollama:qwen3.5-coder` (baseline ceiling)

Binario: worktree limpio en `fedbc3e` + esta rama (`bfcl-anchor`) — el
working tree principal tiene WIP de Paper 2 que NO debe entrar al
binario del sweep.

## Lecturas pre-declaradas (qué significaría cada patrón)

- **E1 — Ordenamiento de baselines**: si el orden 1B < 3B < coder del
  suite default se reproduce en el ancla (online), la amenaza de
  idiosincrasia de suite se debilita; si se invierte materialmente, es
  evidencia reportable de que la curva del paper es suite-específica.
  Sin umbral numérico: lectura direccional, se reporta lo que dé.
- **E2 — La palanca lead a 1B**: si el composite supera a su baseline
  1B con Newcombe fuera de cero, el valor de la palanca transfiere más
  allá de la suite autoral; si ≈0 o negativo, el paper debe acotar el
  claim del lever a su propia suite. (En el suite default: +70pp.)
- **E3 — Pinned ceiling**: esperamos composite ≈ gemma4:e4b solo
  (Newcombe cruzando cero), replicando externamente la lectura central
  del paper. **Si el composite supera claramente al solo, contradice la
  revisión central del paper y se reporta con la misma prominencia.**
- **E4 — Offline vs online**: el gap identidad→argumentos por
  modelo/escala es un resultado descriptivo nuevo (¿cuánto de "llamó la
  tool correcta" sobrevive a "con los argumentos correctos"?).

Ninguna de estas lecturas gatilla cambios de harness ni iteración; el
análisis usa la misma estadística del paper (Wilson, Newcombe
within-sweep — los 5 brazos corren en UN sweep multi-brazo —,
bootstrap por tarea para los deltas headline).

## Registro

Auto-alojado (commit de esta rama, anterior al sweep). Sin OSF: medición
sin cláusula de adopción, mismo criterio que los solo-baselines; se
reporta el mecanismo tal cual en el paper si el ancla entra al texto.
