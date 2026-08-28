# Pre-registro: A/B del gate de evidencia para cerrar tareas

Fecha: 2026-08-28
Estado: **proposed — con una precondición de viabilidad que hoy NO se
cumple.** Commiteado antes de correr nada.
Línea: palancas de confiabilidad para SLM. Implementación en
`1db3d70` (`+ablate:task-evidence`, off por default).

## Origen

El gate traduce los *checkers* de Recuris (arXiv:2608.24876 § 2.2.3):
`task_update(id, "done")` se rechaza salvo que una tool call del
registry haya terminado sin error desde el último `done` aceptado, de
modo que el estado dependa de lo que pasó en el entorno y no de lo que
el modelo afirma. El síntoma que ataca está documentado acá: v8 K-6
registró que un 3B re-marca `done` con frecuencia.

## LA PRECONDICIÓN, medida antes de diseñar nada

El gate solo puede actuar si el modelo **usa** las tools de la lista.
Antes de escribir el diseño se midió eso sobre el archivo completo de
sweeps del proyecto:

| | |
|---|---|
| Filas con `+ablate:task-list` encendida | **760** |
| Filas que llamaron alguna `task_*` | **17 (2,2 %)** |
| `task_update` emitidos en total | 34 |
| Backends que las usaron | uno solo: `qwen3.5-coder+plan:gemma4:e4b` |

De esas 17, **6 emitieron un `task_update` sin ninguna tool real
previa** — la clase que el gate interceptaría. Pero `tool_call_names`
no registra el *status* del update, así que no se puede distinguir un
`done` prematuro de un `in_progress` legítimo: 6 es una **cota
superior**, y el gate solo intercepta `done`.

**Conclusión de la precondición: el A/B no tiene poder.** Con una tasa
base de uso del 2,2 %, y sobre ella una fracción de `done` prematuros,
un sweep de 34 tareas × 3 seeds = 102 corridas dispararía el gate en
**menos de una**. No es que la palanca sea mala: es que **no hay
instrumento para medirla**, igual que `sc-compaction` resultó no medible
en gpt-oss por floor (`hypothesis-2026-08-13-sc-retention`).

Lanzar el sweep igual serían ~48 h de Nitro para un nulo garantizado por
construcción, y ese nulo se leería como "el gate no sirve" cuando lo que
mediría es "la lista no se usa".

## El hallazgo mayor que hay detrás

La palanca `task-list` (C′.2) **está esencialmente inerte**: 760 filas,
2,2 % de uso, un solo backend. Su module doc dice que "se promueve solo
si el A/B pre-registrado del planner la valida", y ese A/B nunca
concluyó a favor. Los datos sugieren que el problema no es que la lista
no ayude sino que **los modelos no la invocan**.

Eso convierte al gate de evidencia en una palanca montada sobre otra que
no funciona, y hace que la pregunta interesante cambie de orden.

## Pregunta, reordenada

**Q0 (previa, barata):** ¿por qué los modelos no usan las task tools, y
se puede subir esa tasa sin romper nada?

**Q1 (la original, bloqueada por Q0):** dado que el modelo usa la lista,
¿el gate de evidencia mejora el pass rate, o solo agrega fricción?

## Diseño de Q0 — el experimento que sí se puede correr

| | |
|---|---|
| Suite | `default.toml` restringida a las familias `multi_step` y `error_recovery` (donde las 17 filas históricas se concentran) |
| Modelos | `gpt-oss:20b` (LocalBackend/Harmony) y `ornith:9b` — los dos que saturan `default.toml`, para que el techo no confunda |
| Brazo **U0** | `+ablate:task-list` (estado actual) |
| Brazo **U1** | `+ablate:task-list` con las descripciones de las dos tools reescritas para pedir uso explícito |
| Brazo **U2** | `+ablate:task-list` sembrada por planner (`+plan:`), que ya siembra sin que el modelo llame `task_add` |
| Seeds | 42, 43 |
| Métrica primaria | **tasa de uso**: fracción de corridas con al menos un `task_update` |
| Métrica secundaria | pass rate (para detectar que subir el uso no dañe) |

**Costo declarado**: ~3 brazos × 2 seeds × ~15 tareas. A ~60 s/tarea en
ornith y ~420 s en gpt-oss local, entre 3 y 12 h según el executor. El
recorte permitido, declarado acá: correr **solo ornith:9b** (más rápido)
si el tiempo apremia; nunca eliminar el brazo U0.

### Criterio de desbloqueo, pre-registrado

**Q1 se lanza solo si algún brazo alcanza ≥ 30 % de tasa de uso.** Por
debajo de eso, un A/B del gate sobre 34×3 corridas seguiría sin poder,
y el número sale de aritmética simple: con 30 % de uso y ~35 % de
`update` sin trabajo previo, un sweep de 102 corridas dispararía el gate
en ~10, que es el mínimo para que un McNemar exacto pueda alcanzar
p < 0,05 (con 10 discordantes todos en una dirección, p = 0,002).

Si ningún brazo llega al 30 %, el resultado es **"la lista no se usa y
el gate no es medible"**, se reporta como tal, y la palanca queda
experimental/OFF — el mismo desenlace que `sc-route`.

## Diseño de Q1 — congelado ahora, ejecutable solo tras Q0

| | |
|---|---|
| Suite | la que Q0 haya mostrado con mayor tasa de uso |
| Brazo **A** | `+ablate:task-list` (lista sin gate) |
| Brazo **B** | `+ablate:task-list;task-evidence` |
| Brazo **E** | **A/A**: brazo A repetido (piso de ruido in-sweep) |
| Seeds | 42, 43, 44 |
| Orden | round-robin A-42, B-42, E-42, A-43, … (lección del incidente KV-quant) |

Se congela ahora para que no se elija a la vista de Q0.

## Hipótesis y priors honestos

- **H0 (viabilidad)**: alguna intervención sube la tasa de uso por
  encima del 30 %. *Prior: incierto.* Que un solo backend de 760 filas
  las use sugiere que el problema puede ser de capacidad, no de
  redacción — y ahí U1 no ayudaría.
- **H1 (el gate ayuda)**: B supera a A fuera del piso. *Prior: débil a
  favor.* El mecanismo es real y está documentado (K-6), pero el gate
  también puede trabar corridas legítimas — ver riesgos.
- **H2 (el gate no daña)**: B no cae fuera del piso. *Prior: es la que
  más me preocupa*, y por eso es criterio de rechazo propio.

## Métricas

Primaria: pass rate dual (`passed`, `passed_strict`), McNemar exacto
pareado por (tarea, seed) para B−A contra el piso A/E.

Secundarias, todas ya instrumentadas: rondas, tokens, `[RouteMiss]`,
`wall_time_ms`, y —específica de esta palanca— **cuántas veces disparó
el gate**, contable desde los tool results de error del `task_update`.
Sin esa cuenta, un nulo no distingue "no ayudó" de "no disparó".

## Criterios de decisión, pre-registrados

1. **Viabilidad primero**: sin el 30 % de Q0, Q1 no se lanza y se
   reporta la no-medibilidad.
2. **Piso primero**: la discordancia A/E define el piso y el MDE.
   Ningún contraste B−A se interpreta por debajo.
3. **Adoptar** si B supera a A fuera del piso **y** el conteo de
   disparos del gate es > 0 (un efecto sin mecanismo observable no se
   adopta, aunque sea significativo).
4. **Rechazar por fricción** si B cae fuera del piso — el modo de falla
   esperable: un modelo que resuelve algo sin tools (una respuesta
   directa, un `no_tool`) queda trabado sin merecerlo.
5. **Nulo dentro del piso con disparos > 0**: el gate actúa y no cambia
   el resultado. Se reporta y la palanca queda OFF. Es un nulo
   informativo: significa que los `done` prematuros no estaban
   costando tareas.
6. **Sin iteración de tratamiento.** Un solo diseño del gate. Si
   decepciona, la salida es un pre-registro nuevo.
7. Fallos de infraestructura fuera del denominador; > 10 % invalida.
8. **Gate anti-copias (L-9)**: si las corridas de un brazo salen
   idénticas entre seeds, aplica la cláusula de instrumento
   (`BRAZE_LOCAL_TEMP>0`) antes de leer nada.

## Riesgos anotados

- **El riesgo central es que el gate trabe trabajo legítimo.** Una
  tarea resuelta sin tool calls —lectura directa, respuesta de
  conocimiento— no puede cerrar su entrada de lista. El diseño actual
  lo acepta a propósito (la evidencia es de ejecución, no de logro),
  pero si el criterio 4 dispara, esa es la causa a mirar primero.
- **La evidencia se consume, y eso puede ser demasiado estricto**: si
  el modelo hace una tool call que resuelve DOS tareas de la lista, la
  segunda queda sin avalar. Es deliberado (impide cerrar la lista de un
  tirón) pero es una asimetría, y el conteo de disparos permitirá ver
  si muerde.
- **Q0 puede subir el uso a costa del pass rate**: pedir explícitamente
  que use las tools gasta tokens de instrucción en un SLM. Por eso el
  pass rate es secundaria de Q0 y no un adorno.
- **Confusión uso↔capacidad**: que un solo backend use las tools
  sugiere que puede ser un límite de capacidad. Si U1 y U2 fallan
  ambos, esa es la lectura, y hay que decirla en vez de insistir.

## Registro

Este documento se commitea y **pushea antes de lanzar Q0**, de modo que
el orden sea verificable:
`git log --diff-filter=A --format='%ad' -- <este archivo>` debe ser
anterior a la fecha de los JSON de resultados.

Los números de la precondición se reproducen sobre el archivo de sweeps
de `nitro:~/braze/docs/` con el snippet documentado en la sección de
la precondición (conteo sobre `tool_call_names` de las filas cuyo
`backend` contiene `task-list`).

---

## Resultados de Q0 y veredicto (2026-08-28)

Corrido en Nitro bajo `tmux`, 21 minutos. Recorte declarado aplicado:
solo `ornith:9b` — `gpt-oss:20b` (~12 GB) no cabía en los 5,7 GiB
disponibles con la sesión gráfica abierta. El brazo U0 está completo.
Datos: `nitro:~/q0-{U0,U1,U2}-s{42,43}.json`.

### Tasa de uso

| brazo | corridas | usaron `task_*` | tasa | `task_update` | passed |
|---|---|---|---|---|---|
| **U0** estado actual | 12 | 0 | **0 %** | 0 | 12/12 |
| **U1** descripciones insistentes | 12 | 0 | **0 %** | 0 | 12/12 |
| **U2** sembrada por planner | 12 | 10 | **83 %** | 26 | 12/12 |

**U1 queda refutado.** Reescribir las descripciones para pedir el uso
—"REQUIRED FIRST STEP…", "REQUIRED after finishing each step…"— no movió
la aguja ni una corrida. La hipótesis de que el 2,2 % histórico fuera un
problema de redacción **no se sostiene**, al menos en este executor.

**U2 desbloquea el criterio**: 83 % supera holgadamente el 30 %
pre-registrado. Cuando el planner siembra la lista, el modelo sí la
actualiza (26 `task_update` en 10 corridas).

### Pero Q1 NO se lanza, por una razón que Q0 no buscaba

De las **10 corridas de U2 con `task_update`, CERO** lo emitieron sin
una tool real previa. Todas tienen entre 1 y 4 herramientas ejecutadas
antes del primer update:

```
multi_step_read_count_write   ['read_file', 'write_file', 'task_update', …]
error_recovery_wrong_filename ['glob', 'shell_exec', 'shell_exec', 'shell_exec', 'task_update']
multi_step_sum_two_files      ['read_file', 'read_file', 'write_file', 'task_update', …]
```

**El gate de evidencia no habría disparado ni una vez.** El modo de
falla que ataca —v8 K-6, "un 3B re-marca `done` con frecuencia"— no se
manifiesta en `ornith:9b`: el modelo trabaja primero y marca después,
que es exactamente el orden correcto.

Esto lo decide el **criterio 3 del propio diseño de Q1**, escrito antes
de correr nada: *"no se adopta un efecto sin mecanismo observable
aunque sea significativo — el conteo de disparos del gate tiene que ser
> 0"*. Q0 muestra de antemano que ese conteo sería 0. Correr Q1 sería
gastar horas para confirmar lo que ya se sabe.

Hay que decir con claridad qué anticipó el pre-registro y qué no: el
criterio de desbloqueo (30 % de uso) **se cumplió**, y la razón para no
lanzar Q1 es otra que Q0 no fue diseñado para buscar. No estaba
previsto; apareció.

### La tensión estructural que esto destapa

Los dos hechos juntos son incómodos y valen más que la palanca:

1. El modelo que **comete** el error (un 3B, K-6) **no usa** la lista:
   0 % en U0 y U1, y 2,2 % en 760 filas históricas.
2. El modelo que **usa** la lista (ornith con planner, 83 %) **no
   comete** el error: 0 de 10 updates sin evidencia.

El gate de evidencia está bien implementado y resuelve un problema que,
donde se puede medir, no ocurre. No es un fallo del gate: es que la
población donde el mecanismo aplica y la población donde el instrumento
funciona **no se solapan** con los executores disponibles.

### Caveat de la suite

Los tres brazos dan 12/12. La suite Q0 —familias `multi_step` y
`error_recovery` de `default.toml`— está **saturada** para `ornith:9b`.
Para Q0 no importa (su métrica primaria es tasa de uso, no pass rate),
pero invalida esta suite para cualquier Q1 futuro: sin espacio hacia
arriba no hay mejora que detectar. Elegirla fue un error de diseño mío
al escribir el pre-registro, que nombraba a ornith como "de los que
saturan `default.toml`" y aun así tomó tareas de ahí.

### Veredicto

**Q1 NO se lanza. La palanca `task-evidence` queda implementada y
OFF**, con el mismo desenlace que `sc-route`: mecanismo razonable, sin
condiciones para medirlo.

Lo que haría falta para reabrirla, y ninguna es barata:

- Un executor que use la lista **y** cometa el error. No hay candidato
  entre los disponibles; habría que buscarlo, no suponerlo.
- Una suite no saturada para ese executor.
- O bien: aceptar que el valor del gate es de **seguro** (impide una
  clase de falla) y no de mejora medible, y decidir si eso justifica
  mantener la palanca. Es una decisión de diseño, no experimental, y
  como tal no se resuelve con un sweep.

### Lo que Q0 sí aporta, independiente del gate

- **La palanca `task-list` no es inerte por redacción sino por
  invocación**: con planner llega al 83 %. Si alguna vez se quiere
  validar C′.2, el brazo tiene que ser `+plan:` — U0 solo mide que el
  modelo no la descubre.
- **Un dato negativo limpio sobre prompt engineering**: hacer las
  descripciones de una tool más imperativas no cambió nada. Es el tipo
  de intervención que se asume efectiva y acá midió cero.
