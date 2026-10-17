# Outline v3 del Paper 3 — la tesis acotada tras RRSI y SAGE

Fecha: 2026-10-17
Estado: **reemplaza la premisa de `paper3-outline-v2-2026-08-28.md`**. La evidencia
del v2 (curva umbral vs FAR, pisos de ruido, falso positivo del Paper 2) sigue en
pie; la frase que lo sostenía, no.
Origen: `/paper-gaps` de la línea braze
(`~/vault/journals/_opportunities/2026-10-17_02-10_line-braze-harness-slm.md`).

## Lo que cambió

El v2 afirmaba: *"ninguno [de los gates] deriva ese umbral del ruido de su propia
configuración"*. Entre junio y septiembre aparecieron cuatro gates estadísticos para
harnesses que se auto-modifican, y uno de ellos la falsa:

- **RRSI** (Xia et al., Google, arXiv 2609.24972; v3 del 4-oct). Verificado contra el
  HTML completo el 2026-10-17. δ es *"the spread of repeated evaluations of the
  unchanged base harness"* (§3.3, App. D.1). En EngDesign lo toma del percentil 1 de
  una nula construida partiendo en dos las réplicas de la propia evaluación base. Los
  valores son 0,017, 0,004 y 0,020 (Tabla 5; k = 2, 2 y 4 trials por tarea). δ queda
  **fijo** (*"fixed before a run and never re-tuned"*, *"must not be tuned"*). El
  paper dice *"we run no sweep"* sobre δ y no reporta tasa de aceptación falsa.
- **SAGE** (Wang et al., arXiv 2609.36043, 28-sep). Verificado en HTML. Usa un test
  pareado de una cola. Ajusta α en held-out; α *"should match the task noise"*
  (§3.3), pero **no mide el ruido**. Deja como trabajo futuro *"an adaptive threshold
  that tightens or loosens α from an online estimate of task noise"* (§5.3). No barre
  α ni reporta curva de FAR. No repite configuraciones.
- **PACE** (arXiv 2606.08106, jun): e-process con α del usuario; la aceptación greedy
  hace 30-42 % de commits falsos. **HarnessBank** (arXiv 2607.13683, jul): gate 2σ con
  σ de las diferencias por tarea de la misma corrida; un gate de corrida única
  acredita una mejora neutra en ~60 % de los casos. Ambos vienen del informe del
  subagente (abstract y HTML), sin lectura propia todavía.

## La tesis, acotada

> Los gates que calibran su umbral contra el ruido lo miden **una vez**, sobre el
> harness base, y lo dejan fijo. El ruido cambia con la configuración, así que un
> umbral calibrado en H0 acepta cambios nulos de los incumbentes posteriores a una
> tasa que su calibración no controla. Este trabajo (1) mide cuánto cambia el ruido
> entre configuraciones de una misma suite, (2) mide la tasa de aceptación falsa de
> un δ fijo de H0 contra un δ re-derivado por configuración, y (3) entrega la curva
> umbral vs FAR que ninguno de los cuatro gates reporta.

Lo que **no** se afirma: que RRSI, SAGE, PACE o HarnessBank estén mal en su régimen
(frontera, Opus 4.8 o similares). La afirmación queda restringida al régimen ruidoso de
SLM locales, igual que el caveat de régimen del v2.

## Resultado nuevo (medido hoy, datos en mano)

`scripts/paper3_per_config_threshold.py` → `docs/paper3-per-config-threshold.json`.
En cada sweep, el primer brazo con ≥3 réplicas hace de H0 y los demás brazos (misma
suite, otra configuración) hacen de incumbentes. Con réplicas de una misma
configuración no hay efecto, así que todo Δ>0 aceptado es un falso positivo.

| | valor |
|---|---|
| sweeps con H0 + incumbentes | 25 |
| brazos incumbentes / pares nulos | 89 / 1.724 |
| tasa de volteo por brazo (mismo config) | mediana 6,3 %; p10 0 %; p90 17,6 %; máx 33,3 % (todas las suites) |
| rango de volteo entre brazos de un mismo sweep | mediana 11,6 pp; máx 26,3 pp (0 % a 26,3 %, curva multiescala) |
| incumbentes más ruidosos que su H0 | 29/89 (33 %) |
| **FAR con δ fijo de H0 (estilo RRSI)** | **7,3 %** |
| **FAR con δ re-derivado por configuración (leave-one-pair-out)** | **0,8 %** |
| — solo brazos con δ_H0 > 0 (75 brazos, 1.444 pares) | fijo 3,7 % vs por-config 0,9 % |
| — brazos con H0 sin ruido observado (δ_H0 = 0; 14 brazos) | fijo 25,7 % vs por-config 0,4 % |

Curva pooled δ→FAR: 0,00 → 26,4 %; 0,05 → 25,1 %; 0,10 → 7,8 %; 0,15 → 2,0 %;
0,20 → 0,7 %; 0,30 → 0,1 %.

Lectura: el caso degenerado (un H0 saturado o sin variación mide δ = 0) es donde la
calibración única falla en grande. Aun excluyéndolo, el δ fijo acepta cuatro veces
más cambios nulos. El precio de re-derivar es correr réplicas de cada incumbente.
Ese costo en corridas es la contribución 4 del v2, y ahora tiene contra qué medirse.

Corrección hecha hoy en el experimento central: `paper3_false_acceptance.py` incluía
14 grupos de sweeps marcados INVALID o contaminados por infraestructura (162 de 1.602
comparaciones). Ahora los excluye. Recalculado sobre los 133 archivos actuales (1.554
comparaciones): Meta-Harness 26,3 %, AutoDesign/HCL δ=1 15,2 %, HCL δ=3 1,0 %,
McNemar 0,1 %. La v1 queda en `paper3-false-acceptance-v1-2026-08-25.json`. El orden
y la curva no cambian.

## Contribuciones, v3

1. **Heterogeneidad del ruido entre configuraciones de una misma suite**. La tasa de
   volteo va de 0 % a 33 %, y un tercio de los incumbentes es más ruidoso que su base.
2. **FAR de la calibración única contra la re-derivada por configuración** (7,3 % vs
   0,8 %), con el desglose del caso degenerado δ_H0 = 0.
3. **Curva umbral vs FAR** sobre reglas publicadas y sobre δ continuo (el núcleo del
   v2, ahora con datos limpios).
4. **Precio en corridas** de re-derivar el umbral, contra el MDE declarado.
5. El falso positivo del Paper 2 como caso de estudio: pasó pre-registro, McNemar y
   Holm, y se disolvió contra un control de prompt idéntico.

## Threats nuevos

- **Estimador de δ**: se usó el máximo |Δ| entre réplicas para los dos brazos. Con 3
  réplicas, el leave-one-pair-out estima δ con solo 2 pares, mientras que H0 usa
  todos los suyos. El sesgo va en contra de la tesis (δ_H0 sale más grande), pero hay
  que mostrarlo con otros estimadores (percentil, half-split como EngDesign).
- **Elección de H0 = primer brazo** del sweep: es arbitraria. Hay que repetir con H0
  aleatorio y con H0 = brazo menos ruidoso.
- Los del v2 siguen vigentes: un régimen, circularidad parcial y procedencia de motor
  ausente en los sweeps históricos.


## Robustez (2026-10-17, `scripts/paper3_v3_robustness.py` → `docs/paper3-v3-robustness.json`)

Se varían el estimador de δ, la elección de H0 y el número de pares usados para
estimar δ. El IC95 de la diferencia sale de un bootstrap por sweep (cluster = archivo,
2.000 remuestras, 24 sweeps).

| δ | H0 | FAR δ fijo | FAR por-config | diferencia [IC95] |
|---|---|---|---|---|
| max | primero | 6,9 % | 0,8 % | +6,1 [+2,2, +11,3] |
| max | al azar (200 sorteos) | 7,3 % | 0,7 % | +6,6 [+5,2, +8,1] |
| max | el menos ruidoso | 17,8 % | 0,9 % | +16,9 [+11,7, +22,7] |
| max | el más ruidoso | 0,0 % | 0,5 % | −0,5 [−0,9, −0,2] |
| 2sd | primero / al azar | 6,8 / 7,5 % | 0,7 / 0,8 % | +6,1 / +6,8 (IC excluye 0) |
| mean | primero | 14,1 % | 18,2 % | −4,1 [−8,9, +0,8] |
| mean | al azar | 17,9 % | 17,8 % | +0,1 [−1,1, +1,1] |

Igualar el número de pares no cambia nada (±0,3 pp). Lo que se sostiene y lo que no:

- **Se sostiene con estimadores de cola** (máximo, 2·sd), que son los que usa RRSI ("the
  spread"). Re-derivar δ por configuración baja la FAR de ~7 % a <1 %, con un IC que
  excluye el cero. Elegir H0 al azar da lo mismo que tomar el primer brazo, así que el
  resultado no depende de esa elección.
- **El δ fijo solo controla la FAR si H0 resulta ser la configuración más ruidosa.** En
  ese caso es conservador (0,0 %), y el costo es potencia: rechaza mejoras reales
  menores que el ruido de H0. Si H0 es la más tranquila, la FAR sube a 17,8 %. Un δ fijo
  apuesta a cuál es el ruido del incumbente, y esa apuesta no se puede verificar sin
  medirlo.
- **Con un estimador central (media de |Δ|) ninguna regla controla la FAR**: ~18 % en
  ambas. La re-derivación no rescata un umbral mal elegido. Esto es la curva del v2 vista
  desde otro lado: el **nivel** del umbral manda, y la re-derivación ayuda solo cuando
  ese nivel es una cola de la distribución nula.


### Potencia (`scripts/paper3_v3_power.py` → `docs/paper3-v3-power.json`)

A cada par nulo de los incumbentes se le suma una mejora real e (fracción de la suite):
Δ' = Δ + e. Se usa el estimador máximo con leave-one-pair-out. La tabla da el
porcentaje de mejoras aceptadas por cada regla.

| e | H0 primero: fijo / por-config | H0 al azar: fijo / por-config |
|---|---|---|
| 0 (FAR) | 7 % / 1 % | 7 % / 1 % |
| 0,03 | 20 % / 38 % | 33 % / 38 % |
| 0,05 | 22 % / 39 % | 34 % / 39 % |
| 0,10 | 38 % / 64 % | 56 % / 62 % |
| 0,15 | 60 % / 83 % | 76 % / 81 % |
| 0,20 | 73 % / 89 % | 87 % / 87 % |

**No es un trade-off: la re-derivación domina en los dos ejes.** Tiene menos falsas
aceptaciones y potencia igual o mayor en todos los tamaños de efecto. La razón es que un δ
fijo queda mal dimensionado en ambas direcciones: demasiado estrecho para los
incumbentes más ruidosos que H0 (de ahí las aceptaciones falsas) y demasiado ancho para
los más tranquilos (de ahí las mejoras perdidas). El costo de la re-derivación no es
potencia sino corridas: hay que repetir cada incumbente al menos 3 veces.

Consecuencia para la tesis: la contribución 2 queda como "con un umbral de cola, una
calibración única sobre H0 acepta ~7 % de cambios nulos (hasta 18 % si H0 es la
configuración más quieta); re-derivarlo por configuración lo baja a <1 %". Además se
reporta el caso adverso (H0 más ruidosa) y su precio en potencia, medido abajo.

## Qué falta, en orden

1. ~~Robustez y potencia~~ HECHAS (secciones de arriba).
2. Leer PACE y HarnessBank completos (hoy solo vía subagente).
3. Reescribir la introducción y el Related Work con los cuatro gates como antecedentes
   directos. Agregar al bib los cuatro gates y además Noise Floor Audit (2608.22331),
   Coding Agents Have Converged (2609.17394) y Hardy et al. (2610.00651).
4. Preprint en arXiv antes de diciembre: la ventana se cierra (3 gates en 6 semanas).
5. Venue: `/paper-match` con el manuscrito. EMSE ya tiene el Paper 1; considerar TOSEM
   o JSS (no están en el vault).
