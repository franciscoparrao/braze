# Piloto de costo de round-economics — instrumento, calibración y protocolo

Fecha: 2026-08-08
Línea: round-economics (`docs/hypothesis-2026-07-28-round-economics.md`)
Estado: instrumento construido y verificado en vivo; calibración en curso

Este documento cubre el **piloto 1** del pre-registro ("piloto de costo"),
que ese documento define así:

> presupuesto de wall-clock por tarea como *brazo* del bench, no como
> timeout de infraestructura. Después una celda: GPU vs CPU en
> `discriminating.toml`, avara vs derrochadora, 3 repeticiones. Solo para
> ver si la separación de wall-clock alcanza y si la interacción es
> siquiera visible.

## 1. Lo que faltaba construir

El pre-registro decía que lo único pendiente era el presupuesto de
wall-clock como condición de corte de primera clase. Está hecho.

### 1.1 `Engine::with_max_turn_wall_clock`

Tercer corte del turno, junto a `max_turn_iterations` (rondas) y
`max_turn_total_tokens` (tokens). El único de los tres cuyo recurso
**cambia de precio con el despliegue**, que es la razón entera de la
línea: los otros dos son invariantes a si una ronda tarda 2 s o 90 s.

Tres decisiones de diseño que no son cosméticas:

- **Corta en el borde de la ronda**, no abortando la que está en vuelo.
  Un `tokio::time::timeout` alrededor de `run_turn` — que es lo que el
  bench ya tenía — mata la ronda en curso y con ella su `Usage`, así que
  las rondas y los tokens de toda fila cortada quedan censurados
  (J-21/J-10) y no son comparables entre brazos. El corte en el borde
  deja la contabilidad intacta. Esta es la diferencia entera entre un
  timeout de infraestructura y un brazo experimental.
- **No concede la ronda de resumen sin tools** que sí concede el
  presupuesto de tokens. Esa ronda cuesta tiempo, y su costo escala con
  el precio de la ronda — o sea con el factor que el experimento
  manipula. Concederla le regalaría al brazo caro una ronda extra medida
  en el mismo eje que el experimento varía.
- **El reloj arranca antes de la ronda del planner**, no antes del loop
  del ejecutor. El proyecto ya había decidido que la ronda del planner
  cuenta como ronda (`TaskResult::rounds` la cuenta); arrancar el reloj
  después le daría a un brazo con planner una ronda gratis.

`FailureCause::WallClockExhausted` es deliberadamente distinta de
`Timeout`. Una fila `[Timeout]` en un sweep con presupuesto significa que
el backstop mordió primero — o sea que el experimento midió el backstop.
El bench ahora deriva el backstop a 3× el presupuesto cuando no se lo
fija a mano, rechaza al arranque un backstop menor o igual al
presupuesto, y avisa post-sweep si igual quedaron filas censuradas.

**El presupuesto está cuantizado por la duración de la ronda**, y eso no
es un defecto de implementación sino una propiedad del fenómeno. El corte
se evalúa al EMPEZAR una ronda, así que un turno que converge en su
primera ronda puede pasarse del presupuesto por mucho y aun así contar
como éxito. Verificado en vivo (Ollama local, presupuesto de 2 s):
`no_tool_qa` convergió en una ronda de 134 s y pasó; `read_file_basic`
necesitaba una segunda ronda y salió `[WallClockExhausted]` con
`rounds=1` y sus tokens intactos (`tokens_in=1155, tokens_out=22`) — la
contabilidad que el backstop de infraestructura pierde.

La lectura importante: la granularidad del control es el precio de la
ronda. El brazo barato tiene control **más fino** sobre su presupuesto
que el caro, porque le caben más decisiones adentro. Eso es parte de lo
que la hipótesis afirma, no ruido a corregir.

Una consecuencia del contrato que ya tenía el bench, y que conviene tener
presente al leer los números: `passed = converged && assertions_passed`.
O sea que una tarea cuyo artefacto quedó **completo y correcto** pero
cuyo turno se cortó antes de que el modelo dijera "listo" cuenta como
fallo. Es la misma contabilidad que ya tienen `Timeout` y
`MaxIterationsExhausted`, así que no rompe comparabilidad con los sweeps
previos, y castiga por igual a los cuatro brazos — no sesga la
interacción. Pero es una contabilidad conservadora, y el pass rate a
presupuesto fijo que sale de acá es un piso, no el trabajo hecho.

### 1.2 El factorial tiene que caber en UN sweep

El pareo (tarea, repetición) que la estadística del Paper 1 usa —McNemar
exacto— es *dentro* de la corrida. Dos llaves nuevas de `+ablate:`:

- `max-iterations=N` — el tope de rondas por fila. Es, antes que nada,
  la mitad "avara vs derrochadora" del factorial; vivía solo en `Config`,
  o sea global al sweep.
- `gpu-layers=N` — capas ofloadeadas a GPU por fila. Es el instrumento B
  (mismos pesos, otro precio); vivía solo en `BRAZE_LOCAL_GPU_LAYERS`,
  que es del proceso.

### 1.3 Dos bugs que habrían invalidado el experimento en silencio

Aparecieron al conectar las piezas, y ninguno de los dos habría dado
señal de error:

1. **El caché de modelo del `LocalBackend` no incluía las capas GPU en su
   clave** (era `(path, n_ctx, env)`). El segundo brazo del sweep habría
   reusado el modelo cargado por el primero: los dos precios de ronda
   medidos al mismo precio, con el JSON declarando que eran distintos.
2. **Cada rollout de `+ablate:ttc=N` recibía el presupuesto completo.**
   La configuración derrochadora habría corrido con N× el tiempo de la
   avara — confundiendo el tratamiento con el recurso que el experimento
   mantiene fijo. Ahora el presupuesto es de la **tarea** y los rollouts
   se reparten un deadline: los que ya no entran, no corren. Que quepan
   más rollouts cuando la ronda se abarata **es** el mecanismo que la
   hipótesis predice, así que el reparto no es solo corrección
   metodológica, es parte del fenómeno.

Verificación: 1.138 tests del workspace, clippy `-D warnings` limpio,
más el chequeo en vivo de `+ablate:gpu-layers` contra el binario CUDA real
en Nitro (`source="caller"`, `gpu_layers=0` vs `99` en la traza del
backend).

## 2. Calibración

Sub-suite de 6 tareas de `discriminating.toml` cubriendo sus familias
(`docs/../crates/braze-bench/suites/calibracion.toml`, no versionada como
banco de medición). Nitro, binario `local-cuda`, temp 0.7 / top_p 0.8 /
top_k 20 / repeat_penalty 1.05, seed 42.

### 2.1 El precio de la ronda separa bien

| | s/ronda | mediana s/tarea | media s/tarea |
|---|---|---|---|
| GPU (`gpu-layers=99`) | 3,9 | 5,9 | 10,8 |
| CPU (`gpu-layers=0`) | 16,9 | 32,7 | 62,5 |

**Separación ≈ 4,4× por ronda**, mejor que el 3-4× que el pre-registro
esperaba del instrumento B. La manipulación de costo alcanza; no hace
falta gastar la única iteración declarada (combinar A y B).

### 2.2 Bajo temperatura los dos precios NO producen el mismo stream

El pre-registro justifica el instrumento B así: "bajo decodificación
greedy producen la misma secuencia de tokens a distinto precio", que es
una manipulación causal del costo con la capacidad fijada **por
construcción**. Eso vale para greedy.

El piloto **no** corre greedy, y por una razón que el propio proyecto ya
midió: con greedy las réplicas son copias (el hallazgo v9 L-9 que dejó
ininterpretable el piloto de contexto), así que no habría forma de
estimar el ruido del régimen. Con temperatura, las diferencias de punto
flotante entre GPU y CPU desempatan distinto y las trayectorias divergen:
en la calibración, `renombrar_campo_5_usos` usó 3 rondas en GPU y 11 en
CPU.

Consecuencia honesta: la capacidad queda fijada **en distribución**, no
token a token. El sampler es idéntico entre brazos de precio (mismo seed,
misma temperatura), así que el factor de precio no toca el régimen de
sampling — pero la garantía fuerte del pre-registro se debilita y hay que
declararla en Threats, no venderla como está escrita.

### 2.3 `discriminating.toml` tiene efecto de suelo bajo la clase gpt-oss

Este es el hallazgo que más movió el piloto, y es sobre el banco, no
sobre la hipótesis. Dos modelos independientes cayeron en el mismo punto:

| par modelo × banco | pass | s/tarea GPU (media) |
|---|---|---|
| `qwen2.5:3b` × `discriminating` | **1/6** | 10,8 |
| `gemma-4-E4B` × `discriminating` | **1/6** | **134,8** |
| `qwen2.5:3b` × `default` (19 tareas) | **15/19 = 78,9%** | 4,1 |

`discriminating.toml` fue construida para discriminar a `gpt-oss:20b` y
**no discrimina por debajo de esa clase**. Un piloto contra el piso no
puede responder lo que el piloto existe para responder: ni la
configuración avara ni la derrochadora tienen adónde mover el resultado,
así que una interacción nula sería del banco y no del fenómeno.

`gemma-4-E4B` se probó como sustituto —QAT 3,9 GB, entra entero en los
6 GB de la RTX 3050, o sea contraste de precio intacto— y quedó en el
mismo 1/6, además de costar 12× más por tarea que qwen. Se descartó.
`gpt-oss:20b` también: 11,3 GB en una GPU de 6 GB es offload parcial, el
contraste de precio caería de 4,4× a ~1,5× y debilitaría la manipulación
misma.

### 2.4 El banco del piloto: la unión de los dos

`suites/round-economics-v1.toml` = `default.toml` (19) +
`discriminating.toml` (34) = **53 tareas**, sin editar ninguna. La
resolución sube a **1,9 pp por ítem** —mejor que los 2,9 del
pre-registro y que los 5,3 de `default` solo— y `qwen2.5:3b` cae cerca
del 40%, o sea el centro de la banda, que es donde una interacción tiene
lugar para aparecer en las dos direcciones.

La regla de construcción es la **unión**, declarada antes de mirar
ningún efecto. Elegir ítems por dificultad observada habría sido
seleccionar sobre el resultado; el chequeo de salud de banco del bench
(técnica #2, `docs/irt-suites-2026-08-07.md`) reporta *después* qué ítems
no discriminaron, que es diagnóstico post-hoc y no filtro previo.

**Dos desviaciones del pre-registro, ambas declaradas.** El pre-registro
fija `discriminating.toml` como banco y `qwen2.5:3b` como modelo de esta
celda. El modelo se conserva; el banco cambia. La razón es una propiedad
del banco **medida después de escribir el pre-registro**, no un resultado
que no gustó — y el banco nuevo es estrictamente más resuelto que el que
el pre-registro pedía, no menos.

Advertencia que viaja con el banco: es nuevo y **no tiene piso de ruido
propio**. El piloto que lo estrena lo mide como primer entregable.

## 3. La celda que se corrió

Modelo `qwen2.5:3b` por `LocalBackend` (blob de Ollama, familia ChatML),
banco `round-economics-v1.toml`, 3 réplicas, temp 0.7 / top_p 0.8 /
top_k 20 / repeat_penalty 1.05, seed 42. Los cuatro brazos en **un
sweep**, para que el pareo (tarea, repetición) exista.

| brazo | precio | configuración |
|---|---|---|
| `gpu-layers=0;max-iterations=3` | caro | avara |
| `gpu-layers=0;max-iterations=20;ttc=3` | caro | derrochadora |
| `gpu-layers=99;max-iterations=3` | barato | avara |
| `gpu-layers=99;max-iterations=20;ttc=3` | barato | derrochadora |

- **Presupuesto de wall-clock: 30 s por turno.** Elegido de la
  calibración para que muerda a precio caro (mediana CPU ~20-33 s) y no
  a precio barato (mediana GPU ~3-6 s) — que es el régimen donde la
  interacción puede aparecer. Backstop de infraestructura: 600 s, muy
  por encima, para que ninguna fila salga censurada.
- **`best-of-n` queda fuera del piloto.** El pre-registro define
  "derrochadora" como TTC + best-of-n + reintentos amplios; con `ttc=3`
  y `best-of-n=3` el costo por tarea se multiplica por 9 y bajo un
  presupuesto de 30 s no terminaría nada. Entra al factorial completo si
  el piloto sobrevive. Declarado, no omitido.
- **Sin `--sequential-stop`.** El corte secuencial ahorra tiempo cuando
  el criterio ya está decidido, pero acá el primer entregable es el piso
  de ruido del régimen, y eso necesita las celdas completas.

## 4. Resultados

636 celdas (53 tareas × 3 réplicas × 4 brazos), 6 h 40 min en Nitro.
Datos: `nitro:~/piloto-round-economics.json`.

### 4.1 La manipulación funcionó — el presupuesto mordió como se diseñó

Es la pregunta que el pre-registro puso primero ("si la separación de
wall-clock alcanza") y la respuesta es sí, sin ambigüedad:

| brazo | cortes por presupuesto | mediana s/tarea |
|---|---|---|
| caro, avara | **25**/159 | 31,4 |
| barato, avara | **3**/159 | 8,6 |
| caro, derrochadora | **37**/159 | 34,9 |
| barato, derrochadora | **9**/159 | 29,9 |

El brazo caro choca contra el presupuesto **8× más seguido** que el
barato con la misma configuración. Eso es exactamente el régimen que la
hipótesis necesita, y es una afirmación de mecanismo: se verifica
contando filas `[WallClock]`, sin estadística.

### 4.2 La interacción es direccional pero NO separable del ruido

| | efecto de derrochar |
|---|---|
| a precio caro | **−3,1 pp** |
| a precio barato | **+2,5 pp** |
| **interacción** | **+5,7 pp**, IC95% bootstrap pareado **[+0,0, +10,2]** |

El signo es el que la hipótesis predice: derrochar **daña** cuando la
ronda es cara y **ayuda** cuando es barata. Pero el intervalo toca cero
exactamente, y hay una lectura más severa que la desautoriza del todo.

**La misma interacción, estimada en cada réplica por separado:**

| réplica | interacción |
|---|---|
| 1 | +9,4 pp |
| 2 | **−0,0 pp** |
| 3 | +7,5 pp |

Media +5,7 pp, **rango 9,4 pp, sd 5,0 pp**. Es decir: la dispersión entre
réplicas idénticas **es mayor que el efecto**, y una de las tres réplicas
da cero exacto. Con 2 grados de libertad el IC de la media es
[−6,7, +18,1].

**Piso de ruido del régimen** (§ 1 del análisis, el primer entregable
declarado): 7-9 tareas inestables de 53 por brazo, 8-12 celdas
discordantes entre réplicas. Es más ruidoso que el piso medido para el
régimen de rondas fijas (`docs/noise-floor-2026-07-26.md`), como el
diseño anticipaba: acá el tiempo binariza.

### 4.3 Veredicto

**Se materializó el asesino #2 del pre-registro**, textualmente: *"34
tareas no alcanzan para una interacción. A 2,9 pp por ítem, un efecto de
interacción realista puede quedar dentro del ruido."* Subimos la
resolución a 1,9 pp con 53 ítems y **el efecto sigue adentro del ruido**.

Lo que el piloto establece:

1. **El instrumento funciona** y la manipulación de precio es fuerte
   (4,4× por ronda, 8× en frecuencia de corte). No hace falta gastar la
   iteración declarada de combinar instrumentos A y B.
2. **La interacción no es medible con este poder.** Direccionalmente
   consistente en 2 de 3 réplicas, nula en la tercera.
3. La afirmación de mecanismo sobrevive; la de pass rate no — que es
   exactamente la distinción metodológica del piso de ruido del proyecto.

**El factorial completo NO se corre como está diseñado.** Antes hay que
decidir si se puede pagar el poder que necesita, y esa es una pregunta
de recursos, no de hipótesis.

### 4.4 Un defecto del instrumento, encontrado por los datos

8 filas de 636 (1,3%) salieron `[Timeout]` pese a un backstop de 600 s
contra un presupuesto de 30 s. Todas tienen **`rounds` de 0 o 1** y
exactamente 600 s de reloj: es **una sola ronda desbocada**. El corte en
el borde de la ronda no puede acotar una ronda que no termina — con
`BRAZE_MAX_TOKENS=12288` y generación en CPU a ~6 tok/s, una sola ronda
puede pasar de media hora.

La § 1.1 documenta que el presupuesto está cuantizado por la duración de
la ronda; esto muestra que el caso peor no es "presupuesto + una ronda"
sino "presupuesto + una ronda **no acotada**". Acotarlo requiere un
deadline a nivel de streaming, dentro de la ronda — anotado como trabajo
futuro, no hecho acá.

**Hecho el 2026-08-09**: `Engine::with_max_round_wall_clock`
(`--round-wall-clock-secs`, `BRAZE_MAX_ROUND_WALL_CLOCK_SECS`) — deadline
por ronda aplicado sobre cada espera del stream (request incluido), fila
`[RoundWallClock]` con las rondas completadas intactas. El LocalBackend
además chequea `tx.is_closed()` por token generado Y por chunk de
prefill: sin eso, verificado en vivo, la generación seguía quemando CPU
decenas de segundos tras el corte (el canal analysis de Harmony
suprimido y el prefill no intentan ningún `send`). La granularidad de
cancelación queda acotada por una llamada FFI (carga del modelo, un
chunk de prefill de 2048, un token) — cancelarla adentro pediría el
abort callback de ggml, anotado como trabajo futuro.

Impacto en el resultado: las 8 filas se reparten 1/3/2/2 entre los cuatro
brazos (no sesgan) y tocan 4 tareas. Excluyéndolas, la interacción pasa
de +5,7 a **+4,8 pp** con IC **[−1,0, +9,4]** — o sea que la conclusión
no cambia, solo se vuelve más explícitamente nula.

## 5. Lo que este piloto NO puede decidir

Su primer entregable no es el efecto sino **cuánta varianza tienen las
réplicas idénticas bajo este régimen**, y hay una razón estructural para
esperar que sea alta:

- El piso de ruido medido dice que el walltime es **±30% entre corridas
  idénticas** en modelos chicos (`docs/noise-floor-2026-07-26.md`).
- La propia `discriminating.toml` documenta en su encabezado que un tope
  de tiempo que muerde **binariza ruido continuo de reloj y lo
  amplifica** — con 300 s un banco oscilaba 7/4/3; con 900 s, 6/6/6.

Este diseño hace que el tiempo sea el presupuesto que muerde, a
propósito. O sea que **hereda esa amplificación por construcción**. El
análisis (`docs/round-economics-analysis-2026-08-08.py`) reporta el piso
de ruido del régimen ANTES que los descriptivos y que la interacción, y
esa es la lectura correcta: una interacción es más chica que los efectos
principales que la componen, así que se lee contra el ruido, nunca contra
cero.
