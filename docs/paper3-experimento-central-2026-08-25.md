# Paper 3, experimento central: tasa de aceptación falsa de los gates publicados

Fecha: 2026-08-25. Script: `scripts/paper3_false_acceptance.py`
(seed fijo 20260825). Datos: `docs/paper3-false-acceptance.json`.
Corrido **sin hardware de inferencia** — es re-análisis del banco.

## Qué se midió

De 105 JSON del repositorio, 85 tienen corridas utilizables. Agrupando
por (archivo, backend) se obtuvieron **140 grupos con réplicas de
configuración idéntica** (mediana 19 tareas por grupo, 41 backends
distintos). Dentro de un grupo, dos repeticiones de la MISMA
configuración forman un par donde **por construcción no hay efecto que
detectar**: cualquier diferencia es ruido. Se construyeron **1.602
comparaciones nulas** (ambos órdenes, porque el ruido no tiene
dirección privilegiada) y se preguntó a cada regla publicada si
"aceptaría" ese cambio.

## Resultado

| regla | acepta | tasa de aceptación falsa |
|---|---|---|
| Meta-Harness (mejor score en search set) | 391 | **24,4 %** |
| AutoDesign (`J_train↑ ∧ J_dev no baja`) | 230 | **14,4 %** |
| HCL (`Δ ≥ 1` + retención de anchor) | 230 | 14,4 % |
| HCL (`Δ ≥ 3` + retención de anchor) | 12 | **0,7 %** |
| braze (McNemar exacto pareado, α=0,05) | 0 | **0,0 %** |

Una de cada cuatro veces, la regla de selección por mejor score
promovería un cambio que no existe. Con retención histórica y un
umbral mínimo de una tarea, una de cada siete.

## Dos matices que la lectura honesta exige

**1. El umbral hace casi todo el trabajo, no la estructura del gate.**
HCL con `δ=1` acepta 14,4 % y con `δ=3` cae a 0,7 %. No es que
"estimación puntual = malo": es que **un umbral de mejora mínima
demasiado bajo no protege de nada**, y ninguno de los tres papers
justifica su δ contra un piso de ruido medido. El aporte del Paper 3
no debería redactarse como "hay que usar estadística" sino como
**"el umbral tiene que derivarse del ruido de la configuración, y eso
exige medirlo"** — que es más útil y más difícil de refutar.

**2. Nuestro propio gate es conservador hasta el extremo, y hay que
decirlo.** Cero aceptaciones en 1.602 pruebas no es virtud pura: con
pocos pares discordantes, el McNemar exacto **no puede** alcanzar
p<0,05 (con 5 discordantes el mínimo alcanzable es 0,0625). Es decir,
nuestro gate no acepta ruido, pero tampoco detectaría efectos reales
moderados en suites chicas — es el MDE que el Paper 2 ya declaraba.
Reportar el 0,0 % sin ese caveat sería exactamente el tipo de
autocomplacencia que este proyecto persigue en otros.

## Matiz externo: no todos ignoran la validación

**HarnessOpt-Bench** (arXiv 2608.06301, 6-ago-2026, Ursekar et al.)
puntúa a sus candidatos en una **partición held-out inaccesible
durante la búsqueda**, con entorno de ejecución auditado y diseño
explícito para "evaluación cara y estocástica" (111 corridas
puntuadas). El claim del outline —"ninguno maneja el ruido"— **queda
refutado como generalización** y debe reescribirse.

La distinción precisa que sí se sostiene: un held-out protege contra
**sobreajuste de la búsqueda** (elegir el candidato que memorizó el
train), pero no contra **ruido de medición en el test final** — si la
métrica del held-out tiene varianza, una ganancia observada ahí puede
ser ruido igual. Son defensas contra problemas distintos, y la segunda
sigue ausente en los cuatro sistemas. Ese es el hueco que el Paper 3
puede reclamar con honestidad.

## Consecuencias para el outline del Paper 3

1. Reemplazar "ninguno maneja el ruido" por la distinción
   sobreajuste-de-búsqueda vs ruido-de-medición, citando
   HarnessOpt-Bench como el caso que sí ataca el primero.
2. El resultado central pasa a ser la **curva umbral vs tasa de
   aceptación falsa** (δ=1 → 14,4 %; δ=3 → 0,7 %), no una lista de
   sistemas reprobados. Es más constructivo y más citable.
3. Incorporar el caveat del propio gate (conservadurismo, MDE) en el
   mismo lugar donde se reporta su 0,0 %.
4. Añadir a la cola del bib: `ursekar2026harnessopt` y los otros
   trabajos del campo detectados el 25-ago (ver
   `docs/nota-campo-harness-2026-08-25.md`).

## Reproducir

```
python3 scripts/paper3_false_acceptance.py
```
Determinista (seed 20260825). No requiere Nitro ni GPU.
