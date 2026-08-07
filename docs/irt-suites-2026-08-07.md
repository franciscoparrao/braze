# IRT sobre las suites: qué ítems cargan información — resultados

Fecha: 2026-08-07. Técnica **#2** de `docs/techniques-roadmap-2026-08-06.md`
(prior declarado antes de correr). Script: `docs/irt-suites-2026-08-07.py`.
Cero GPU para el ajuste; una corrida dirigida de 6 tareas para el
seguimiento en vivo.

## Primero, una corrección de nomenclatura que el análisis destapó

**`default.toml` tiene 19 tareas, no 57.** El "57/57" que el proyecto cita
—incluido el titular del LocalBackend— son **corridas**: 19 ítems × 3
repeticiones. Lo mismo el "n=95" (19 × 5) y el "41/57" del stencil. No
invalida ningún resultado (todos los contrastes son pareados y consistentes
entre sí), pero la resolución real del banco es de **19 ítems**, no 57 — y
esa es la cifra que importa cuando se discute cuánto puede detectar.

## Método

2PL, `P(correcto|θ) = 1/(1+exp(-a(θ-b)))`, estimado por **máxima
verosimilitud marginal** (cuadratura Gauss-Hermite sobre θ~N(0,1), 41
nodos; habilidades por EAP). 482 respondentes × 19 ítems = 9.158 celdas.

**Autocorrección durante el análisis**: el primer ajuste usó JML con ridge
suave y **degeneró** — `a` pegado al tope superior en 18 de 19 ítems. Es el
problema de parámetros incidentales (θ libre por respondente estimado con
solo 19 observaciones). MML lo resuelve integrando θ sobre su prior; con
eso `a` pasa a un rango interpretable (0,44–6,00, solo 5 en el tope). El
método malo quedó documentado en el docstring del script para que nadie lo
repita.

## Resultado 1 — la información se concentra, pero menos de lo esperado

12 de 19 ítems cargan el 80% de la información de Fisher; 15 cargan el 90%.
La cabeza de la tabla son las tres tareas de `error_recovery` y una de
`distractor_selection` (a≈6, b≈0,2 — dificultad justo donde vive el grueso
de los modelos medidos); la cola son las `no_tool_*` (a≈3, b≈−0,9: fáciles,
poco informativas para separar modelos de esta cohorte).

**Validación** (lo que decide si sirve): ¿el subconjunto reproduce el
*ranking de brazos* de cada sweep histórico?

| k | Spearman medio | mediana | ρ=1.0 exacto |
|---|---|---|---|
| 6 | 0,872 | 0,866 | 4/13 |
| 8 | 0,888 | 0,866 | 4/13 |
| 10 | 0,881 | 0,866 | 4/13 |
| 12 | **0,949** | 0,986 | 6/13 |

**Veredicto del prior**: la predicción era "12-15 ítems reproducen el
ranking" — se cumple en tendencia (k=12 → ρ medio 0,949) pero **no con
fidelidad suficiente para reemplazar la suite**: el ranking exacto se
preserva solo en 6 de 13 sweeps. Sobre 19 ítems, recortar a 12 ahorra 37%
de cómputo a cambio de ~5% de error de ranking. **No se adopta como
reemplazo**; sí queda como criterio para *qué agregar* (los ítems de la
cola tienen poco que aportar y el banco crece mejor por la cabeza).

Y hay una razón estructural para no forzarlo: la técnica **#1** (e-process)
ataca el mismo cuello —costo de reloj— sin tocar la validez del banco. Entre
recortar ítems (pierde información) y parar temprano (no la pierde), gana la
segunda. El combo que el roadmap imaginaba (3-4× de ahorro) se logra mejor
con #1 sola.

## Resultado 2 — el hallazgo real: un ítem anti-discriminante

`read_file_basic` tiene **a = 0,44** (el resto: 2,1–6,0) e **info = 0,043**,
veinte veces menos que el siguiente. Un ítem con discriminación ≈0 es uno
cuyo resultado es *independiente de la habilidad del modelo*. Los datos
crudos lo confirman, y de la peor manera — **anti-correlaciona**:

| modelo | pass |
|---|---|
| qwen2.5:**3b** | 46% |
| qwen2.5:**7b** | **12%** |
| gemma4:**e2b** | 40% |
| gemma4:**e4b** | **10%** |

El modelo más grande de cada familia falla **más**. 194 de 299 fallos son
`assertion_tool_call` (no llamó a `read_file`).

### La hipótesis que se cayó

Hipótesis natural: la aserción castiga una alternativa legítima (el modelo
capaz resuelve con `shell_exec` + `wc -l` en vez de `read_file`). **Refutada
en vivo**: corrida dirigida hoy (qwen2.5:7b y 3b, 3 reps cada uno,
`BRAZE_BENCH_KEEP_SESSIONS=1`) da **3/3 y 3/3**. El fallo no reproduce.

### Lo que sí muestran los datos

Los fallos se agrupan en el tiempo (18% el 10-jul, 31% el 12-jul → 62% el
19-jul → 100% hoy), y en el peor día **65 de 143 fallos son
`model_backend_error`**, no aserciones. Es decir: el ítem no está mal
diseñado — estaba **midiendo la infraestructura**, en el período de los
errores de transporte de Ollama 0.30.7 que el upgrade a 0.32.1 (20-jul)
cerró. La anti-correlación por tamaño es consistente con eso: los modelos
grandes cargaban más el servidor.

**Consecuencia metodológica, que es lo que vale**: un ítem con
discriminación ≈0 en IRT es un detector de contaminación de banco que no
requiere leer una sola transcripción. Habría marcado los sweeps del 10-12
de julio como sospechosos **en su momento**, semanas antes de que el
diagnóstico de transporte los encontrara a mano.

## Decisión

```text
Decision: NO adoptar la reduccion de suite por IRT (el ranking exacto se
  preserva en 6/13 sweeps; la tecnica #1 ataca el mismo cuello sin costo
  de validez). SI adoptar el diagnostico de discriminacion como chequeo
  rutinario de salud de banco.
Evidencia: ajuste MML sobre 482x19; validacion de ranking por sweep;
  corrida dirigida que refuto la hipotesis de diseno del item.
Scope donde aplica: default.toml (19 items, 482 respondentes).
Scope donde NO aplica: discriminating.toml tiene 9 respondentes — IRT no
  es ajustable ahi todavia. Requiere ~30+ respondentes; llegara solo con
  el uso.
Riesgos: los `a` en el tope (5 de 19) indican que 19 items es poca base
  para 2PL; un Rasch (a fijo) seria mas estable si se repite el analisis.
Estado nuevo: la reduccion, archived. El chequeo de discriminacion,
  promoted a rutina post-sweep.
```

## Siguiente paso concreto

Agregar al análisis post-sweep un chequeo de una línea: **si algún ítem
queda con `a < 1.0`, marcar el sweep para revisión antes de interpretarlo**.
Es barato, es offline, y su valor ya está demostrado retroactivamente sobre
el incidente de transporte de julio.
