# La curva harness-vs-escala: 4 brazos × 4 escalas de executor

Fecha: 2026-07-10/11
Contexto: la tabla central del paper ("el harness compensa la escala del
modelo"). Los 4 brazos de la matriz (baseline / `+plan` / `+lead` /
`+plan+lead`, gemma4:e4b fijo como planner y lead) sobre 4 executors de
escala creciente: llama3.2:1b, qwen2.5:3b, qwen2.5:7b, qwen3.5-coder.
16 brazos × 19 tareas × 5 reps = **1.520 corridas** contra Nitro.
Estado: **CERRADO**. Datos: `docs/sweep-curva-multiescala-2026-07-10.qwen.json`
(12 brazos qwen) + `docs/sweep-curva-multiescala-2026-07-10.partial-1b.json`
(4 brazos llama3.2:1b — slice válido del intento interrumpido por la
caída de Nitro; sus 1.140 filas qwen muertas se excluyen del análisis).

Trazabilidad: ambos slices corrieron el MISMO binario (worktree congelado
en `e9b841e`, código idéntico a `e16143e`); el campo `braze_git_commit`
del metadata se lee al escribir el JSON, por eso el slice qwen reporta
`e4bcc8b` (el HEAD siguió avanzando durante las ~8h del sweep). Los
brazos `+plan` miden el planner VIEJO (prosa como assistant) — anterior
a la iteración `e8c7d3f`, deliberadamente: su A/B compara contra estos
números. Cero errores de red en las 1.520 filas analizadas.

## La tabla

Pass rate (n=95 por celda, IC 95% Wilson):

| Executor | baseline | +plan | +lead | +plan+lead |
|---|---|---|---|---|
| llama3.2:1b | 19% [12,28] | **0% [0,4]** | **89% [82,94]** | 61% [51,70] |
| qwen2.5:3b | 68% [59,77] | 49% [40,59] | 92% [84,96] | 88% [80,93] |
| qwen2.5:7b | 80% [71,87] | 82% [73,89] | 95% [88,98] | 95% [88,98] |
| qwen3.5-coder | **98% [93,99]** | **49% [40,59]** | 94% [87,97] | 92% [84,96] |

`error_recovery` — la skill discriminante (n=15):

| Executor | baseline | +plan | +lead | +plan+lead |
|---|---|---|---|---|
| llama3.2:1b | 0/15 | 0/15 | 14/15 | 5/15 |
| qwen2.5:3b | 2/15 | 3/15 | 15/15 | 14/15 |
| qwen2.5:7b | 0/15 | 12/15 | 15/15 | 15/15 |
| qwen3.5-coder | 15/15 | 4/15 | 15/15 | 14/15 |

Latencia promedio (s) por corrida:

| Executor | baseline | +plan | +lead | +plan+lead |
|---|---|---|---|---|
| llama3.2:1b | 3.6 | 6.5 | 12.8 | 9.4 |
| qwen2.5:3b | 2.3 | 12.8 | 12.1 | 17.3 |
| qwen2.5:7b | 5.1 | 27.1 | 16.2 | 19.9 |
| qwen3.5-coder | 23.7 | 40.7 | 19.8 | 23.9 |

## Hallazgos

1. **La curva existe y es monótona: la ganancia del lead decae con la
   escala hasta cruzar cero en el techo.** +70pp a 1b (19% → 89%),
   +24pp a 3b, +15pp a 7b, **-4pp** en qwen3.5-coder (98% → 94%, dentro
   del ruido pero con el signo esperado: un lead más débil que el
   executor no puede sino estorbar). Es exactamente la predicción de la
   tesis — el harness compensa escala, y deja de pagar cuando la escala
   ya está. El dato estrella: **un 1B con lead (89%) supera al 3B (68%),
   al 7B (80%) — a fracción del costo de inferencia del 7B.**

2. **El plan-en-prosa daña en casi toda la escala, catastróficamente en
   los DOS extremos — y por el mismo mecanismo.** 1b: 0/95 (ni
   `single_tool` sobrevive; 37 de 95 fallas son respuestas VACÍAS).
   3b: -19pp. 7b: neutro-positivo (+2pp; ver punto 4). qwen3.5-coder:
   **98% → 49%**, y de sus 48 fallas, 35 son respuestas vacías — el
   mismo artefacto de degeneración que la matriz diagnosticó a 3b,
   ahora demostrado en el mejor modelo local del proyecto. Un thinking
   model que recibe "su propio" plan como texto de assistant se queda
   mudo igual que un 1B. Esto convierte la iteración `e8c7d3f`
   (descarte de single-step + render user-role) de apuesta razonable en
   hipótesis directamente testeable contra el artefacto documentado, en
   ambos extremos de la escala.

3. **La composición hereda el gradiente: cuanto más chico el executor,
   menos rescata el lead el daño del planner.** 1b: 61% vs 89% del lead
   solo (el daño persiste, -28pp); 3b: 88% vs 92% (casi converge); 7b:
   95% = 95% (converge); coder: 92% vs 94%. El lead es un mecanismo de
   recuperación con capacidad finita — con un executor que degenera lo
   suficiente, ni la apertura proactiva lo repara.

4. **La excepción del 7b es informativa, no ruido**: es la única escala
   donde el plan ayuda (`error_recovery` 0/15 → 12/15 — sus fallas
   baseline son todas semánticas, `assertion_text`/`files`, y el plan
   las corrige). Sugiere una ventana de capacidad: suficiente para
   *seguir* un plan sin degenerar, insuficiente para no necesitarlo.
   Puntual, no cambia la recomendación global (a 7b el lead logra lo
   mismo, 95%, sin el riesgo del planner en el resto de las escalas).

5. **El detalle de latencia del techo**: en qwen3.5-coder, `+lead` es
   MÁS RÁPIDO que el baseline (19.8s vs 23.7s) — gemma4:e4b abriendo
   los primeros rounds es más barato que el thinking model pensándolos.
   Un lead liviano como acelerador de un executor pesado es una lectura
   no anticipada de la palanca; queda anotada, no medida a propósito.

6. **Replicaciones**: el baseline del 3b (68% acá, 71.6% matriz, 67.4%
   3-brazos, 70.5% A/B original — cuatro sweeps independientes en la
   banda 67-72%) y su brazo `+lead` (92% / 92.6% / 92.6%) están
   estables; `error_recovery` baseline sigue en el piso (0-3/15 en toda
   la serie qwen2.5).

## Para el paper

La curva completa la evidencia central del ángulo A: **una palanca de
harness de capacidad (lead proactivo) compensa hasta ~70pp de escala y
decae monótonamente hasta el techo, mientras una palanca de contexto
(plan-prosa) daña en casi toda la escala — con el mismo mecanismo de
degeneración en ambos extremos.** "El harness compensa la escala" queda
cuantificado; "no todo andamiaje ayuda" queda demostrado con n=1.520 en
un solo par de archivos reproducibles.

## Limitaciones

- Un solo planner/lead (gemma4:e4b) — la curva es sobre la escala del
  EXECUTOR; la del lead queda abierta (el A/B del 09 mostró
  qwen3.5-coder ≈ gemma4:e4b como leads a 2× de latencia).
- n=15 por celda de skill; los extremos son concluyentes, los matices
  (94 vs 95 agregado) no.
- El slice de 1b y el qwen corrieron con ~8h de separación y una caída
  de Nitro en medio — mismo binario y misma config, pero no la misma
  sesión térmica del servidor; las latencias entre slices se comparan
  como orden de magnitud.
- Los brazos `+plan` miden el planner pre-iteración (deliberado — son
  la referencia del A/B de `e8c7d3f`, pendiente).

## Cómo reproducir

```bash
L="ollama:gemma4:e4b"; ARMS=""
for E in llama3.2:1b qwen2.5:3b qwen2.5:7b qwen3.5-coder; do
  ARMS="$ARMS,ollama:$E,ollama:$E+plan:$L,ollama:$E+lead:$L,ollama:$E+plan:$L+lead:$L"
done
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/default.toml \
  --backends "${ARMS#,}" --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-curva-multiescala-<fecha>.json
```

## Próximos pasos

1. A/B del planner iterado (`e8c7d3f` + brazo `+ablate:task-list`) —
   ahora con dos referencias de daño (3b y coder) que el render nuevo
   debe rescatar, o remoción.
2. A/B de `search_tools` sobre `suites/tool-search.toml`.
3. Para el paper: figura de la curva (pass rate vs escala, una línea por
   brazo) — los datos de este doc son la fuente.
