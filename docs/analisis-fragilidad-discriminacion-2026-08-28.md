# Fragilidad vs discriminación por ítem: la réplica no concluye, la banda sí

Fecha: 2026-08-28
Datos: 16 sweeps de `discriminating.toml` ya existentes — **no se corrió
nada nuevo**.
Script: `scripts/fragility_vs_discrimination.py`
Origen: lectura de Parupudi, *There Is No Neutral Harness*
(arXiv:2608.21382), § 4.4.

## La pregunta

Parupudi mide, sobre 3.679 ítems de opción múltiple y 26 configuraciones
de harness, que la **discriminación** de un ítem correlaciona con su
**fragilidad** (r = +0,28, IC95% [0,25, 0,30]), y concluye que comprimir
un benchmark por discriminación *conserva* los ítems que cargan la
sensibilidad al harness: *"Compression does not remove harness
sensitivity. It keeps the items that carry it."*

Si eso transfiere al régimen agéntico, tiene una consecuencia incómoda y
concreta acá. `discriminating.toml` se construyó eligiendo tareas **cerca
de la frontera del modelo** —su propio comentario de cabecera lo dice—,
y eso *es* discriminación. El piso de ruido de 17,6 pp medido en el A/B
de weight-quant no sería mala suerte del instrumento: sería consecuencia
directa del criterio con que se eligieron los ítems.

Esta nota mide eso con datos que ya existían.

## Diseño

Dos diseños, porque miden fenómenos distintos y confundirlos fue el
primer error de esta nota:

| | fragilidad desde | discriminación desde |
|---|---|---|
| **1. entre réplicas** | `wq-A`×3 + `wq-E`×2 (MXFP4, config idéntica) | `kv-quant`×9 menos `wq-B`×2 |
| **2. entre configuraciones** | `kv-quant`: f16a / q8_0 / q4_0 (3 seeds c/u) | `wq-{A,E}`×5 menos `wq-B`×2 |

El diseño 2 es el análogo directo del suyo. El 1 mide la varianza que
Parupudi **excluye por diseño** (su decoding es greedy y de semilla
única) y que es justamente la que este proyecto mide con sus brazos A/A.

En ambos, fragilidad y discriminación salen de conjuntos de corridas
**disjuntos**: compartirlas induciría correlación espuria. Las 34 tareas
tienen dato en los tres conjuntos y los 16 sweeps comparten semántica de
grading (`functional-primary+strict-secondary/2026-08-12`).

## Resultado principal: indeterminado, no nulo

| diseño | Spearman ρ | IC95% bootstrap (10.000, sobre ítems) |
|---|---|---|
| 1 — entre réplicas | +0,065 | [−0,285, +0,422] |
| 2 — entre configuraciones | **+0,134** | **[−0,191, +0,428]** |
| Parupudi (MCQ, n = 3.679) | +0,28 | [0,25, 0,30] |

El punto estimado del diseño 2 va en la dirección de Parupudi, y su
segundo estadístico también: el cuartil más discriminativo tiene spread
0,500 contra 0,449 del resto (brecha +0,051; la suya es 0,96 contra
0,85).

**Pero el intervalo contiene tanto el cero como su +0,28.** Con 34 ítems
no se puede distinguir "no hay efecto" de "exactamente el efecto que él
midió". Reportarlo como nulo sería sobrevender un resultado que el n no
sostiene.

Consecuencia para la hipótesis que motivó el análisis: **ni sostenida ni
descartada**. Que el piso de ruido de la suite sea consecuencia de
haberla optimizado por discriminación sigue siendo una conjetura
razonable y sin evidencia propia. Responderla exige más ítems, y la
suite tiene el tamaño que tiene por una razón deliberada
(`docs/noise-floor-2026-07-26.md`).

## Lo que sí quedó medido, y pesa más que la pregunta original

### El 53% de los ítems voltea entre corridas idénticas

Cinco corridas de MXFP4 con la misma configuración:

```
robust-correct (5/5)   16/34   47%
fragile                18/34   53%
robust-wrong   (0/5)    0/34    0%
```

**Ninguna tarea es imposible.** Las 34 se resolvieron al menos una vez.
La suite no tiene ítems fuera de la frontera del modelo — tiene ítems
que el modelo acierta *a veces*.

### La banda

```
robust      0,471
media       0,782      ← lo que reporta un sweep
optimista   1,000
amplitud    52,9 pp
```

**Fracción run-lucky = 0,40**: el 40% de lo que se le acredita al modelo
bajo la métrica habitual no sobrevive a repetir la corrida. Es el
análogo agéntico de su 85% *config-lucky*, y viene de la fuente que él
excluye explícitamente.

Dicho de otro modo: un sweep que reporta 78% está reportando un número
que solo el 47% de las veces es reproducible ítem por ítem, con
configuración fija y sin tocar nada.

### El KV cache mueve el 82% de los ítems

Spread medio 0,461 entre f16a / q8_0 / q4_0. Un parámetro de motor que
ningún reporte de benchmark menciona mueve cuatro de cada cinco tareas,
casi medio punto de tasa de acierto. Es evidencia directa de la clase de
variable que `engine_version` (commit `1bcc21a`) existe para registrar.

## Limitaciones, declaradas

- **n = 34.** Es el límite duro de todo lo anterior y la razón de que la
  correlación no concluya.
- **La discriminación se estima con 2 corridas del brazo débil**, así que
  por ítem toma valores en {0; 0,5; 1}. Muy granular, y produce
  discriminaciones negativas (hasta −0,39) que casi seguro son ruido y no
  un ítem donde Q3_K_M sea mejor.
- **Deriva potencial entre conjuntos**: los `kv-quant` son del 18-ago y
  el weight-quant del 23-24. Ninguno registra `engine_version` (es
  posterior), así que no se puede descartar que el motor cambiara.
- **Un solo modelo.** Todo esto es gpt-oss:20b; nada dice que la
  estructura se repita en otro executor.

## Reproducibilidad

`python3 scripts/fragility_vs_discrimination.py <dir>`, determinista bajo
la semilla fijada en el script.

**Los 16 JSON NO están versionados en el repo** — viven en
`nitro:~/braze/docs/`. Sin ellos el script no corre, y esa es la misma
decisión diferida que el resto de los sweeps untracked de `docs/`. Si
esta nota va a citarse desde el Paper 3, versionarlos deja de ser
opcional.

## Qué hacer con esto

1. **Para el Paper 3**: el resultado publicable no es la correlación
   —que no concluye— sino la banda. Que una suite reporte 78% cuando
   solo el 47% es reproducible ítem por ítem es el hueco que Parupudi
   deja abierto al declarar que no testea tareas agénticas y que excluye
   la varianza de muestreo. Es el dato propio en el dominio que él dice
   no cubrir.
2. **Para el reporte de sweeps**: adoptar la partición y la banda como
   salida estándar de `braze-bench` (robust / fragile / robust-wrong más
   robust→optimistic), en vez del pass rate solo. Es barato: se calcula
   sobre las repeticiones que ya se corren.
3. **NO** re-derivar la conjetura de la discriminación sin más ítems.
   Con este n no se puede.

## Referencias

- Parupudi, V. S. R. *There Is No Neutral Harness: Modern LLM
  Leaderboards Are Manufactured by Config-Fragile Items*.
  arXiv:2608.21382. Copia en `docs/2608.21382v1.pdf` (ignorado por git).
- `docs/noise-floor-2026-07-26.md` — por qué la suite tiene 34 ítems.
- `docs/hypothesis-2026-08-22-weight-quant-ab.md` — el A/B de donde salen
  `wq-{A,B,E}`.
- `docs/hypothesis-2026-08-16-kv-quant-ab.md` — el A/B de donde salen los
  `kv-quant`.
