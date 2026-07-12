# search_tools a n=90: el "sin costo de correctness" no sobrevive — y el mecanismo no es el que parecía

Fecha: 2026-07-12
Contexto: tercera corrida del A/B de search_tools, con 15 repeticiones
(360 corridas) para resolver las celdas que el re-run post-fixes
(`docs/sweep-search-tools-ab-postgate-2026-07-12.md`) dejó ambiguas a n=5.
Binario `57db13f` (gate J-9, assert estricto J-7, budget justo J-17, warm-up
J-6), mismos brazos/suite/seeds base que las corridas anteriores (las reps
0-4 comparten seeds con ellas). Nitro, cero fallos de red.
Datos: `docs/sweep-search-tools-ab-n15-2026-07-12.json`/`.log`.

## Resultados agregados

| Brazo | Pass rate [IC 95%] | tok input prom |
|---|---|---|
| 3b ON (deferral) | 53/90 = **58.9%** [49,68] | 2.749 |
| 3b OFF (206 listadas) | 75/90 = **83.3%** [74,90] | 14.718 |
| 7b ON | 75/90 = **83.3%** [74,90] | 2.518 |
| 7b OFF | 87/90 = **96.7%** [91,99] | 15.286 |

**Los ICs ya no se solapan**: −24pp (3b) y −13pp (7b) para el brazo
deferral. El costo replica por tercera vez (5.4×/6.1× menos tokens). El
gate J-9 disparó **cero veces en 360 corridas** — tercera confirmación de
que nadie llama tools ocultas por nombre.

Por tarea (ON vs OFF): las celdas que separan son `noisy_no_tool` en el 3b
(5/15 vs 15/15), `noisy_grep` en el 3b (3/15 vs 15/15), `noisy_multi_step`
en el 7b (0/15 vs 12/15) y `single_tool` en el 3b (33/45 vs 45/45).
`noisy_distractor` es 15/15 en TODOS los brazos.

## El diagnóstico — sondas con sesiones preservadas

Antes de concluir "la deferral cuesta correctness", se sondearon las dos
celdas más raras con el runner en modo debug (sesiones preservadas,
transcripts reales):

**Sonda 1 — `noisy_no_tool` en el 3b ("¿cuánto es 7×8?", 0 tool calls,
1 ronda).** Tres condiciones, mismos seeds:

| Condición | tok input | Pass |
|---|---|---|
| Deferral ON (8 locales + stub de search) | 1.187 | **5/15** |
| Sin ruido y sin stub (8 locales) | 1.103 | **11/15** |
| OFF (8 locales + 206 de ruido) | 7.560 | **15/15** |

El transcript de una rep fallida: el modelo responde literalmente `"5.5"`.
El determinismo es exacto (las reps 0-4 del sweep replican patrón F-P-P-P-P
en la sonda aislada). Lecturas: (a) la tarea es marginal para qwen2.5:3b en
CUALQUIER condición limpia (11/15 ≈ 73%); (b) el stub de search le quita
~6/15 (su summary — "Search a catalog of 200 additional tools... raster
clip, send email" — es contenido semántico junto a una pregunta de
aritmética); (c) las 206 tools inertes, paradójicamente, la estabilizan a
15/15.

**Sonda 2 — `noisy_multi_step` en el 7b (leer 10 y 20, escribir la suma).**
Transcripts del brazo ON: en 4/5 reps el modelo escribe **`"10\n20"`** en
`suma.txt` — ni siquiera calcula la suma — y su texto final dice "contiene
los números 10 y 20" (sin "30": falla el assert de texto Y el de archivo,
coherentes entre sí). En el brazo OFF (12/15) sí computa 30. Mismos seeds.
No es J-7 endureciendo el assert, no es el harness, no es "no encuentra la
tool" (lee y escribe perfecto): **con el inventario chico el modelo computa
peor; con 206 stubs inertes de relleno, computa bien.**

## Hallazgos

1. **El claim "mismo pass rate" del A/B original NO sobrevive a n=90 con el
   harness endurecido.** Era un artefacto combinado de n=30 + el assert
   laxo pre-J-7 (que aceptaba narración intermedia como respuesta). El
   ahorro de tokens (5-6×) es sólido; la neutralidad de correctness no.

2. **Pero el mecanismo del costo NO es "findability"**: el modelo nunca
   necesitó ni intentó llamar tools ocultas (gate en cero, 360 corridas), y
   las tareas que caen no involucran búsqueda. Las sondas muestran
   **fragilidad composicional**: la conducta semántica de un modelo chico
   (¿computa 7×8? ¿suma 10+20?) depende de contenido del prompt que es
   semánticamente irrelevante para la tarea, en dirección impredecible —
   aquí, el prompt CORTO rinde peor que el largo, lo contrario de lo que
   cualquier teoría de distracción/dilución predice.

3. **Para la tesis del paper esto es mejor que el hallazgo original**: no
   "la deferral es gratis" sino "los efectos del harness sobre modelos
   chicos son no-monótonos y deben medirse, no asumirse". La palanca sigue
   siendo la única opción a escala gateway (30K tokens de inventario no
   caben en 8K de contexto), pero su costo a escala chica es real y su
   origen es la sensibilidad composicional del modelo, no el mecanismo de
   búsqueda. `noisy_distractor` 15/15 en todos los brazos refuerza que la
   plausibilidad, no la cantidad, es lo que distrae (hallazgo 1 del A/B
   original, que SÍ sobrevive).

4. **Implicación de configuración**: el umbral default (40) sigue bien para
   el caso gateway. Para inventarios que caben en contexto, este dato
   sugiere NO activar deferral por default en modelos ≤7B — coherente con
   que el default de config ya es conservador.

## Decisión pendiente (Figura 3)

Recomendación: **regenerar la Figura 3 desde este JSON** (n=90, ICs
disjuntos, binario con el harness endurecido) reencuadrando el caption: el
eje central pasa a ser el costo (5-6×) con el efecto de correctness
reportado honesto (−13/−24pp) y el diagnóstico de fragilidad composicional
como texto — las sondas de este doc son citables como mini-experimento.
Los dos sweeps anteriores quedan como análisis de sensibilidad.

## Cómo reproducir

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/tool-search.toml \
  --backends "ollama:qwen2.5:3b,ollama:qwen2.5:3b+ablate:tool-search-threshold=1000000,ollama:qwen2.5:7b,ollama:qwen2.5:7b+ablate:tool-search-threshold=1000000" \
  --repetitions 15 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-search-tools-ab-n15-<fecha>.json
```

Las sondas (tres condiciones de `noisy_no_tool`, transcripts de
`noisy_multi_step`) se corrieron con un parche temporal del runner que
preserva sesiones (`BRAZE_BENCH_KEEP_SESSIONS`, no commiteado) — si se
repiten seguido, promover el flag es un ítem S.
