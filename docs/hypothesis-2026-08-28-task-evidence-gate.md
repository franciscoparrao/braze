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
