# Piso de ruido de `braze-bench`: cuánto difieren dos corridas idénticas

**El número que le faltaba al proyecto.** Sin él, ningún A/B sobre modelos
chicos se puede interpretar: no hay forma de saber si una diferencia de N
tareas es un efecto o es la máquina.

## Método

13 sweeps de `default.toml` con configuración **idéntica** (misma suite, 3
repeticiones, `--seed 42`, timeout 180s, mismo binario, misma GPU), variando
solo el modelo y el régimen de sampling.

La clave del diseño: el seed del bench es `base + repetición`, o sea **idéntico
entre corridas** para cada repetición. Así que dentro de una corrida las
repeticiones difieren, pero **entre corridas la única fuente de variación es el
no-determinismo de punto flotante de la GPU**. Cualquier diferencia observada es
ruido por construcción.

## Resultados

| | pass rate | discordancia entre pares | tareas inestables | walltime medio |
|---|---|---|---|---|
| **gemma4:e4b — greedy** (5 corridas) | 53-55 / 57 (sd 0.89) | mediana 2, **máx 5** | 5 de 57 | 7.6-12.1s |
| **gemma4:e4b — temp 0.2** (5 corridas) | 53-57 / 57 (sd 1.50) | mediana 4, **máx 5** | 7 de 57 | 7.5-10.2s |
| **gpt-oss:20b — greedy** (3 corridas) | 57-57 / 57 (sd **0.00**) | **0** en los 3 pares | **0** | 11.5-11.7s |

*Discordancia* = cuántos resultados `(tarea, repetición)` se dan vuelta entre
dos corridas idénticas. Es la métrica operativa.

## Reglas operativas

- **gemma4:e4b — una diferencia de ≤5 tareas entre dos brazos es
  indistinguible del ruido.** Con temperatura el ruido rutinario se duplica
  (mediana 2 → 4) aunque el máximo no cambie.
- **gpt-oss:20b es determinista en este suite**: 0 discordantes, sd 0.00.
  Cualquier diferencia observada es real.
- **El walltime es MUCHO más ruidoso que el pass rate en e4b**: ±30% entre
  corridas idénticas (7.6-12.1s de promedio). En gpt-oss, ±1%. Una comparación
  de velocidad en e4b necesita superar el 30% para significar algo.

## Tres hallazgos estructurales

**1. La inestabilidad está concentrada, no dispersa.** Las tareas que oscilan
son casi todas de `error_recovery` — donde una tool call falla y hay que
recuperarse, o sea las más ramificadas. **52 de 57 son roca.** Consecuencia
incómoda: la superficie realmente medible del suite es chica. Una palanca que
no toque `error_recovery` es casi invisible para `default.toml` en e4b.

**2. El ruido es propiedad del modelo, no del harness.** Mismo suite, mismo
binario, misma GPU: e4b oscila y gpt-oss no. Los logits de e4b empatan al nivel
del ruido de punto flotante (medido el mismo día: `"<eos>"=23.225 "<"=23.175
"The"=23.109`) y la GPU desempata distinto en cada corrida. gpt-oss no vive en
ese filo.

**3. Las afirmaciones de mecanismo sobreviven a este ruido; las de pass rate
no.** Es la distinción metodológica más útil que salió de esto. "Las rondas de
cero tokens pasaron de 5 a 0" es verificable directamente y no depende de
estadística. "El pass rate subió 3 puntos" es indistinguible del ruido. Diseñar
las palancas para que tengan una afirmación de mecanismo comprobable vale más
que perseguir puntos de pass rate.

## Qué implica retroactivamente

Aplicando el umbral a lo medido el mismo día:

| Afirmación previa | Veredicto |
|---|---|
| DRY mejora e4b (51 → 53) | **MUERTA.** Diferencia de 2, muy debajo de 5. Ruido. |
| La temperatura daña (51 → 43) | **SEÑAL.** 8 tareas, por encima del umbral. |
| La guarda de EOG sube el pass rate (~+3) | **Dentro del ruido.** Pero su afirmación de mecanismo (rondas de 0 tokens: 5 → 0) se sostiene. |
| gpt-oss 3.4× más rápido (41.4 → 12.1s) | **SÓLIDA.** Ruido de walltime en gpt-oss: ±1%. |
| KV placement medido: 1.95× (29.2 → 15.0s) | **Se sostiene** (muy por encima del ±30%), pero los márgenes son más finos de lo presentado: el 20.7 vs 15.0 (38%) apenas supera el ruido. |

## Nota de procedimiento: el primer intento se perdió entero

Los primeros 13 sweeps produjeron **JSONs vacíos** y nadie se enteró hasta el
análisis. Dos causas, ambas instructivas:

1. **Error de copia**: el script exportaba `BRAZE_LOCAL_FAMILY=harmony`,
   arrastrado de los scripts de roam (que son para gpt-oss). Sobre gemma4:e4b
   eso fuerza una plantilla cuyo `<|start|>` no es token único en ese
   vocabulario, y el backend se niega — correctamente.
2. **El bench omitió el backend y salió con éxito**, escribiendo un JSON vacío.
   Es el pendiente ya anotado ("fail-fast de brazo en el bench: 57 fallos
   instantáneos de carga no deben quemar un brazo en silencio"), ahora con una
   segunda ocurrencia y un costo concreto.

Agravado por una decisión mía: el script redirigía la salida a `/dev/null`, así
que un fracaso total se veía igual que un éxito. **Un script de experimento no
atendido debe verificar que produjo datos y abortar si no** — el relanzamiento
lo hace.

---

## Adenda (2026-07-27): la inestabilidad sigue al LARGO DE TRAYECTORIA, y con umbral

> ⚠️ **REFUTADO el 2026-07-28.** Lo que sigue describe a **gpt-oss:20b**, no
> un fenómeno general: en gemma4:e4b la inestabilidad es ~50% incluso por
> debajo de las 6 rondas, así que **no es función del largo sino del
> modelo**. Se conserva porque el hallazgo acotado a gpt-oss sigue siendo
> válido y es lo que justifica `fast-core.toml` — pero la regla operativa
> ("replicar solo las tareas largas") solo vale para modelos con baja
> divergencia por paso. Ver `docs/umbral-trayectoria-refutado-2026-07-28.md`.

Medido sobre 3 réplicas idénticas de la suite discriminante v2 (34 tareas,
gpt-oss:20b, temp 0, tope 900s). **Costo adicional: cero** — sale de datos
que ya estaban en disco.

| Rondas de la tarea | Inestables |
|---|---|
| 0-3 | 0/2 (0%) |
| 3-6 | 1/14 (7%) |
| 6-10 | 5/7 (71%) |
| 10+ | 6/11 (55%) |

Y el contraste directo entre grupos:

| | n | rondas | segundos | tokens de salida |
|---|---|---|---|---|
| estables | 22 | 6.3 | 132 | 1002 |
| inestables | 12 | 11.4 | 264 | 1742 |

**No es que las tareas difíciles sean más ruidosas: son las LARGAS.** Casi
exactamente el doble en todas las dimensiones, y con un corte marcado
alrededor de las 6 rondas — por debajo son prácticamente deterministas, por
encima son monedas al aire. (La no-monotonía entre 71% y 55% es ruido a n=7
y n=11; el umbral sí se sostiene.)

**Mecanismo plausible, y explica por qué hay umbral y no pendiente**: cada
ronda es una oportunidad de que un empate de logits se resuelva distinto —
medido el mismo día, `"<eos>"=23.225 "<"=23.175 "The"=23.109`, dentro de
0.05 — y una trayectoria que diverge **no vuelve a converger**. Es
ramificación con estado absorbente: la probabilidad de haber divergido
alguna vez crece rápido con el número de rondas y luego satura.

### Qué implica para diseñar bancos agénticos

1. **Las tareas de más de ~6 rondas exigen repeticiones; las de menos, no.**
   ⚠️ **Acotado tras la refutación**: vale para modelos con baja divergencia
   por paso (gpt-oss). Para uno como gemma4:e4b, donde los logits empatan
   dentro de 0.05, hay que replicar TODO o no medir. **El presupuesto de
   réplicas se calibra por modelo, no por intuición ni por largo de tarea.**
2. **Reportar el largo de trayectoria junto al pass rate.** Dos bancos con
   el mismo pass rate y distinta distribución de rondas no tienen la misma
   varianza, y hoy nada en el reporte lo delata.
3. **Un banco puede partirse por costo/ruido**, que es lo que motivó
   `fast-core.toml`: 13 tareas cortas, ~15 min, 1 inestable — para trabajo
   rutinario. La suite completa queda para medir efectos que viven en
   trayectorias largas, donde hay que pagar repeticiones sí o sí.

### Lo que NO se afirma

Esto es un modelo (gpt-oss:20b), un hardware (RTX 3050 6GB) y una suite. El
umbral concreto de ~6 rondas es de este arreglo; lo que se propone como
general es la **forma** de la relación (inestabilidad creciente con el largo
de trayectoria, por divergencia absorbente), no el número.
