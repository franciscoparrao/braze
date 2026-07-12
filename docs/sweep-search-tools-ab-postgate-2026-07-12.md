# Re-run del A/B de search_tools con el harness endurecido (gate J-9 + fixes v7)

Fecha: 2026-07-12
Contexto: re-run del A/B original (`docs/sweep-search-tools-ab-2026-07-11.md`,
binario `bb07363`) con el binario `57db13f`, que difiere en cuatro fixes de la
auditoría v7 relevantes para esta medición: **J-9** (gate: una tool diferida
nombrada directamente ya NO se despacha — el bypass que debilitaba el claim
del mecanismo), **J-7** (`expect_text_contains` evalúa solo el texto posterior
al último evento de tool — mata falsos PASS por narración), **J-17** (el
budget de contexto del brazo ON se calcula sobre los stubs visibles, no sobre
el catálogo de 206 — el brazo deferral ya no compacta prematuramente) y
**J-6** (warm-up por brazo). Mismos 4 brazos, suite, seeds, temp y reps que
el original: 120 corridas, Nitro, cero fallos de red.
Datos: `docs/sweep-search-tools-ab-postgate-2026-07-12.json`/`.log`
(con `RUST_LOG=braze_engine=info`).

## Resultados (original → re-run)

| Brazo | Pass rate | tok input prom | compactaciones |
|---|---|---|---|
| 3b ON (deferral) | 77% → **70%** [52,83] | 2.551 → 2.748 | ? → 0 |
| 3b OFF (206 listadas) | 83% → **83%** [66,93] | 14.273 → 14.261 | ? → 6 |
| 7b ON | 100% → **87%** [70,95] | 2.545 → 2.513 | ? → 0 |
| 7b OFF | 100% → **100%** [89,100] | 15.283 → 15.286 | ? → 10 |

## Hallazgos

1. **El mecanismo queda verificado ESTRICTAMENTE: el gate J-9 disparó CERO
   veces en 120 corridas** (grep de "blocked a direct call" sobre el log con
   tracing activo). Ningún pass del A/B original venía del bypass — el modelo
   nunca llamó una tool oculta por nombre, ni una vez. El claim de la Figura 3
   ("el modelo solo puede usar lo listado o lo buscado") es ahora literalmente
   cierto por construcción Y empíricamente irrelevante como confound
   retroactivo.

2. **La historia de costo replica exacta**: 5.2× (3b) / 6.1× (7b) menos
   tokens de input — los brazos OFF reproducen sus tokens al ±0.1%. La mitad
   central de la Figura 3 está firme.

3. **La mitad "sin costo de correctness" se mueve EN CONTRA del brazo ON con
   el harness endurecido**, concentrada en celdas específicas:
   - `7b ON noisy_multi_step`: 5/5 → **1/5** (assertion_text; OFF sigue 5/5).
   - `3b ON noisy_grep`: 4/5 → **1/5** (assertion_tool_call; OFF sigue 5/5).
   - `3b ON noisy_multi_step`: 0/5 → 2/5 (mejoró); `3b ON noisy_no_tool`:
     5/5 → 3/5.
   Los agregados (70% vs 83% en 3b; 87% vs 100% en 7b) tienen ICs
   solapados, pero la dirección ya no es la del original ("83% vs 77%, el
   ON incluso arriba" → ahora ON abajo en ambos modelos).

4. **La atribución entre J-7 y J-17 NO es separable con estos datos.** Dos
   mecanismos compiten: (a) J-7 removió falsos PASS por narración — los 5/5
   de `7b ON multi_step` originales se evaluaron sobre el turno completo, y
   el re-run exige el token en la respuesta final; (b) J-17 cambió el timing
   de compactación del brazo ON (0 compactaciones vs las que hubiera con el
   budget chico original), lo que cambia los contextos y por tanto el
   comportamiento muestreado. Con n=5 por celda no se distingue. Lo que SÍ
   se puede decir: los brazos OFF, cuyo harness no cambió en nada relevante,
   replican al detalle — el desplazamiento es real del brazo ON bajo el
   harness nuevo, no ruido de infraestructura.

5. **Implicación para la Figura 3**: el claim honesto pasa de "mismo pass
   rate, 5.6× menos tokens" a "**5-6× menos tokens; el costo de correctness
   es cero o pequeño-negativo según la celda, con ICs anchos a n=30**". Las
   opciones: (a) regenerar la figura desde este JSON con el claim suavizado;
   (b) mantener la figura original anotando este re-run como sensibilidad al
   endurecimiento del harness; (c) subir las repeticiones de esta suite
   (15 reps ≈ 1h de Nitro) para resolver las celdas divergentes antes de
   decidir. La (c) es la única que distingue señal de ruido en
   `noisy_multi_step`/`noisy_grep`.

## Cómo reproducir

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/tool-search.toml \
  --backends "ollama:qwen2.5:3b,ollama:qwen2.5:3b+ablate:tool-search-threshold=1000000,ollama:qwen2.5:7b,ollama:qwen2.5:7b+ablate:tool-search-threshold=1000000" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-search-tools-ab-postgate-<fecha>.json
```
