# A/B constrained decoding vs escalera de rescate — veredicto

Fecha: 2026-07-12
Contexto: diseño pre-registrado en `docs/constrained-decoding-ab-design.md`
(origen: revisión de `JustVugg/colibri` #48). Mecanismo implementado el
mismo día (ver `PLAN.md` § "Prompt-tools + constrained decoding"): modo
`+ablate:prompt-tools` (brazo B, addendum de system prompt + envelope
JSON parseado como canal primario) y `+ablate:constrained-tools` (brazo
C, B + `format` de Ollama forzando el schema del envelope). 3 executors
× 3 brazos + fila exploratoria `gemma3:1b` (solo B/C) × 19 tareas × 5
reps = 1.045 corridas, seed 42, temp 0.2, Nitro, `--no-ollama-stop`.
Estado: **CERRADO — dispara la cláusula de iteración pre-registrada, no
adopta ni rechaza en los términos estrictos del pre-registro.** Datos:
`docs/sweep-constrained-decoding-2026-07-12.json`/`.log`.

## Resultados

| Executor | A: baseline | B: prompt-tools | C: constrained-tools |
|---|---|---|---|
| llama3.2:1b | 17.9% [11.5,26.8] | 25.3% [17.6,34.8] | 30.5% [22.2,40.4] |
| gemma4:e2b | 82.1% [73.2,88.5] | 20.0% [13.2,29.1] | 71.6% [61.8,79.7] |
| qwen2.5:3b (control) | 65.3% [55.3,74.1] | 25.3% [17.6,34.8] | 42.1% [32.7,52.2] |
| gemma3:1b (unlock, fuera de criterio) | — (HTTP 400, no soporta tools nativo) | 18.9% [12.3,28.0] | 17.9% [11.5,26.8] |

Deltas C−A y C−B con intervalo Newcombe 95% sobre el delta within-sweep:

- **llama3.2:1b**: C−A = +12.6pp, Newcombe95% **[−0.3, +25.1]** — cruza
  cero, no es "fuera del ruido" pese a que el punto estimado supera el
  umbral de +10pp. C−B = +5.3pp [−8.2, +18.5], también cruza cero.
- **gemma4:e2b**: C−A = **−10.5pp** [−23.3, +1.8] — dirección negativa,
  el brazo constrained no supera al baseline nativo en el driver diario.
  C−B = +51.6pp [+38.6, +64.2] — el constraint recupera casi toda la
  pérdida de B, pero desde un piso catastrófico.
- **qwen2.5:3b (control)**: C−A = **−23.2pp** [−37.6, −8.8], CI
  enteramente negativo — el format tax predicho para el control tuneado
  se confirma limpio, sin ambigüedad.

Verificación del mecanismo (`schema_fail + rescues ≈ 0` en C, la firma
que el diseño exige para atribuir la ganancia al constraint y no a otra
cosa): **rescues = 0 en las 11 filas** (el envelope nunca cuenta como
rescate, confirmando que el cambio del engine funciona como se diseñó),
pero **`schema_validation_failures` NO es ≈0 en C**: 99 en llama3.2:1b,
31 en gemma4:e2b, 71 en qwen2.5:3b. El constraint fuerza la sintaxis del
*envelope* (`action`/`name`/`arguments` bien formados) pero el campo
`arguments` sigue siendo `{"type": "object"}` genérico — un modelo puede
satisfacer el schema forzado y aun así rellenar `arguments` con una
forma que no matchea el schema real de la tool específica. Esto es
exactamente la fuga que el diseño anticipó como candidata única de
iteración (ver abajo).

## Veredicto contra el criterio pre-registrado

Ningún brazo cumple **Adoptar** (`C−A ≥ +10pp fuera del ruido` Y `C−B>0`
Y mecanismo verifica, en al menos un débil): llama3.2:1b tiene el signo
correcto pero el CI cruza cero y el mecanismo no verifica
(`schema_fail=99`); gemma4:e2b tiene signo negativo.

Tampoco cumple **Rechazar** en su letra estricta (`C≤A en ambos débiles`,
o `C>A pero C≤B`): llama3.2:1b tiene C>A y C>B simultáneamente, así que
ninguna de las dos subcláusulas de rechazo lo captura limpiamente —
aunque el punto estimado sea positivo, el pre-registro exige *fuera del
ruido* para contar como señal, y no lo está.

Esto dispara la cláusula de **Iterar UNA vez**: el modo de falla
dominante es identificable (`schema_validation_failures` concentrado
justo donde se predijo) y es exactamente la candidata única
pre-declarada — el envelope con `arguments: {"type": "object"}` genérico
deja pasar argumentos mal formados por tool; la iteración pre-registrada
es reemplazarlo por un `oneOf` por tool usando su `input_schema` real.
**No se ejecutó en esta pasada** — mecanismo M adicional (extender
`build_envelope_format` a `oneOf` por tool + engine para rutear al
sub-schema correcto + tests) y un segundo sweep de ~1h de Nitro; queda
como el único ítem de iteración legítimo, a gastar cuando corresponda.

## Lecturas honestas

1. **El modo de falla dominante de los débiles NO es puramente
   sintáctico** — la hipótesis alternativa pre-declarada del diseño se
   confirma parcialmente: `gemma4:e2b` en modo B (prompt-tools sin
   constraint) colapsa a 20.0% desde un baseline nativo de 82.1%, una
   caída de −62pp por el simple cambio de modalidad (perder el campo
   `tools` nativo de Ollama), antes de que el constraint entre en juego.
   El constraint (C) recupera la mayor parte de esa caída (+51.6pp sobre
   B) pero no todo el terreno perdido — el "format tax" de perder la
   plantilla nativa de tool-calling es más caro que lo que el
   constraint puede reponer en este modelo.
2. **`llama3.2:1b` es el único executor donde el constraint efectivamente
   ayuda en dirección** (+12.6pp sobre el baseline que Ollama sirve
   nativo, con schema_fail alto en A también: 114 — el genérico ya
   fallaba mucho ahí), consistente con la intuición original del diseño
   para el executor MÁS débil — pero el n=95 no alcanza para separarlo
   del ruido, y el mecanismo verificado (rescues=0, schema_fail alto)
   dice que la ganancia no es "sintaxis resuelta", sino otra cosa que
   este sweep no aísla (posiblemente: el addendum de prompt-tools mismo
   es una guía más explícita que el schema nativo permisivo que Ollama
   ve para este modelo específicamente — hipótesis para la iteración).
3. **`gemma3:1b`, el bonus de unlock, corre pero no rinde**: HTTP 400 en
   nativo se resuelve (18.9%/17.9% en B/C), confirmando el desbloqueo de
   API — pero el modelo de 1B genérico sin fine-tune de function-calling
   no compone con el mecanismo lo suficiente para ser útil; se reporta
   como demostración de la palanca, no como resultado adoptable (estaba
   fuera del criterio desde el diseño).
4. **El control (`qwen2.5:3b`) confirma limpiamente el format tax**
   pre-declarado: −23.2pp con CI enteramente negativo. Un modelo ya
   fine-tuneado para tool-calling nativo pierde, no gana, al forzarlo a
   un canal de prompt+JSON — exactamente la predicción "expectativa
   pre-declarada para el control" del diseño.

## Iteración: `oneOf` por tool — RECHAZA, no rescata

Fecha: 2026-07-12 (mismo día, sesión siguiente). La única iteración
pre-declarada se implementó y corrió: `build_envelope_format` pasa de
`arguments: {"type": "object"}` genérico a un `oneOf` con una variante
por tool (`name` fijado por `const`, `arguments` con el `input_schema`
real de esa tool). 4 filas × 19 tareas × 5 reps = 380 corridas, mismo
seed (42), mismo binario salvo el cambio de schema, Nitro,
`--no-ollama-stop`. Datos:
`docs/sweep-constrained-decoding-iteration-2026-07-12.json`/`.log`.
Smoke previo a n=19 (1 rep) confirmó el mecanismo antes de gastar el
sweep completo: `schema_fail` pasó de 99 (brazo C original,
llama3.2:1b) a **0**.

### El mecanismo verifica perfecto — y el pass rate empeora

| Executor | A: nativo | C original (genérico) | C iterado (oneOf/tool) |
|---|---|---|---|
| llama3.2:1b | 17.9% | 30.5% | **16.8%** [11.1,26.7] |
| gemma4:e2b | 82.1% | 71.6% | **53.7%** [45.5,65.9] |
| qwen2.5:3b (control) | 65.3% | 42.1% | **24.2%** [17.4,35.1] |

`schema_validation_failures` agregado en el brazo iterado: **0** en
llama3.2:1b y qwen2.5:3b, **9** en gemma4:e2b (vs. 99/71/31 en el brazo
original) — la firma que el pre-registro exige para atribuir el efecto
al constraint (`schema_fail + rescues ≈ 0`) por fin se cumple, limpio,
en las tres filas. Pero el pass rate no sube: **baja**, y con
intervalos Newcombe 95% que ya no cruzan cero en ningún caso:

- llama3.2:1b: iterado − original = **−13.7pp** [−26.0, −0.8]; iterado
  − nativo = −1.1pp [−12.5, +10.5] (ya ni siquiera el signo positivo
  del brazo original sobrevive).
- gemma4:e2b: iterado − original = **−17.9pp** [−32.1, −3.9]; iterado
  − nativo = **−28.4pp** [−42.0, −15.5].
- qwen2.5:3b (control): iterado − original = **−17.9pp** [−31.5,
  −4.0]; iterado − nativo = **−41.1pp** [−54.4, −27.4] — el format tax
  del control, ya negativo en el diseño original, se duplica.

`tool_execution_failures` explica dónde cayó el pass rate:
69/95 (llama3.2:1b), 53/95 (gemma4:e2b), 55/95 (qwen2.5:3b) — muy por
encima del brazo original (7/19/24 respectivamente). El desglose por
tarea en llama3.2:1b lo hace concreto: `glob_basic` y `grep_basic`
pasaban **100%** en el brazo original (schema genérico) y caen a
**0%** en el iterado; `shell_exec_basic` 60%→20%, `write_file_basic`
20%→0%. El fix elimina la sintaxis rota del sobre, pero fuerza al
decoder a comprometerse con el `oneOf`/tool y el schema exacto de esa
tool en un solo paso — y esa restricción más estricta empeora, no
mejora, la ejecución real. Ninguna tarea que fallaba en el brazo
original por `schema_fail` pasó a convertirse en un PASS en el
iterado; se convirtió en un `exec_fail` o en la pérdida de tareas que
antes SÍ pasaban.

### Veredicto final contra el criterio pre-registrado

**RECHAZAR**, ahora sin ambigüedad: `C ≤ A` se cumple en los dos
débiles con la iteración puesta (llama3.2:1b prácticamente empata con
nativo dentro del ruido, ya no lo supera; gemma4:e2b cae muy por
debajo). La cláusula de iteración ya se gastó — el pre-registro no
contempla una segunda — así que este es el cierre: **la capa de
harness (rescate textual) sigue siendo el tradeoff correcto; tener
acceso al decoder no lo cambia**, ni con el schema laxo original ni
con el schema estricto de la iteración. El mecanismo que se creía
limitante (schema genérico dejando pasar argumentos mal formados) no
era el cuello de botella real — arreglarlo no liberó ninguna ganancia
oculta, sugiriendo que el modo de falla dominante de estos modelos es
genuinamente semántico (eligen mal, no escriben mal), consistente con
la hipótesis alternativa que el diseño pre-declaró desde el inicio.

## Disposición para el paper

Entra a la discusión (§ ablations / discusión del espectro
inferencia-vs-harness) como un **negativo diagnosticado bajo
pre-registro, con su única iteración corrida y también negativa** —
el ciclo completo (diseño → mecanismo → sweep → iteración → veredicto)
queda cerrado y citable de punta a punta, sin cabos sueltos. A
diferencia del A/B del planner (que SÍ se rescató), este es el caso
simétrico: la disciplina de pre-registro evitó tanto adoptar un
resultado ruidoso como forzar una segunda iteración sin base. Las dos
tablas de este documento (original + iteración) y sus lecturas son
citables tal cual.

## Implicación para `docs/local-backend-stencil-design.md`

Ese documento gateaba la decisión de construir `LocalBackend` + un
sampler con masking de logits (`stencil`) exactamente a este
resultado, con una salvedad: solo contemplaba los dos casos limpios
("ayuda" → construir; "domina el format tax" → reconsiderar). El
resultado real pasó primero por la zona ambigua (iteración) y terminó
en el segundo caso, ahora sin ambigüedad y habiendo probado la versión
MÁS estricta de constraint que `stencil` ofrecería (schema real por
tool, no solo el sobre). Un sampler in-process con masking de logits
sería, si acaso, una versión aún más estricta que el `oneOf`/tool que
acaba de empeorar el resultado — no hay señal en estos datos de que
subir el rigor del constraint compre algo. **Recomendación: no
perseguir `LocalBackend`/`stencil` sobre esta base.** Si en el futuro
se retoma, debería ser motivado por la capacidad del modelo
(offloading de MoE, § Hardware de ese documento) y no por constrained
decoding — ese eje específico quedó cerrado, negativo, con dos
iteraciones de evidencia.
