# A/B de search_tools: la ganancia es costo, no pass rate (a 200 tools de ruido)

Fecha: 2026-07-11
Contexto: la palanca C′.1 (`crates/braze-engine/src/tool_search.rs`)
contra su suite (`suites/tool-search.toml`: 6 tareas × 200 tools de
ruido sintético — ninguna tarea necesita una tool de ruido). 2 brazos ×
{qwen2.5:3b, qwen2.5:7b} × 5 reps = 120 corridas, binario `bb07363`,
cero fallos de red.
Estado: **CERRADO.** Datos: `docs/sweep-search-tools-ab-2026-07-11.json`/`.log`.

## Resultados

| Executor / brazo | Pass rate | tokens input prom/corrida | wall prom |
|---|---|---|---|
| 3b, deferral ON (default) | 23/30 (77%) [59,88] | **2.551** | **2.3s** |
| 3b, deferral OFF (206 listadas) | 25/30 (83%) [66,93] | 14.273 | 5.2s |
| 7b, deferral ON | 30/30 (100%) | **2.545** | **4.2s** |
| 7b, deferral OFF | 30/30 (100%) | 15.283 | 10.6s |

(`noisy_multi_step` 0/5 en ambos brazos de 3b — debilidad conocida del
modelo, idéntica con y sin deferral; no discrimina la palanca.)

## Hallazgos

1. **La hipótesis de distracción NO se confirmó a esta escala de ruido.**
   Con 206 tools listadas, ni el 3b ni el 7b pierden pass rate (el 3b
   incluso 83% vs 77%, dentro del ruido con n=30). qwen2.5 no se
   distrae con 200 tools *inertes e irrelevantes* — un contraste
   interesante con su debilidad medida en `distractor_selection`, donde
   los distractores son *plausibles*. La cantidad de opciones no es el
   problema; la plausibilidad sí.

2. **La ganancia real y grande es costo: 5.6× menos tokens de prompt y
   ~2.3-2.5× menos latencia, a costo de pass rate cero (7b) o dentro del
   ruido (3b).** 2.5K vs 14-15K tokens de input por corrida — visible
   también en vivo en el `PromptBudgetAuditHook` (tools_tokens_est ~940
   vs ~3.900 por request). En un backend cloud eso es 5.6× de factura de
   input; en local, la mitad del wall.

3. **A la escala del caso real, la deferral no es optimización sino
   habilitación.** 200 tools de ruido caben (a duras penas) en el
   `num_ctx=8192` local; el gateway objetivo (1.500+ tools) proyecta
   ~30K tokens solo de inventario — no cabe. El A/B no puede medir ese
   régimen (el brazo OFF no correría); lo que sí establece es que
   ocultar el catálogo no cuesta correctness en el régimen donde ambos
   brazos corren.

4. **Veredicto para el default**: el umbral 40 queda como está. La
   palanca es segura (sin costo de correctness medible), paga en
   costo/latencia desde ya, y es la única opción viable a escala
   gateway. La suite y la ablation quedan permanentes para re-medir si
   el mecanismo cambia (p.ej. si el search pasara a BM25).

## Limitaciones

- n=30 por brazo — detecta efectos grandes, no matices de ±6pp.
- Las tareas nunca REQUIEREN activar una tool oculta (por diseño de la
  suite: el ruido nunca es la respuesta). El flujo búsqueda→activación→
  invocación está cubierto por el test e2e de `engine.rs`, no por este
  A/B — una suite futura podría medir la calidad del ranking léxico
  cuando la respuesta SÍ está en el catálogo oculto (el régimen MCP
  real).
- Ruido inerte y homogéneo; distractores plausibles (el hallazgo 1
  sugiere que importan más) quedan para la suite de exploración si se
  corre (docs/explorador-aislado-ab-design.md).

## Cómo reproducir

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 \
cargo run -p braze-bench -- crates/braze-bench/suites/tool-search.toml \
  --backends "ollama:qwen2.5:3b,ollama:qwen2.5:3b+ablate:tool-search-threshold=1000000,ollama:qwen2.5:7b,ollama:qwen2.5:7b+ablate:tool-search-threshold=1000000" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-search-tools-ab-<fecha>.json
```
