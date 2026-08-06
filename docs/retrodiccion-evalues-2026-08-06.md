# Retrodicción e-values/SPRT sobre los sweeps históricos — resultados

Fecha: 2026-08-06. Técnica #1 de `docs/techniques-roadmap-2026-08-06.md`
(donde el prior quedó declarado antes de correr). Script:
`docs/retrodiccion-evalues-2026-08-06.py` (métodos y supuestos en su
docstring). Cero GPU: puro análisis de los JSON ya commiteados.

## Qué se corrió

Todos los pares de brazos con ≥20 celdas pareadas en los JSON de `docs/`:
**300 contrastes** (el número está inflado deliberadamente: sweeps multi-brazo
aportan todos sus pares, y las variantes `.contam` conservadas también entran
— es un stress test del método, no un inventario de decisiones históricas).
Dos monitores por contraste, en el orden de ejecución real:

- **E-process** (mixture Beta(½,½) sobre discordantes de McNemar; Ville a
  1/α=20): solo puede rechazar H0, nunca aceptarla.
- **SPRT doble unilateral** (p1=0.75 ≈ razón de discordantes 3:1; α=0.05,
  β=0.20): puede además aceptar H0 temprano.

## Resultados

| | valor |
|---|---|
| Ahorro SPRT mediano (todos los contrastes) | **62.1%** |
| Ahorro mediano entre contrastes decididos | **74%** |
| Ahorro total estimado (cota superior) | **~119 horas** |
| Contrastes decididos antes del final | 228/300 |

**Acuerdo con el análisis n-fijo** (la parte que mi prior no anticipó bien):

| Veredicto secuencial | n | Concuerda con McNemar final | |
|---|---|---|---|
| E-process rechaza | 194 | 181 | **93%** |
| SPRT declara efecto | 188 | 172 | **91%** |
| SPRT declara null | 40 | 19 | **48%** |

## Evaluación del prior, sin maquillaje

- **"30-50% de ahorro mediano"** → se quedó corto: **62%** (74% entre
  decididos). La dirección era correcta; la magnitud, conservadora.
- **"sin cambiar ninguna decisión histórica"** → **FALSO tal como se
  enunció.** Un 7-9% de los llamados de efecto secuenciales no coinciden con
  la significancia n-fijo (rechazos tempranos que los datos posteriores
  diluyen — válidos bajo control anytime de α, pero *distintos*). Y el 52% de
  los nulls del SPRT tienen McNemar final significativo.

## La lectura correcta del desacuerdo en los nulls

No es (principalmente) error del método: es **definicional**. El SPRT con
p1=0.75 acepta H0 cuando el efecto es menor que 3:1 en discordantes — o sea
su "null" significa *"efecto por debajo del umbral declarado"*, mientras
McNemar detecta cualquier desviación de ½ con n suficiente. Para los
criterios pre-registrados de braze —que son de umbral (±3 tareas ≈ 9pp), no
de significancia pura— la semántica del SPRT es de hecho **la que el proyecto
usa**: "efecto sub-umbral" y "no adoptar" son la misma decisión. Pero hay que
decirlo en cada criterio, no asumirlo.

## Decisión de la técnica #1

**Validada con matices — se adopta el camino, no el default.** Concreto:

1. **Para gates** (como el de plomería del A/B en curso): e-process puro.
   93% de acuerdo, solo-rechazo (un gate solo necesita disparar), y habría
   disparado el gate de plomería mucho antes de las 27h.
2. **Para criterios de adopción**: SPRT con **p1 mapeado al umbral
   pre-registrado de cada experimento** (no un 0.75 genérico), y la
   aceptación de H0 documentada como "sub-umbral", que es la semántica real
   de los criterios del proyecto.
3. **El ahorro reportado es cota superior**: asume ejecución pareada de los
   brazos. Con la ejecución brazo-por-brazo actual del bench aplica del
   segundo brazo en adelante. La integración (`--sequential-stop`, opt-in,
   off por default como toda palanca nueva) queda como siguiente paso de la
   técnica, con su propio A/B operativo… que irónicamente ya no necesita
   A/B: la retrodicción sobre 300 contrastes ES la validación offline.

## Limitaciones

- El orden de llegada de pares se reconstruyó del orden del array `results`
  del primer brazo; sweeps con orden de ejecución distinto al registrado
  moverían los puntos de corte (no el veredicto final).
- Los 300 contrastes incluyen pares sin sentido experimental (escalas de la
  curva entre sí, contaminados) — a propósito, como stress test; el acuerdo
  entre los pares "reales" no se separó en esta pasada.
- Las ~119h asumen el mismo mix de hardware que produjo cada sweep.
