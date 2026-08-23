# Hipótesis: A/B de cuantización de pesos en gpt-oss:20b (MXFP4 nativo vs Q3_K_M)

Fecha: 2026-08-22
Estado: proposed — commiteado ANTES de bajar el segundo GGUF o correr
nada (registro git-only; ver § Registro y su caveat).
Línea: viabilidad del executor principal en hardware propio; hermano
del A/B de KV-quant (`docs/hypothesis-2026-08-16-kv-quant-ab.md`), que
mide el otro término de la misma economía.

## Origen

Pregunta del autor tras el bench de FreeToken (2026-08-22): si mxfp4
es el formato **más lento en el camino CPU** de los cinco medidos
(14,3 GB/s contra 54,2 de bf16, 3,8×), ¿puede gpt-oss:20b correr con
una cuantización más eficiente? La pregunta tiene dos filos —
velocidad y **si cabe** — y ambos importan en Nitro.

## Qué se sabe y qué no (antes de medir)

- El bench de FreeToken midió **sus** kernels MoE, no los de
  llama.cpp. **No transfiere directo**; es la motivación, no la
  evidencia.
- MXFP4 **no es una cuantización aplicada a posteriori**: es el
  formato nativo con que OpenAI entrenó los expertos de gpt-oss
  (quantization-aware). Los quants de la comunidad (Q3_K_M, Q4_K_M…)
  son **re-cuantizaciones de una cuantización**, con pérdida
  potencialmente acumulada. El riesgo de calidad no es simétrico con
  el caso habitual FP16→Q4, y por eso hay que medirlo en vez de
  suponerlo en cualquier dirección.
- Smoke del 2026-08-22 (nodo headless, sin OOM): MXFP4 con
  `reasoning=medium` **no completa** una tarea de la suite en 900 s
  (14 rondas, ~64 s/ronda, timeout). Con `reasoning=low` la misma
  tarea pasó en 567 s (smoke del 18-ago). La lentitud **no es
  memoria**: con 13 GB libres se comporta igual.

## Pregunta

¿`Q3_K_M` cambia el pass rate y/o el tiempo por tarea de gpt-oss:20b
respecto de su formato nativo `MXFP4`, en nuestra suite y nuestro
harness?

## Diseño

| | |
|---|---|
| Suite | `discriminating.toml` (34 tareas, oráculo `cargo check`) |
| Motor | **LocalBackend/Harmony** en Nitro (donde vive el baseline 57/57) |
| Brazo A | `gpt-oss-20b-MXFP4.gguf` (12 GB, nativo, ya en `~/models`) |
| Brazo B | `gpt-oss-20b-Q3_K_M.gguf` (10,7 GB, unsloth — a descargar) |
| Brazo E | **A/A**: MXFP4 repetido (piso de ruido in-sweep) |
| Seeds | 42, 43, 44 — una invocación por (brazo, seed) |
| Orden | **round-robin**: A-42, B-42, E-42, A-43, … (lección del incidente KV-quant: brazo-por-brazo confunde deriva con tratamiento) |
| Total | 3 brazos × 3 seeds × 34 = **306 corridas** |
| Env | `BRAZE_LOCAL_REASONING=low` en **los tres brazos**, `BRAZE_MAX_TOKENS=12288`, `BRAZE_OLLAMA_NUM_CTX=32768`, timeout 900 s |
| Precondición | Nodo **headless** verificado (sesión gráfica detenida) y `free -h` con ≥12 GB disponibles ANTES de lanzar |

**Costo declarado**: a ~570 s/tarea, cada invocación son ~5,4 h → el
sweep completo ≈ **48 h**. Si el tiempo apremia, el recorte permitido
—y declarado aquí, no decidido después— es **bajar a 2 seeds**
(≈32 h), nunca eliminar el brazo A/A.

**`reasoning=low` es entorno, no tratamiento**: idéntico en los tres
brazos, y obligatorio porque a `medium` el instrumento no mide
(timeout-floor verificado). Consecuencia declarada: los resultados
**no son comparables** con el 57/57 histórico de `default.toml`, que
corrió a otro reasoning y otra suite. El A/B es autocontenido.

## Hipótesis y priors honestos

- **H1 (velocidad)**: Q3_K_M reduce el tiempo por tarea respecto de
  MXFP4. *Prior: probable pero de magnitud incierta* — el dato que la
  motiva es de otros kernels; los quants-K tienen kernels AVX maduros
  en llama.cpp, pero gpt-oss es MoE y su camino puede no comportarse
  como un denso.
- **H2 (calidad)**: el pass rate de Q3_K_M no cae más allá del piso
  A/A. *Prior: incierto, con riesgo real a la baja* — Q3 es agresivo y
  re-cuantiza un formato ya cuantizado. Un daño medible sería el
  resultado esperable, no una sorpresa.
- **H3 (huella)**: Q3_K_M reduce la memoria pico y el margen de OOM.
  *Prior: casi seguro* (10,7 vs 12 GB), pero se mide igual porque el
  KV domina el pico.

## Métricas

Primaria: pass rate dual (`passed` y `passed_strict`), McNemar exacto
pareado por (tarea, seed) para B−A, contrastado contra el piso A/E;
tests a nivel tarea (sign/Wilcoxon sobre los 34 conteos). Secundarias:
**wall time por tarea** (el endpoint de H1), rondas, tokens,
`schema_validation_failures`, `rescued_tool_calls`, pass^3, y memoria
pico observada. MDE declarado al medir el piso A/E, antes de leer B.

## Criterios de decisión, pre-registrados

1. **Piso primero**: la discordancia A/E define el piso y el MDE.
   Ningún contraste B−A se interpreta por debajo de él.
2. **Adoptar Q3_K_M como default local** si: el pass rate no cae fuera
   del piso **y** el tiempo por tarea baja de forma consistente en las
   tres seeds. Se documenta como cambio de configuración con
   baselines nuevos (no se re-etiquetan los históricos).
3. **Rechazar** si el pass rate cae fuera del piso: se documenta el
   precio en calidad del quant agresivo — resultado útil, porque la
   comunidad usa estos quants sin medirlos con oráculo objetivo.
4. **Nulo en ambas dimensiones** (ni más rápido ni peor): MXFP4 se
   queda por ser el formato nativo, y se reporta que la elección de
   quant no mueve la aguja en este régimen — que contradiría la
   motivación del bench de FreeToken y es publicable como matiz.
5. **Sin iteración de tratamiento.** Fallos de infraestructura fuera
   del denominador; >10% invalida el sweep (repetir una vez, completo).
6. **Gate anti-copias (L-9)**: si las corridas de un mismo brazo
   resultan idénticas entre seeds, aplica la cláusula de instrumento
   (`BRAZE_LOCAL_TEMP>0`) antes de leer ningún contraste.

## Riesgos anotados

- **Q3_K_M podría no cargar** con la plantilla Harmony o tener
  metadata distinta; el smoke lo caza. Si no carga, se reporta como
  no-ejecutable y **no se sustituye** por otro quant (eso sería
  elegir el tratamiento a la vista del instrumento).
- El pico de memoria lo domina el KV, no los pesos: la ganancia de
  1,3 GB podría no traducirse en margen real. Se mide (H3).
- Un timeout de 900 s con tareas de ~570 s deja poco aire: si un brazo
  sufre más timeouts que otro, **eso es parte del resultado** (H1 en
  su forma extrema), y se reporta con la tasa por brazo, no se corrige
  subiendo el tope a mitad de camino.

## Registro y su caveat (lección del 2026-08-22)

Este documento se commitea y **pushea al repositorio público antes de
descargar el segundo GGUF y antes de lanzar**, de modo que el orden
sea verificable por terceros:
`git log --diff-filter=A --format='%ad' -- <este archivo>` debe ser
anterior a la fecha de los JSON de resultados. Es la práctica que la
auditoría del Paper 2 mostró que **no** se cumplió para el piloto M1,
donde registro y datos entraron en un mismo commit posterior.
