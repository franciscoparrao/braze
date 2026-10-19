# Workbook — Paper 3 completo · "A Threshold Calibrated Once"
> Este archivo NO es el manuscrito. Contiene diagnóstico generado por IA (marcado).
> El manuscrito vive en: `paper3/main.tex` + `paper3/{introduction,related-work,methods,results,discussion,threats}.tex`

## 0. Estado del footprint (lo primero que hay que saber)

Todo el texto actual del manuscrito (las seis secciones, el abstract y la conclusión)
lo redactó el modelo directamente en los `.tex`, sesión del 2026-10-17/18. Bajo la
doctrina de `/paper-prose`, ese texto equivale a un **borrador de referencia
pegado**, no a prosa del autor. Hay dos caminos honestos:

- **A. Reconstruir** las secciones con el protocolo leer-cerrar-reconstruir (§2 de
  abajo): el autor lee una sección, la cierra y la reescribe sin mirarla. Después usa
  este diagnóstico sobre su versión. La declaración pasa a "asistencia estructural; la
  redacción es del autor".
- **B. Mantener** el texto y declarar el footprint real: "borrador generado por IA a
  partir del diseño, los datos y los análisis del autor, revisado y editado por el
  autor". Es el mismo nivel que ya declara el Paper 2 ("drafting and editing manuscript
  text from the author's study designs…"). La declaración actual de `main.tex` está en
  esa línea, pero hay que confirmar que siga siendo exacta tras la revisión.

El diagnóstico de §3 sirve igual en ambos caminos. En A se aplica sobre la
reconstrucción; en B, sobre el texto actual.

## 1. Scaffold de argumento
No solicitado (el texto ya existe). Si se elige el camino A, se genera por sección.

## 2. Referencia watermarkeada (⚠️ NO PEGAR)
El borrador de referencia **es** el manuscrito actual (ver §0). No se duplica aquí.

## 3. Log de diagnóstico — iter 1 (2026-10-18)

Convención: `[tipo] sección/párrafo.oración — qué pasa (sin reescritura)`.
Las marcas **[ERROR]** no son de estilo sino de exactitud: el texto dice algo que los
datos o las otras secciones contradicen. Van primero.

### 3.1 Errores de exactitud (prioridad 1)
- **[ERROR] methods §2.1 p2.o2** — "an earlier version of our analysis that included them
  *overstated the noise by 162 comparisons*". Hay dos problemas: (a) 162 es un número de
  comparaciones, no una magnitud de ruido; (b) contradice a threats §Exclusions, donde
  esa versión dio tasas "within a point" de las reportadas. Además es leakage de proceso.
- **[ERROR] related-work §Scope.o1** — "small local models (3--20B) … Rust coding suites
  graded by cargo check". Contradice a methods §2.1: el rango es 1B–27B, hay tres
  modelos alojados y seis suites (una de BFCL y una de compactación, que no son de
  coding en Rust).
- **[ERROR] results §4.2 p1.o3** — "14 incumbents whose base *never flipped* across its
  repetitions". δ_H0 = 0 significa que el número de tareas aprobadas fue igual en todas
  las réplicas de H0, no que ninguna tarea volteó: dos volteos opuestos se cancelan.
- **[ERROR] conclusion.o2** — "In our archive *that calibration* is not a property of the
  benchmark". Lo que no es propiedad del benchmark es el *ruido*, no la calibración. Es
  un término cambiado que invierte el sentido de la oración.

### 3.2 Hedge miscalibrado (prioridad 2)
- **[hedge] intro p6.o3** — "a threshold calibrated once is miscalibrated for *most of the
  decisions* it makes". Los datos muestran que 29/90 incumbentes son más ruidosos que la
  base, y una FAR de 6,9 % contra 0,8 %. "Most" no está demostrado.
- **[hedge] intro p2.o3** — "the noise is large *where these loops are cheapest to run*".
  No hay evidencia citada de dónde es más barato correr estos loops. Además se repite
  como premisa en discussion §Noise.o1 ("Self-evolution is cheapest to run on small local
  models").
- **[hedge] related-work §Overfitting.o4** — "the instability these benchmarks report *is
  the aggregate symptom* of that per-decision error". Es una afirmación causal sobre
  trabajos ajenos que no se midió. discussion §Noise.últ lo dice mejor ("*can* come
  from"); los dos lugares no concuerdan.
- **[hedge] discussion §Noise.o5** — "Larger models … make a quiet base more likely and a
  noisy incumbent no less likely". Sale de tres bandas de tamaño (17 brazos en ≥20B) y
  sin test. El hedge debería corresponder a ese n.

### 3.3 Registro-IA (prioridad 3)
- **[ai-register: meta-caracterización] intro p6.o1** — "The scope of these claims is
  *deliberate*." Califica la elección en vez de mostrarla.
- **[ai-register: meta-caracterización] related-work §Gates.o3** — "Its experiments are,
  *notably*, on small models". Si se borra "notably" no se pierde información.
- **[ai-register: fórmula] related-work §Gates p2.o3** — "That is the gap this paper
  fills." Es una frase-molde de posicionamiento.
- **[ai-register: fórmula] intro p4.o2** — "to our knowledge, has not been measured". Es
  aceptable solo si el /paper-gaps sostiene la ausencia; hoy la sostiene en parte.
- **[ai-register: abstracción] related-work §Noise.o3** — "a source *orthogonal* to ours".
  Es "orthogonal" en sentido figurado; el paréntesis siguiente ya da el referente concreto.
- **[ai-register: metáfora repetida]** — "a bet on the noise of…" aparece en intro p4,
  related-work §Scope, results §4.2 y discussion §Re-derive. Cuatro veces delata
  plantilla. Hay que elegir un lugar.
- **[ai-register: metáfora] intro p2.o4** — "answering a coin toss on a sizeable
  fraction of its decisions". Es vago ("sizeable") y la metáfora no tiene anclaje
  numérico.

### 3.4 Término inconsistente (prioridad 3)
- **tolerance / threshold / margin / δ / δ̂** se usan como sinónimos parciales. "Margin"
  es específico de HCL (en tareas) y "tolerance" de RRSI (fracción). Hay que fijar uno
  por concepto y declararlo en methods §2.4.
- **base / base harness / H0 / first arm**: se alternan sin regla.
- **incumbent / configuration / arm**: "arm" es la unidad del archivo e "incumbent" el
  papel en la simulación. En results §4.1 se mezclan ("149 arms" junto a "90
  incumbents"). Basta con una frase en methods que diga cuál es cuál.
- **false acceptance / false positive**: related-work §Harness-opt usa "false positive", y
  el resto, "false acceptance". ("false commits" en el párrafo de PACE es el término de
  PACE y está bien.)

### 3.5 Densidad y transición (prioridad 4)
- **[densidad] related-work §Gates.o2** (PACE): una oración con cuatro ideas (test, umbral,
  garantía, auditoría).
- **[densidad] related-work §Gates.o5** (RRSI): seis cláusulas encadenadas con comas y
  punto y coma.
- **[densidad] related-work §Noise.o5** (Noise Floor + Wiedmann + Hardy): tres trabajos en
  una oración con punto y coma.
- **[transición] intro p2.o1** — "Two recent observations make *the question* pressing".
  La pregunta todavía no se planteó (llega en p4). El lector no sabe a qué "question" se
  refiere.
- **[transición] results §4.2 p3** — los "Three robustness checks" llegan después de la
  tabla, que ya muestra las filas de esos checks. Conviene ordenar tabla → lectura de la
  fila principal → checks.

### 3.6 Leakage de sesión
- methods §2.1 p2.o2 (ver ERROR arriba).
- threats §Exclusions.o2 — "An earlier exploratory version included them". Aquí sí es un
  punto metodológico deliberado (las exclusiones son post hoc respecto del primer
  análisis); se queda, siempre que diga lo mismo que methods.

## 4. Bitácora de footprint (alimenta `disclose`)
- spec: no (el texto se escribió sin scaffold previo).
- reference: **sí, pegado directamente en el manuscrito** (todas las secciones, abstract
  y conclusión, redactadas por el modelo el 2026-10-17/18).
- diagnose: 1 iteración (este log).
- edición de lenguaje del modelo: además del borrador, el modelo aplicó correcciones de
  exactitud y atribución tras /verify-refs (commit 0c87c64).
- Declaración que corresponde **hoy**: "AI-generated draft from the author's design,
  data and analyses, revised by the author" (camino B), salvo que se reconstruya
  (camino A).
