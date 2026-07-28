# Hipótesis: el eje no es la escala del modelo, es el precio de una ronda

Fecha: 2026-07-28
Estado: `proposed` — nada corrido todavía
Línea: paper3-round-economics

## El hueco

La curva harness-vs-escala del Paper 1 se levantó en un régimen donde **una
ronda cuesta caro**: entre segundos y minutos por ronda, según modelo y nodo.
Todas las palancas que gastan rondas —la escalera de rescate, el loop de
reparación de schema, el reintento de ronda vacía, TTC por auto-consistencia,
best-of-n, la escalación lead/worker— se evaluaron contra ese precio, y varias
salieron neutras o negativas.

Pero el precio de una ronda no es una constante de la naturaleza. Es una
propiedad del despliegue, y varía en dos órdenes de magnitud entre un modelo
de 20B en CPU y uno sub-1B on-device con prefill acelerado. **Nadie ha medido
un loop agéntico en el régimen de rondas baratas**, y todas nuestras
conclusiones sobre esas palancas están condicionadas a un régimen que no
declaramos.

Lo hace concreto un dato del proyecto: el tope de tiempo por tarea convierte
ruido continuo de reloj en ruido binario de pass/fail. Con 300 s un mismo
banco oscilaba 7/4/3 entre corridas idénticas; con 900 s, 6/6/6
(`docs/operar-gpt-oss-2026-07-28.md` § 1). Si el presupuesto que muerde es de
tiempo y no de rondas, entonces abaratar la ronda no es una mejora de
ingeniería: **cambia qué configuración de harness es la correcta**.

## Pregunta

¿La curva harness-vs-escala es realmente una curva harness-vs-**precio de la
ronda**? Es decir: ¿lo que determina qué palancas conviene encender es la
capacidad del modelo, o cuánto cuesta reintentar?

## Hipótesis principal

Bajo un presupuesto fijo de **wall-clock** (no de rondas), la configuración
óptima de harness depende del precio de la ronda: a rondas caras gana un
harness **avaro** (pocos reintentos, topes ajustados, escalar temprano); a
rondas baratas gana un harness **derrochador** (reintentar mucho, TTC,
best-of-n). Formalmente, se predice una **interacción** entre precio de ronda
y configuración de palancas sobre el pass rate a wall-clock fijo.

Esto no es la observación trivial de que ir más rápido termina más tareas
antes del plazo. Eso es de primer orden y es obvio. La afirmación es de
segundo orden y es la que puede estar equivocada: que el **ranking** de
configuraciones se dé vuelta.

## Hipótesis nula

El pass rate a wall-clock fijo lo determina la capacidad del modelo. Abaratar
la ronda reescala el eje sin cambiar qué configuración gana: no hay
interacción, solo un efecto principal de velocidad.

## Hipótesis secundarias — las predicciones diferenciales

Son lo que hace falsable a todo esto. Si las tres se cumplen, hay paper; si la
2 falla, la manipulación era "más cómputo" y no hay tesis.

1. **Las palancas que gastan rondas ganan** cuando la ronda se abarata:
   escalera de rescate, loop de reparación, reintento de ronda vacía, TTC,
   best-of-n.
2. **Las palancas que gastan contexto NO ganan**: plan inyectado, inventario
   de tools, memoria de proyecto. Su costo es por token, no por ronda, así que
   el precio de la ronda no debería moverlas. *Esta es la predicción que
   discrimina*: si suben igual que las del grupo 1, lo único que hicimos fue
   darle más cómputo al modelo y la tesis es falsa.
3. **La mitad negativa**: las clases de fallo que son **brecha de capacidad**
   y no estocásticas no mejoran con ninguna cantidad de rondas — y con rondas
   baratas **empeoran**, porque el loop gira más tiempo antes de que alguien
   note que no va a converger.

## La mitad negativa merece su propia sección

La medimos el 2026-07-28 (`docs/roam-metrics-memoria-2026-07-28.md` § 7):
`gpt-oss:20b` no puede emitir `U+1D62`. Con el carácter **nombrado y exhibido
delante** por el diagnóstico nuevo de `edit_file`, reintentó y volvió a
comérselo. Ninguna cantidad de rondas arregla eso: no es varianza, es una
capacidad ausente.

En el régimen de rondas caras esa distinción se paga sola —el turno se muere
contra el tope de 20 rondas en 25 minutos y alguien lo mira—. En el régimen de
rondas baratas, el mismo fallo gira miles de veces sin costo aparente. **Las
rondas baratas no hacen menos importante distinguir la clase de fallo: la
hacen urgente.**

De ahí sale la implicación de diseño, que es lo que le da dientes al paper:
un harness en régimen de rondas baratas necesita **clasificar en qué fallo
está**, porque "reintentar" es gratis para una clase e infinito para la otra.
El diagnóstico de primera divergencia con codepoint (`b1325fa`) es la primera
instancia de esa clasificación, y ya se midió que convierte un deadlock ciego
de 20 rondas en un abandono honesto en 4.

## La manipulación: cómo variar el precio de la ronda sin tocar la capacidad

El problema de diseño es que casi todo lo que abarata una ronda también cambia
el modelo. Tres instrumentos, de más limpio a menos:

| # | Instrumento | Capacidad | Rango | Rol |
|---|---|---|---|---|
| A | **Decoding especulativo** con draft certificado | **exactamente** constante — el stream draftado ES el stream greedy, verificable | ~2× | manipulación principal |
| B | **Mismo modelo, GPU vs CPU** en Nitro (greedy, misma seed) | idéntica: mismos pesos, mismos tokens | ~3-4× | manipulación principal |
| C | Niveles de cuantización del mismo modelo (INT4/INT8/FP16) | **confundida** — cambia costo y capacidad | grande | solo robustez |

A y B son los buenos porque bajo decodificación greedy **producen la misma
secuencia de tokens a distinto precio**. Eso es una manipulación causal del
costo con la capacidad fijada por construcción, que es raro poder hacer.

Sobre A: la propiedad existe y es verificable — Neutrino publica un
certificado de 27.648 tokens draftados contra greedy con cero divergencias
(§ "Neutrino", conversación 2026-07-28), y `llama.cpp` soporta decoding
especulativo nativamente, que es la vía sin dependencias nuevas.

C queda como chequeo de robustez y, de paso, como instrumento para la mitad
negativa: la predicción es que la cuantización agresiva degrada la fidelidad
de **emisión** más que la de comprensión, que es exactamente la clase de fallo
de la § anterior.

## El brazo de contexto necesitaba una palanca que funcione

La predicción diferencial 2 —"las palancas de contexto no ganan"— tiene un
defecto que hay que reparar antes de correr nada: el brazo de contexto estaba
armado con plan inyectado, inventario de tools y memoria de proyecto, y el
propio proyecto ya midió que **esas palancas son neutras o dañinas**. El plan
en prosa en el rol assistant degenera en respuestas vacías en los dos extremos
de la escala; `enable_project_memory` no tiene A/B que la valide.

Si el brazo de contexto está hecho de palancas que no funcionan, "no ganan
cuando la ronda se abarata" sale cierto **trivialmente**, y la predicción que
discrimina la tesis deja de discriminar nada. Hace falta una palanca de
contexto que sí funcione, para que su no-ganancia signifique algo.

### La candidata: el diseño de cuatro fases, escrito por un humano

Sale de *"Why Software Factories Fail: or: harness engineering is not enough"*
(Dex, HumanLayer, 2026 —
`github.com/humanlayer/advanced-context-engineering-for-coding-agents`,
`wsff.md`). Prescribe cuatro fases **antes** de que el agente escriba código:
revisión de producto, arquitectura de sistema, diseño de programa (call
stacks, árbol de archivos, firmas de tipos) y cortes verticales. Es una
palanca de contexto, y es el extremo del eje que braze nunca midió: el
proyecto se fue hacia planes **autogenerados** y encontró un acantilado de
formato de entrega; esto es el otro extremo, **humano y máximamente
estructurado**.

**Estatus evidencial de la fuente, declarado de entrada**: es un post de un
vendor —HumanLayer vende infraestructura de agentes—, sin revisión por pares,
y sus cifras propias ("30 minutos de planificación ahorran horas de revisión",
"2-3× más rápido", "~40% de las tareas de un solo tiro") vienen sin método. Se
toma **el argumento como hipótesis**, nunca las cifras como evidencia. Que la
receta circule sin medición es precisamente lo que hace que medirla valga la
pena.

Su tesis de fondo —*"no amount of harness engineering or loopsmaxxing can
solve what is fundamentally a model-training issue"*— es además el
contrapunto directo al encuadre de este proyecto, y tiene una instancia
concreta en nuestros propios datos: el modelo no pudo emitir `U+1D62` ni con
el carácter nombrado delante (§ "La mitad negativa"). Pero colapsa una
distinción que esa misma medición separa: el harness no puede **crear**
capacidad ausente, y sí puede volverla **legible y barata** — 25 minutos de
deadlock ciego con daño silencioso contra 7 de rechazo honesto sin daño. La
erosión que él describe es invisible, y la invisibilidad es propiedad del
harness, no del modelo.

La pregunta que abre es limpia y es previa a todo lo demás de este documento:

> ¿Un documento de diseño de cuatro fases escrito por un humano se comporta
> como la lista de tareas tipada (ayuda) o como el plan en prosa (daña)?

### Sub-hipótesis A — prerequisito, se corre primero

**A caro (régimen actual), un diseño de cuatro fases escrito por un humano
mejora el pass rate frente a no tener plan, y frente al plan autogenerado en
prosa que ya sabemos que daña.**

Brazos, todos a precio de ronda baseline:

| Brazo | Contenido |
|---|---|
| `sin-plan` | control |
| `prosa-auto` | plan generado por el planner, rol assistant (el negativo conocido) |
| `tasklist` | lista de tareas tipada (el estructurado que ya funciona) |
| `4fases` | documento humano: producto, arquitectura, diseño de programa, cortes verticales |

Si `4fases` no supera a `sin-plan`, **se cae y no entra al factorial** — y es
un resultado publicable por sí solo, porque contradice una receta que se está
difundiendo sin medición.

### Sub-hipótesis B — solo si A sobrevive

**El beneficio de `4fases` no cambia con el precio de la ronda**, porque su
costo es por token y no por ronda. Es la predicción diferencial 2, ahora con
una palanca que efectivamente hace algo. Si `4fases` ayuda a rondas caras
*y* gana todavía más a rondas baratas, la tesis del documento es falsa y hay
que decirlo.

### El problema serio: dónde se corre esto

`discriminating.toml` **no sirve** para A. Sus 34 tareas son chicas y de un
archivo; un documento de cuatro fases es para *features*, no para eso. El
propio autor lo dice: ~40% de las tareas van de un tiro, las medianas llevan
producto+arquitectura combinados, y las cuatro fases completas son solo para
trabajo grande.

O sea el tratamiento aplica exactamente donde `n` es más chico. Y peor: el
trabajo grande cae en el régimen donde el proyecto **ya midió que los
resultados no replican** — arriba de ~6 rondas, 55-71% de inestabilidad
(`docs/operar-gpt-oss-2026-07-28.md` § 3).

Hay que decirlo sin adornos: **esta sub-hipótesis vive en el régimen que
sabemos irreproducible.** Las tres salidas posibles, en orden de honestidad:

1. **Correrla sobre roam** como piloto explícito, con pocas tareas de tamaño
   feature y varias réplicas, aceptando poder estadístico bajo y reportándolo
   como piloto, no como medición. roam existe justamente porque es donde el
   harness se rompe y la suite no.
2. **Construir una suite de tareas tamaño-feature** con oráculo objetivo. Es
   caro y es un proyecto en sí mismo.
3. **Concluir que no es medible con el poder disponible** y publicarlo como
   limitación. Es un cierre legítimo, no un fracaso — y es información útil
   para cualquiera que quiera evaluar recetas de este tipo.

La decisión entre 1 y 3 se toma **después del piloto y no antes**, con el
criterio declarado abajo.

### Criterio de decisión de A, pre-registrado

- **Sigue a B** si `4fases` supera a `sin-plan` por un margen fuera del piso
  de ruido medido, en un piloto de al menos 3 réplicas sobre roam, *y* la
  varianza entre réplicas no excede la que el propio régimen de >6 rondas ya
  tiene documentada.
- **Se cierra como no-medible** si la varianza entre réplicas idénticas domina
  la diferencia entre brazos. Esa es la salida 3, y hay que tomarla sin
  intentar rescatarla con más corridas.
- **Sin iteración**: A no tiene reintentos declarados. Si el piloto no
  discrimina, no se re-especifica el tratamiento para que discrimine.

## Unidad experimental y arms

- **Suite**: `discriminating.toml` (34 tareas, 2.9 pp por ítem).
  `default.toml` está saturado —gpt-oss saca 57/57— y no puede detectar ni
  mejora ni regresión.
- **Unidad**: par (tarea, repetición) bajo un **presupuesto de wall-clock
  fijo** por tarea, no un tope de rondas. El tope de rondas se sube a un valor
  que no muerda, para que el presupuesto que binariza sea el de tiempo.
- **Diseño**: factorial precio-de-ronda × configuración-de-harness.

| Factor | Niveles |
|---|---|
| Precio de ronda | caro (baseline) / barato (instrumento A o B) |
| Configuración | avara / derrochadora / solo-contexto |

Donde *avara* = topes ajustados, sin TTC, sin best-of-n; *derrochadora* =
TTC + best-of-n + reintentos amplios; *solo-contexto* = la mejor palanca de
contexto disponible, sin ninguna palanca de ronda. La tercera es la que prueba
la hipótesis secundaria 2, y **qué se pone adentro lo decide la sub-hipótesis
A**: si `4fases` sobrevive, es esa; si no, el brazo queda con `tasklist`, que
es lo mejor que el proyecto tiene medido, y la predicción diferencial se lee
con esa debilidad declarada.

## Métricas y estadística

Se reusa la maquinaria del Paper 1, sin inventar nada:

- **pass^k** (tau-bench) como métrica de confiabilidad, no solo pass rate.
- **McNemar exacto** sobre pares tarea-repetición, **Holm** entre arms.
- **Newcombe** para los intervalos de diferencia de proporciones.
- El objetivo estadístico es el **término de interacción**, no un efecto
  principal. Es más difícil de detectar y hay que decirlo de entrada.
- **Antes de interpretar cualquier cosa**: `docs/noise-floor-2026-07-26.md`.
  Es regla del proyecto y acá pesa más que nunca, porque una interacción es
  más chica que los efectos principales que la componen.

## Criterios de decisión, pre-registrados

Se declara antes de correr, y se respeta aunque incomode:

- **Adoptar la tesis** si el término de interacción queda fuera del piso de
  ruido medido en **al menos dos de los tres instrumentos** (A, B, C), *y* la
  predicción diferencial 2 se sostiene direccionalmente (las palancas de
  contexto no ganan, o ganan menos de la mitad que las de ronda).
- **Rechazar** si la configuración derrochadora no supera a la avara en el
  brazo barato, o si las palancas de contexto ganan tanto como las de ronda.
- **Una sola iteración permitida**, declarada de antemano: si el brazo barato
  no alcanza suficiente separación de wall-clock, se permite ampliar el rango
  combinando A y B (especulativo *sobre* GPU contra denso sobre CPU). Nada
  más.

## Qué mataría el paper

Con honestidad, porque las tres son plausibles:

0. **El brazo de contexto se queda sin palanca que funcione.** Si `4fases` no
   supera a `sin-plan` (sub-hipótesis A) y el brazo cae de vuelta en
   `tasklist`, la predicción diferencial 2 pierde fuerza: "no ganan" se vuelve
   difícil de distinguir de "no hacían nada". No mata el paper, pero obliga a
   declarar esa debilidad en Threats en vez de venderla como confirmación.
1. **La predicción diferencial falla** (las palancas de contexto ganan igual).
   Entonces la manipulación fue "más cómputo" y no hay tesis. Es el riesgo
   más probable.
2. **34 tareas no alcanzan** para una interacción. A 2,9 pp por ítem, un
   efecto de interacción realista puede quedar dentro del ruido. Si pasa, el
   paper necesita una suite más grande de la que el proyecto puede correr, y
   eso es un cierre honesto, no un fracaso.
3. **El piso de capacidad domina todo**. Si la mayoría de los fallos del brazo
   barato son brechas de capacidad y no estocásticos, el paper colapsa en el
   de métodos sobre fidelidad de emisión — que quizás sea el mejor resultado
   posible, pero es otro paper.

## Factibilidad hoy

**Todo lo necesario existe en Nitro.** El teléfono es la *motivación*, no un
requisito: los instrumentos A y B corren sobre el LocalBackend actual, con
`llama.cpp` que ya está linkeado. No hay que comprar hardware, ni adoptar el
runtime de ningún vendor, ni tocar el backend.

Explícitamente: **no bloquear esta línea esperando hardware móvil.** Si sale,
el despliegue on-device es el trabajo siguiente y la motivación de la
discusión; si no sale, no se gastó nada en fierros.

Lo único que falta construir es el presupuesto de wall-clock por tarea como
condición de corte de primera clase del bench (hoy el tope de tiempo existe
pero se comporta como error de infraestructura, no como brazo experimental).

## Relación con las otras líneas

- **Paper 1** (harness-vs-escala): esta es su secuela natural y reusa su
  maquinaria completa. La contribución se enuncia como *"la curva del Paper 1
  está condicionada a un régimen de costo que no declaramos, y acá lo
  declaramos y lo variamos"*. No lo contradice; lo acota.
- **Paper de métodos**: la mitad negativa de acá (§ 3) es su contenido
  central. Si esta línea corre, probablemente **absorbe** al paper de métodos
  en vez de competir con él, y queda un solo paper más fuerte en vez de dos
  flacos.
- **Prioridad**: sigue siendo terminar y mandar el **Paper 1**. Esto se diseña
  ahora porque el diseño es barato y el momento de escribirlo es mientras la
  evidencia está fresca — no porque convenga empezarlo antes.

## Próximo paso concreto, si se decide correr

Dos pilotos independientes que pueden correr en cualquier orden, y ninguno
compromete el factorial:

1. **Piloto de costo** (la línea principal): presupuesto de wall-clock por
   tarea como *brazo* del bench, no como timeout de infraestructura. Después
   una celda: `qwen2.5:3b` GPU vs CPU en `discriminating.toml`, avara vs
   derrochadora, 3 repeticiones. Solo para ver si la separación de wall-clock
   alcanza y si la interacción es siquiera visible.
2. **Piloto de contexto** (sub-hipótesis A): `sin-plan` vs `4fases` sobre
   roam, pocas tareas tamaño-feature, 3+ réplicas. Su primer entregable no es
   el efecto sino **si el régimen permite medir algo** — la varianza entre
   réplicas idénticas es el dato que decide entre seguir y cerrar.
3. Recién con los dos, el factorial — y con el brazo de contexto poblado por
   lo que haya ganado en 2.

El piloto 2 tiene una virtud aparte: **es barato y su resultado negativo
también sirve.** Si un diseño humano de cuatro fases no mueve la aguja sobre
un repo real, eso contradice una receta que hoy circula sin medición, y vale
publicarlo aunque el resto de la línea no salga.
