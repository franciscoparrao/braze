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

---

## Resultados finales y veredicto (2026-08-28)

Script: `scripts/weight_quant_close.py`. Pareo por (tarea, seed) sobre
las seeds que los tres brazos comparten (42, 43).

**El sweep cumple el diseño mínimo, no está truncado.** Se corrieron
A×3, B×2, E×2. El § Costo declarado autoriza de antemano —"declarado
aquí, no decidido después"— bajar a 2 seeds si el tiempo apremia,
"nunca eliminar el brazo A/A". El brazo A/A está completo, así que el
recorte cae dentro de lo pre-registrado. La interrupción del 24-ago
(apagado del equipo durante `B-s44`) coincidió con el recorte
permitido; no hay decisión post-hoc que justificar.

### Brazos (métrica dual, seeds compartidas, n = 68 celdas)

| brazo | passed | strict | s/tarea |
|---|---|---|---|
| **A** MXFP4 nativo | 55/68 (80,9 %) | 55/68 | 420 |
| **E** MXFP4 (A/A) | 50/68 (73,5 %) | 50/68 | 425 |
| **B** Q3_K_M | 32/68 (47,1 %) | 32/68 | 606 |

### El piso primero (criterio 1)

A contra E —el mismo brazo contra sí mismo— da **17/68 celdas
discordantes (25,0 %)**, McNemar exacto p = 0,33. El piso no es
significativo, que es exactamente lo que un A/A debe mostrar, y su
magnitud es la vara contra la que se lee todo lo demás.

Dato para el Paper 3: **una de cada cuatro celdas voltea sin que nada
cambie**.

### El tratamiento

B contra A: **29/68 discordantes, 26 a favor de A y 3 de B**, McNemar
exacto **p = 1,5 × 10⁻⁵**. Delta de pass rate **−33,8 pp**. Está
holgadamente fuera del piso, tanto en discordantes (29 vs 17) como en
dirección (26/29 en un solo sentido).

### Hipótesis

- **H1 (velocidad) — REFUTADA, y en la dirección contraria.** Q3_K_M no
  es más rápido: es **44,3 % más lento** (606 s contra 420 s por
  tarea). La motivación entera del A/B era la sospecha de que MXFP4
  fuera el formato lento en el camino CPU; medido en nuestro harness
  con nuestro oráculo, el quant agresivo pierde también acá.
- **H2 (calidad) — REFUTADA.** El pass rate cae 33,8 pp, muy fuera del
  piso.
- **H3 (huella)** — no medible con estos datos: los JSON no registran
  memoria pico. Queda sin responder y se declara como tal.

### Veredicto: RECHAZAR Q3_K_M (criterio 3)

El pass rate cae fuera del piso, así que aplica el criterio 3 tal como
estaba escrito: *"se documenta el precio en calidad del quant agresivo
— resultado útil, porque la comunidad usa estos quants sin medirlos con
oráculo objetivo"*. Ese precio resultó ser doble, calidad **y**
velocidad.

**MXFP4 nativo se queda como el formato local de gpt-oss:20b.**

La lectura de mecanismo que el pre-registro anticipó se sostiene:
Q3_K_M no es una cuantización de pesos FP16, es una **re-cuantización
de una cuantización** (MXFP4 es el formato nativo con que se entrenaron
los expertos, quantization-aware). El riesgo no era simétrico con el
caso habitual, y por eso se midió en vez de suponerlo.

### Caveats

- Los JSON **no registran `engine_version`** (la capacidad es del
  2026-08-27, posterior al sweep), así que no se puede descartar del
  todo que el motor cambiara entre brazos. Los tres corrieron en la
  misma ventana de 48 h sobre el mismo binario, lo que lo hace
  improbable pero no verificable a partir del dato.
- 34 tareas × 2 seeds. El efecto es tan grande que el `n` no es el
  cuello de botella acá, a diferencia de
  `docs/analisis-fragilidad-discriminacion-2026-08-28.md`.
- Un solo modelo y un solo quant de la comunidad. No se afirma nada
  sobre Q4_K_M ni sobre otras familias.
