# Análisis de poder de round-economics — la palanca que falta son ítems, no cómputo

Fecha: 2026-08-09
Línea: round-economics (`docs/hypothesis-2026-07-28-round-economics.md`)
Insumo: `nitro:~/piloto-round-economics.json` (636 filas del piloto de costo,
`docs/round-economics-pilot-costo-2026-08-08.md`)
Script: `docs/round-economics-power-2026-08-09.py`

El piloto dejó una decisión abierta: "¿se puede pagar el poder que el
factorial necesita, o se cierra la línea como no-medible?" Este análisis
convierte esa pregunta en números. La respuesta corta: **el poder que falta
no se compra con horas de Nitro — se compra autorando ítems, y el precio en
ítems es desproporcionado.**

## Método

Modelo semi-paramétrico sobre los datos del piloto: p(pass) por
(tarea, brazo) estimada de las 3 réplicas; experimentos futuros simulados
como draws binomiales por celda; la interacción se estima igual que en el
análisis del piloto (diferencia de diferencias, IC95% bootstrap pareado por
tarea); poder = fracción de simulaciones cuyo IC excluye 0.

Sanity check: el punto reproduce exactamente el documentado (+5,7 pp). El IC
sale algo más ancho ([+0,0, +13,2] vs [+0,0, +10,2]) por un detalle de
agregación del bootstrap (media por tarea primero vs pooling de celdas); no
cambia ninguna conclusión — ambos tocan cero.

Escenarios de tamaño de efecto: el observado (+5,7 pp) y encogimientos al
60%/50% (~+3,4/+2,9 pp), porque el punto del piloto es él mismo ruidoso
(sd entre réplicas 5,0 pp > efecto) y el estimador observado de un efecto
que apenas asoma sobreestima por construcción (winner's curse). El
encogimiento escala solo el componente de interacción de la descomposición
2×2 por tarea, preservando efectos principales.

## Resultado 1: las réplicas NO compran poder

Poder simulado con 53 tareas fijas, variando réplicas:

| escenario | R=3 | R=5 | R=8 | R=10 | R=15 | R=20 |
|---|---|---|---|---|---|---|
| efecto observado (+5,7 pp) | 26% | 26% | 24% | 28% | 26% | 30% |
| encogido 60% (~+3,4 pp) | 17% | 15% | 19% | 23% | 23% | 25% |

**Meseta en ~25-30% sin importar R.** La razón es estructural, no numérica:
la inferencia es un bootstrap pareado **por tarea**, así que la varianza que
manda es la heterogeneidad de la interacción **entre tareas** — y esa no se
encoge replicando la misma tarea. Con R→∞ las p por celda se conocen
exactas y el IC sigue dominado por qué 53 tareas cayeron en el banco.
Multiplicar réplicas (la palanca barata: solo horas de Nitro) es gastar en
la varianza equivocada.

## Resultado 2: los ítems SÍ compran poder, pero el precio es autorar

Poder simulado con R=3, variando el número de tareas (remuestreadas del
pool actual, o sea "más tareas intercambiables con las que hay"):

| escenario | T=53 | T=80 | T=120 | T=200 | T=300 |
|---|---|---|---|---|---|
| efecto observado (+5,7 pp) | 31% | 39% | 52% | 72% | 92% |
| encogido 60% (~+3,4 pp) | 19% | 26% | 35% | 47% | 67% |

| objetivo | ítems nuevos a autorar | Nitro (4 brazos × R=3) |
|---|---|---|
| ~72% de poder si el efecto es el observado | ~147 | ~25 h |
| ~80-90% si el efecto es el observado | ~250 | ~38 h |
| ~80% si el efecto real es ~+3,4 pp | >300 | >40 h |

El cómputo es lo de menos (una-dos noches de Nitro). El costo real es
**autorar 150-300 tareas nuevas de calidad discriminante** — contra las 34
de `discriminating.toml` que costaron su propio ciclo de diseño — y con el
riesgo abierto de que el efecto real sea menor que el observado, en cuyo
caso ni 300 alcanzan.

## Caveats del modelo

- Las p por celda vienen de 3 réplicas, así que la heterogeneidad entre
  tareas del modelo incluye ruido de estimación además de heterogeneidad
  real — el poder a T grandes puede estar algo subestimado. No cambia la
  meseta de réplicas, que es estructural.
- Los escenarios "encogidos" son la corrección honesta al winner's curse;
  el efecto real puede ser cualquiera de la banda [−6,7, +18,1] que las 3
  réplicas permiten.
- Simular "más tareas" como remuestreo del pool asume que las tareas nuevas
  se parecen en distribución a las 53 existentes. Tareas diseñadas
  específicamente para que la interacción muerda (más sensibles al
  presupuesto) podrían rendir más por ítem — pero diseñarlas mirando el
  efecto es seleccionar sobre el resultado, exactamente lo que la regla de
  construcción del banco prohibió.

## Recomendación

**Cerrar la línea como no-medible con los recursos del proyecto** — la
salida que el pre-registro contempla. Argumento:

1. El asesino #2 del pre-registro se materializó y este análisis muestra
   que no tiene arreglo barato: la palanca disponible (réplicas/cómputo)
   no compra poder, y la que sí (ítems) cuesta semanas de autoría con
   retorno incierto.
2. Lo que la línea ya produjo queda en pie y es publicable como nulo
   piloteado: el instrumento (presupuesto de wall-clock como brazo, 4,4×
   de separación, 8× en frecuencia de corte), la afirmación de mecanismo,
   el piso de ruido del régimen, y este análisis de poder como cierre
   metodológico.
3. La consecuencia del gate pre-registrado: **metaheurísticas queda
   bloqueada** — el piloto no demostró que la interacción sea medible, y
   esa era la condición de entrada.

Si más adelante aparece un banco grande "gratis" (p.ej. adoptar un banco
externo con cientos de ítems compatibles), la línea puede reabrirse con
este análisis como dimensionamiento ya hecho.
