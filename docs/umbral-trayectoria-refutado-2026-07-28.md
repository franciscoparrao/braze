# El umbral de trayectoria NO replica — cierre de la línea

**Resultado negativo.** La afirmación *"la inestabilidad de una tarea sigue
al largo de su trayectoria"*, formulada el 2026-07-27 sobre gpt-oss:20b,
**no sobrevive a la replicación en un segundo modelo**. Era una descripción
de un modelo, no un fenómeno.

Este documento cierra el eje. Se escribe con el mismo detalle que si hubiera
salido positivo, porque el valor de haberlo medido es el mismo.

## El diseño

Mismo protocolo exacto que produjo el hallazgo original: `discriminating.toml`
(34 tareas), 3 réplicas idénticas, `--seed 42`, temperatura 0, tope 900s,
mismo binario, misma GPU. Única variable: el modelo.

Se aplicó la separación de **estable-por-fácil vs. estable-por-imposible**,
anotada antes de mirar los datos: un modelo débil produce muchas tareas que
siempre fallan, y esas son estables por saturación inferior, no por cortas.
Mezclarlas corrompería la relación que se quiere medir.

## El resultado

| Rondas | gpt-oss:20b | gemma4:e4b |
|---|---|---|
| 0-3 | **0%** inestables (0/2) | **44%** (4/9) — 57% entre alcanzables |
| 3-6 | **7%** (1/14) | **48%** (12/25) — 75% entre alcanzables |
| 6-10 | 71% (5/7) | *no alcanza* |
| 10+ | 55% (6/11) | *no alcanza* |

Pass rate: gpt-oss 26/27/27 de 34; e4b 13/11/15 de 34.

**A igual largo de trayectoria —de 0 a 6 rondas— gpt-oss es prácticamente
determinista y e4b es una moneda al aire.** La inestabilidad no es función
del largo: es función del modelo.

### La separación importó, pero al revés de lo previsto

En gpt-oss solo **1 de 34** tareas es siempre-falla: el confound era
despreciable y el hallazgo original no dependía de él. En e4b son **11 de
34**, y excluirlas **sube** la inestabilidad de 48% a 75% en vez de bajarla
— o sea que ignorar la separación habría *subestimado* el ruido de e4b, no
inflado el umbral.

### Y la comparación es más débil de lo que parece

**Las distribuciones no se solapan.** e4b nunca supera las 6 rondas porque
falla rápido (siempre-falla promedia 4.0 rondas; siempre-pasa, 2.6). No hay
tramo largo donde comparar los dos modelos, así que el contraste vive
enteramente en el tramo corto.

## Qué queda en pie

**Refutado como afirmación general**: la inestabilidad no sigue al largo de
trayectoria.

**Sigue siendo cierto y medido, acotado a su modelo**:

- En **gpt-oss:20b**, la inestabilidad se concentra en las tareas largas
  (0-7% bajo 6 rondas, 55-71% por encima). Eso es lo que justifica
  `fast-core.toml`, que sigue siendo un instrumento válido **para ese
  modelo**.
- En **gemma4:e4b**, la inestabilidad es ~50% en todas partes, y su ruido ya
  estaba medido antes por otra vía (`docs/noise-floor-2026-07-26.md`:
  discordancia máxima de 5 tareas sobre 57 en `default.toml`).

La regla operativa derivada —*"replicar solo las tareas largas"*— queda
**acotada a modelos con baja divergencia por paso**. Para un modelo como
e4b hay que replicar todo, o no medir.

## La hipótesis que explica ambos casos, y por qué NO se afirma

Si la divergencia por paso fuera propiedad del modelo, la inestabilidad
sería aproximadamente `1 − (1 − p)^L`, con `p` la probabilidad de divergir
en un paso y `L` el largo. Con `p` chico hace falta mucha `L` para acumular
un flip → **umbral aparente**. Con `p` grande satura de inmediato → **ruido
en todas partes**. Encaja con los dos modelos, y hay evidencia directa de
que `p` difiere: el 2026-07-26 se midió que los logits de e4b empatan dentro
de 0.05 (`"<eos>"=23.225 "<"=23.175 "The"=23.109`) mientras gpt-oss no vive
en ese filo.

**Pero es un modelo ajustado *post hoc* a dos puntos cuyas distribuciones de
`L` ni siquiera se solapan.** Afirmarlo sería reemplazar una
sobre-generalización por otra más elegante. Si alguien lo retoma, el test
sería medir `p` directamente (frecuencia de empates de logit bajo umbral, por
modelo) y ver si predice la inestabilidad observada — no volver a ajustar
curvas a pass rates.

## Cierre de la línea

Este era el segundo de dos candidatos a conocimiento nuevo que se
identificaron el 2026-07-27. Ambos cayeron:

1. **Interacciones entre palancas** → la trampa resultó ser nuestra, no del
   género (`docs/lever-interaction-external-check-2026-07-27.md`).
2. **Umbral de trayectoria** → no replica (este documento).

**Conclusión honesta**: esta línea produce buena ingeniería y buen método,
pero no hechos nuevos. El rendimiento científico del proyecto está en el
Paper 1 —ya hecho, con pre-registro, anclas externas y nulos que atemperan
su propio titular— más un eventual paper de **métodos**, que no exige
competir en cómputo y cuyos componentes ya están medidos y documentados:

- piso de ruido por modelo antes de interpretar cualquier A/B;
- las afirmaciones de **mecanismo** sobreviven al ruido, las de **tasa** no;
- toda aserción de presupuesto (timeout, max_rounds, max_tokens)
  **binariza** ruido continuo y lo amplifica;
- la tasa condicional sobre datos censurados es un estimador **sesgado**
  cuando la censura es informativa;
- y ahora: **el ruido es propiedad del modelo**, así que el presupuesto de
  réplicas hay que calibrarlo por modelo y no por intuición.

Ese último punto es, irónicamente, lo que sobrevive del hallazgo refutado —
y es más útil que el umbral que se buscaba.

## Nota de método

Los dos tests que cerraron los dos ejes costaron, juntos, **20 minutos de
lectura de código y ~3 horas de GPU**. Evitaron un factorial 2×2 estimado en
19 horas sobre un eje sin generalidad, y una línea de investigación sobre un
umbral que no existe. Es el mejor retorno por unidad de esfuerzo de toda la
semana, y ninguno de los dos produjo un resultado positivo.
