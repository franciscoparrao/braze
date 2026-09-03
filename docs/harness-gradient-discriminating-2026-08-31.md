# Re-verificación del gradiente harness×escala: no se replica

Fecha: 2026-08-31 (corroboración externa añadida 2026-09-02)
Pre-registro: `docs/hypothesis-2026-08-30-harness-gradient-discriminating.md`
Antecedente: `docs/variance-decomposition-2026-08-30.md`, hallazgo 4
Datos: `docs/sweep-gradient-discriminating-ollama-qwen2.5-{3b,7b}-2026-08-30.json`
Estado: **CERRADO con un ejecutor faltante** (ver § El brazo que no se pudo medir).

## Qué se ponía a prueba

La descomposición sobre `default.toml` encontró que el rango de pass rate
movido por el harness **decrecía monótonamente** con la escala del
ejecutor (31.1 / 14.5 / 5.3 pp para 3b / 7b / coder) — la tesis del
Paper 1 cuantificada. Su propia limitación la ponía en duda: esa suite
está saturada para el coder (0.994 en base), y un techo comprime
mecánicamente el rango.

## Resultado

Suite discriminante v2, 34 tareas, 3 configuraciones, 2 repeticiones,
binario único (a diferencia del análisis original, que mezclaba 14
commits):

| ejecutor | base | +lead | +ablate:no-rescue | **rango** |
|---|---|---|---|---|
| qwen2.5:3b | 0.182 | 0.644 | 0.162 | **48.2 pp** |
| qwen2.5:7b | 0.076 | 0.755 | 0.091 | **67.9 pp** |

**El rango CRECE con la escala, en vez de decrecer. H1 rechazada.**

El orden se mantiene invertido corrigiendo la censura por el peor caso
(asumiendo que todo timeout habría fallado, denominador 68): 39.7 pp
para el 3b contra 51.2 pp para el 7b. No es un efecto de los datos
censurados.

Nota lateral: el 7b rinde **peor que el 3b** en base (0.076 vs 0.182),
al revés que en `default.toml`. Se auditó buscando un artefacto de
grading y no se encontró: el mecanismo es real y localizable — el 7b
emite llamadas con schema válido (25 fallos de schema contra 99 del 3b)
pero que fallan al ejecutarse el doble de veces (114 contra 55), y
termina sin producir los archivos que las aserciones piden.

## El brazo que no se pudo medir

`qwen3.5-coder` **no tiene datos válidos**: su sweep produjo 30 corridas
de las 204 esperadas y fue marcado
`INVALID-sweep-gradient-discriminating-ollama-qwen3.5-coder-2026-08-30-oom.json`.

Causa medida: OOM-kill del servicio Ollama a las 05:15:40 (`journalctl`).
El diseño ya había cambiado el lead de `gemma4:e4b` a `ornith:9b`
precisamente para evitarlo, y no alcanzó. El mecanismo exacto,
verificado después con `/api/ps`: los dos modelos piden 4.1 + 4.7 GB de
VRAM contra los 6 GB de la RTX 3050, así que uno cae **entero** a RAM
(6.4 GB en vez de 1.7) y el total roza los 9 GB disponibles de los 14
del equipo. El KV cache termina de empujarlo.

Atribución honesta: el oom-killer lo invocó un `ServiceWorker` de
escritorio (`oom_score_adj=300`), no el sweep — pero el sweep sostenía
la presión que lo hizo posible.

**Queda pendiente de hardware.** Con 32 GB (2×16 SODIMM DDR5-5600, que
es lo que el i5-13420H soporta a 5200) el brazo corre sin problema.

## Corroboración externa (2026-09-02)

`\citet{tang2026wikiskill}` — WikiSkill, Google Research, arXiv
2608.27454, 28-ago-2026 — reporta el mismo signo con un mecanismo
completamente distinto y a escalas mayores:

> "The benefits of skill evolution increase with model capability and
> complement model scaling": +12.3, +17.5 y +23.9 puntos para
> Qwen-3.5-4B, Qwen-3.5-9B y Qwen-3.6-27B.

Cinco benchmarks, cinco modelos, tres corridas independientes por
método. **El beneficio del andamiaje crece con la capacidad del
ejecutor**, que es exactamente lo que este sweep encontró y lo contrario
de lo que predecía el hallazgo 4.

Esto convierte un resultado que parecía un fallo de replicación
desconcertante en un resultado con respaldo independiente.

## Veredicto sobre el hallazgo 4

**Se retira.** El gradiente decreciente medido en `default.toml` era casi
con certeza artefacto de la saturación del coder, y ahora tiene
refutación por dos vías independientes: interna (este sweep, en una
suite sin techo, con binario único) y externa (WikiSkill, otro
mecanismo, otras escalas, otros benchmarks).

**No debe citarse en el Paper 1 en su forma actual.** Lo que sobrevive es
mucho más débil y hay que enunciarlo así: el harness mueve mucho el
rendimiento de los modelos pequeños en términos absolutos (48-68 pp es
enorme), pero **no hay evidencia de que lo mueva *más* cuanto más
pequeño es el modelo**; la evidencia disponible apunta en la dirección
contraria.

Queda en pie, y sin tocar, el hallazgo 2 de la descomposición —la
interacción harness×tarea domina a ambos efectos principales—, que es
independiente de esto y era el más robusto de los cuatro.

## Limitaciones

- **Dos ejecutores, no tres**: sin el extremo superior no se puede
  decir si el rango sigue creciendo o se da vuelta más arriba. WikiSkill
  sugiere que sigue creciendo hasta 27B, pero con otro mecanismo.
- **Censura no aleatoria**: 9 timeouts en el brazo `+lead` del 3b y 15 en
  el del 7b, todos por el límite de 180 s que el lead hace más probable
  al añadir rondas. Sesga los pass rates del brazo lead hacia arriba.
- **Dos repeticiones**: la resolución la fijan los 34 ítems; las réplicas
  solo estiman ruido.
- **El lead no es el del análisis original** (`ornith:9b` en vez de
  `gemma4:e4b`), por la restricción de RAM. La comparación con
  `default.toml` es conceptual, no directa.
- Los dos ejecutores son de la misma familia (Qwen 2.5).
