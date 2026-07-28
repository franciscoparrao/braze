# Memoria de proyecto encendida × subdivisión de `roam-core` — 2026-07-28

**Qué se probó**: el flujo que el doc operativo describe —*un `braze run` por
paso, con `enable_project_memory` encendida*— ejecutándolo de verdad sobre las
tres extracciones de módulo que le faltaban a `roam-core`: `metrics` (§ 1-6),
y después `kde` y `csv` (§ 7).

**Qué rindió**, en orden creciente de importancia:

1. El flujo de memoria **funciona y quedó verificado end-to-end** (§ 1).
2. La receta de tres pasos se reduce a **dos** (§ 2).
3. El costo del paso de borrado no es el tamaño del bloque sino cuánto tiene
   que buscarlo el modelo (§ 3).
4. El tic de alucinar tools —catalogado hasta hoy como inofensivo— fue el
   mayor consumidor individual del presupuesto de rondas en la corrida que
   falló (§ 4).
5. **Hay caracteres que el modelo entiende perfectamente y no puede
   escribir** (§ 7). Eso corrompe cualquier copia sin que ningún gate lo cace,
   y —peor— vuelve esa región del archivo *estructuralmente ineditable* por el
   agente, porque `edit_file` matchea por texto exacto. Es el hallazgo nuevo
   del día y el que reordena todo lo anterior.

Modelo: `gpt-oss:20b` por LocalBackend (camino Harmony) en Nitro, auto-fit de
capas, `BRAZE_OLLAMA_NUM_CTX=32768`, `BRAZE_MAX_TOKENS=12288`,
`BRAZE_ENABLE_PROJECT_MEMORY=true`. Resultado en roam: commits `93f29c6` y
`c6b8069`, subdivisión completa (`point`, `mcp`, `metrics`, `kde`, `csv`),
`lib.rs` de 469 a 277 líneas, 14/14 tests verdes.

---

## 1. La memoria cruza las sesiones — verificado, no supuesto

Cada paso es un proceso `braze run` distinto, con contexto que nace vacío.
Entre pasos, lo único que sobrevive es `.braze/memory.json`:

| Corrida | Duró | Qué dejó en la memoria |
|---|---|---|
| Paso 1 (crear `metrics.rs`) | 5m47s | `metrics.rs` / `write_file` |
| Paso 2 (fallido) | 16m51s | `lib.rs` / `edit_file` |
| Paso 2b (borrado) | 2m00s | `lib.rs` / `edit_file` (dedup por ruta) |

La pregunta que importaba no es si el archivo se escribe —eso se ve— sino si
llega al modelo. Se resolvió con un probe: un cuarto `braze run` pidiendo
**sin usar herramientas ni leer archivos** la lista de archivos que su propio
contexto dijera que fueron tocados antes. Respondió en 32s:

```
roam-core/src/lib.rs
roam-core/src/metrics.rs
```

Exactamente el contenido de `.braze/memory.json`. La sección se inyecta y el
modelo la lee.

**Alcance real de lo que recuerda**: solo `touched_files`. `completed_signals`
se alimenta de `AgentEvent::TaskCompleted`, que viene de la lista de tareas
tipada, y `enable_task_list` también está apagada por default — así que el
flujo con memoria, encendido solo, registra qué se tocó y nada más. Es lo que
el doc operativo ya decía; ahora está medido.

## 2. La receta baja de tres pasos a dos

El ejercicio del 26-jul dejó la secuencia *crear → quitar-y-enlazar → arreglar
la compilación*. Acá el paso 3 **no hizo falta**, y no por suerte: el paso 1
pedía explícitamente el único cambio de visibilidad que el refactor necesitaba
(`fn haversine` → `pub(crate) fn haversine`, porque los tests de `lib.rs` la
llaman como `Trajectory::haversine`). Plegar ese cambio río arriba eliminó el
error de compilación antes de que existiera.

En el ejercicio anterior ese mismo cambio de visibilidad fue justamente el
contenido del paso 3. La regla que se desprende: **el paso 3 no es una etapa
de la receta, es la deuda que deja no anticipar la visibilidad en el paso 1.**

## 3. El paso de borrado tiene una condición de tamaño

El primer intento del paso 2 pidió, en un solo prompt, borrar las cinco
funciones y agregar el `mod metrics;`. **Falló**: 20 rondas sin converger,
16m51s, y dejó `lib.rs` a medias (agregó el `mod`, borró una de las cinco
funciones). Anatomía de las 20 rondas:

| Rondas | En qué se fueron |
|---|---|
| 2 | ediciones productivas (`mod metrics;` y borrar `haversine`) |
| 7 | lecturas exploratorias de `lib.rs` (dos de ellas cayeron en el caché de llamada repetida) |
| 5 | llamadas a una tool `search` **que no existe** |
| 3 | ediciones rechazadas (`old_string` truncado; una edición no-op cazada por la guarda de abreviación) |
| 2 | una ronda vacía + el `grep` que finalmente sirvió |
| 1 | ronda final, cortada por el tope |

**Productividad: 2 de 20 rondas.** El resto fue el modelo intentando
reconstruir en contexto un archivo de 469 líneas que ya no le cabía en el
presupuesto de rondas.

El re-intento (paso 2b) cambió una sola cosa: **el bloque exacto a borrar iba
embebido en el prompt**, con la instrucción de no leer el archivo y hacer una
única llamada a `edit_file` con `new_string` vacío. Salió en **2m00s, a la
primera**, y el diff fue exactamente el pedido.

Lo que esto corrige del doc operativo: el problema del paso 2 no es cuántas
líneas se borran (40 líneas salieron sin drama), es **cuántas búsquedas tiene
que hacer el modelo para encontrarlas**. Si el llamador ya sabe qué bloque hay
que borrar —y en un refactor siempre lo sabe, porque acaba de copiarlo— pasarlo
verbatim convierte una tarea de 20 rondas en una de 2.

## 4. `search`: el tic dejó de ser inofensivo

El doc lo listaba como "tic recurrente sin consecuencia grave: alucina nombres
de tools (`search`)". En la corrida fallida se llevó **5 de 20 rondas**: cuatro
respondidas con `Unknown tool 'search'. Available tools are: ...` y una quinta
bloqueada por la guarda de llamada repetida. Recién en la ronda 18 usó `grep`,
que es la tool real y encontró lo que buscaba de inmediato.

Frecuencia sobre las 12 sesiones más recientes de Nitro: **11 llamadas a tools
inexistentes**, `search` ×7 y `read...` ×4 — este último con el nombre de la
tool truncado con puntos suspensivos, el mismo tic que ya estaba documentado
para las rutas, pero aplicado al nombre de la herramienta. 5 de 12 sesiones lo
exhiben.

La consecuencia es de presupuesto, no de corrección: el harness responde bien y
el modelo termina recuperándose, pero cada intento cuesta una ronda de las ~6
que el modelo tiene de margen útil. Candidato barato: alias `search` → `grep`.

## 5. Corrupción silenciosa: cero esta vez

El paso 1 copió 51 líneas. `diff` contra el original: **idéntico byte a byte**,
salvo el `pub(crate)` autorizado. Sobrevivieron los `φ`, `Δφ`, `Δλ` del
haversine y todos los strings.

Contraste con el ejercicio del 26-jul, donde el mismo modelo copiando **216**
líneas corrompió dos cosas en silencio (comillas borradas dentro de un
`format!`, subíndices borrados de una fórmula en un doc comment, dejándola
matemáticamente falsa). Esa salida seguía sin commitear en el checkout de
Nitro; se descartó al preparar este ejercicio, después de confirmar por diff
que era la versión corrupta.

En su momento leí esto como que el tamaño del bloque era la variable (51
líneas limpias vs. 216 corrompidas). **Las extracciones de `kde` y `csv`
—§ 7— lo refutaron: la variable es el carácter, no el tamaño.** El bloque de
`metrics` salió limpio porque no contenía ninguno de los caracteres que el
modelo no puede emitir.

**La regla no cambia: verificar con el diff, nunca con el resumen.** Acá el
resumen del paso 2 fallido decía cosas razonables mientras `lib.rs` quedaba a
medias, y solo el `git diff` lo mostró.

## 6. Hallazgos menores de la memoria de proyecto

- `TouchedFile::at` está documentado como *"RFC3339-ish timestamp string"*
  (`crates/braze-memory/src/memory.rs:25`) pero su único productor,
  `ProjectMemoryHook::now` (`crates/braze-engine/src/project_memory_hook.rs:174`),
  escribe epoch Unix crudo (`"1785218110"`). El campo no se renderiza, así que
  no tiene efecto visible — pero el doc comment miente.
- `MemoryMeta::updated_at` (`memory.rs:62`) queda `null` después de cada save
  exitoso; nada en el workspace lo escribe.
- `.braze/` debería ir al `.gitignore` del proyecto anfitrión: es un archivo
  que escribe el propio agente, y el render ya lo trata como datos no
  confiables. Agregado en roam en el mismo commit.

## 7. `kde` y `csv`: el hallazgo que da vuelta la sección 5

Se completaron las dos extracciones que faltaban (commit `c6b8069` en roam:
`lib.rs` 417 → 277 líneas). Rindieron un mecanismo que no estaba descrito en
ninguna parte y que reordena todo lo anterior.

### 8.1 Hay caracteres que el modelo entiende y no puede escribir

`gpt-oss:20b` **no puede emitir `ᵢ` (U+1D62)**. Lo demostró dos veces, con dos
días de distancia y dos prompts distintos:

| Fecha | Bloque | Qué pasó |
|---|---|---|
| 26-jul | 216 líneas | `Σᵢ … (x-xᵢ)² + (y-yᵢ)²` → `Σ … (x-x)² + (y-y)²` |
| 28-jul | 89 líneas | idéntico, **pese a que el prompt advertía explícitamente** *"sin cambiar símbolos matemáticos con subíndices o superíndices"* |

No es "no-ASCII": sobrevivieron `φ`, `Δφ`, `Δλ`, `²`, `×` y la propia `Σ`. Es
esa clase de carácter. Después apareció una segunda instancia, `≈` (U+2248), y
una tercera de otra naturaleza: el anidamiento de comillas simples dentro de un
string de `format!` (`"cannot read '{}': {}"` → `"cannot read '{}'": {}`),
también replicada 2 de 2 veces.

La corrupción del doc comment **no la caza ningún gate**: un comentario es
invisible para `cargo check`, para los tests y para el post-edit check, por
construcción. La fórmula quedó diciendo que la densidad se calcula con la
distancia de cada celda a sí misma.

### 8.2 La consecuencia de segundo orden es peor que la corrupción

`edit_file` matchea por texto exacto. Por lo tanto **una región que contiene un
carácter que el modelo no puede emitir es estructuralmente ineditable por el
agente.**

Se le pidió borrar el bloque de `kde` de `lib.rs` con el texto exacto embebido
en el prompt —la receta corregida de § 3, la que había funcionado en 2m00s—.
El modelo construyó un `old_string` de 91 líneas **correcto en todas menos esa
una**. Resultado: 6 intentos, las 20 rondas del turno, **25m8s, cero avance**.
Grepeó, confirmó que la línea existía, releyó el archivo entero, y nunca dedujo
la causa — porque el mensaje de error (`old_string not found`) no dice *qué*
carácter está mal.

Esto invalida parcialmente la corrección de § 3: embeber el bloque exacto en el
prompt resuelve el costo de *buscar*, pero no ayuda en nada cuando el problema
es de *emisión*. El bloque estaba textual en el prompt y aun así salió mal.

### 8.3 El fallback a `write_file` es la peor rama

En `csv`, tras fallar el `edit_file`, el modelo razonó (el canal analysis se
filtró al stdout) que si no podía repetir la llamada, reescribiría el archivo
completo. Lo hizo: 268 líneas. Daño del rescate:

| # | Daño | ¿Lo caza algún gate? |
|---|---|---|
| 1 | `assert!((x-e).abs() < 1e-6)` → `assert_eq!(x, e)` en dos tests | **no** — viola la convención #2 de roam, que existe por un bug real |
| 2 | `≈` borrado en tres comentarios de la derivación del `href` | **no** |
| 3 | newline final del archivo borrado | **no** |
| 4 | `mod csv;` nunca se agregó | **sí, ruidoso** — los tests llaman `from_csv` y no compilan |

**Verificado, no supuesto**: se reconstruyó el `lib.rs` exacto desde el rollout
de la sesión y se corrió. Tal cual quedó, **no compila** (3 errores `E0599`, no
existe `from_csv`). Reparando solo el daño #4, los otros tres pasan **14/14**.

O sea: el único daño que forzó el descubrimiento fue incidental. Los dos que
importan —una convención de tests desactivada y una fórmula anotada a mano
mutilada— habrían viajado en verde. Los `assert_eq!` pasan hoy porque los
valores son bit-idénticos; es exactamente la trampa que la convención #2 de
roam describe.

**Regla operativa**: si `edit_file` falla dos veces sobre el mismo bloque, la
salida correcta es abortar y escalar a un humano, no reescribir el archivo. La
redacción actual de la guarda de encogimiento sugiere lo contrario (*"If unsure
of the exact current text, prefer write_file with the complete updated
content"*), y el modelo la citó textualmente para justificar la rama que causó
el daño.

### 8.4 Dos hallazgos sueltos de harness

- **`braze run` se cuelga indefinidamente en un prompt de permiso.** La corrida
  de `csv` pidió aprobación para `shell_exec` (`run bash -lc cargo test`,
  ¿Permitir? [y/N]) en un contexto sin TTY: quedó bloqueada esperando un stdin
  que no iba a llegar nunca. Un `run` one-shot no interactivo debería denegar
  por default o fallar rápido, no colgarse. (De paso: el prompt decía "no
  ejecutes comandos de shell" y lo intentó igual.)
- **El canal `analysis` de Harmony se filtró a stdout** en las dos corridas
  fallidas, con el razonamiento crudo del modelo. Útil para depurar, pero no es
  lo que se espera de `--output-format plain`.

### 8.5 Qué se hizo con el código

Ambos borrados terminaron ejecutados a mano y verificados línea a línea contra
`HEAD`: `csv.rs` idéntico byte a byte (52 líneas), `kde.rs` idéntico salvo un
cambio deliberado — el subíndice `ᵢ` normalizado a ASCII (`Σ_i`, `x_i`, `y_i`).
Era la única ocurrencia de U+1D62 en el repo. Se normalizó porque un carácter
que el modelo no puede escribir convierte esa región en no editable por un
agente para siempre; el significado de la fórmula no cambia.

## 8. Los cinco arreglos, y la verificación en vivo del principal

### 8.1 Nombrar el carácter que falló

`edit_file` ahora, cuando no matchea, alinea `old_string` contra el archivo y
reporta la **primera divergencia con su codepoint en los dos lados**, más
ambas líneas. Reemplaza al hint anterior de "línea más parecida", que anclaba
en la *primera* línea de `old_string` y por eso no decía nada cuando esa línea
estaba bien.

**Verificado en vivo** contra el caso exacto que lo motivó (mismo bloque, mismo
carácter, mismo modelo):

| | Antes | Después |
|---|---|---|
| Rondas | 20 (agotó el tope) | **4** |
| Tiempo | 25m08s | **7m31s** |
| Desenlace | reescritura de 268 líneas con daño silencioso | **"I can't perform that edit"** y se detuvo |
| Daño al archivo | 3 regresiones, 2 invisibles | **ninguno** |

Ronda 1, el mensaje: *"First difference: line 29, column 51 of old_string (line
32 of the file) — the file has U+1D62 ('ᵢ') where old_string has U+0020
(space)"*. Ronda 2 releyó el archivo. Ronda 3 reintentó y **volvió a comerse el
`ᵢ`** — tercera replicación del fenómeno, esta vez con el carácter nombrado y
exhibido delante. Ronda 4 la bloqueó la guarda de repetición, y paró.

O sea: el arreglo no le enseña al modelo a emitir el carácter —eso no se puede
arreglar desde el harness— pero convierte un deadlock ciego de 25 minutos en un
fracaso honesto de 7, sin daño colateral y con la causa legible para el humano.
Que el modelo siga sin poder emitirlo **con la respuesta puesta delante** es la
confirmación más fuerte de que es una brecha de capacidad y no un descuido.

### 8.2 Los otros cuatro

- **La guarda de `write_file` ahora depende del tamaño del archivo.** Bajo 120
  líneas sigue ofreciendo la reescritura completa (la evidencia de Aider que la
  justificaba es de archivos chicos); encima, dice explícitamente que no lo
  haga, y por qué: retipear lo que nadie pidió tocar es donde el daño no lo
  caza ningún gate. El modelo citaba textualmente la redacción vieja para
  justificar la rama que rompió los tests.
- **`braze run` ya no se cuelga sin TTY.** El default de seguridad era correcto
  (denegar en EOF) pero nunca se alcanzaba: un stdin heredado y abierto no
  entrega EOF nunca. Ahora se chequea si hay terminal antes de leer, en el
  prompt de permisos y en `ask_user`. Es un chequeo y no un timeout, a
  propósito: debe fallar cerrado de inmediato.
- **`search` → `grep` se sugiere.** No era un typo sino un sinónimo, así que
  ninguna cota de distancia de edición podía cubrirlo; hace falta tabla. Se
  **sugiere**, no se remapea en silencio: los schemas de argumentos difieren, y
  adivinar mal ejecuta la cosa equivocada en vez de devolver un error
  corregible. De paso, se le sacan los puntos suspensivos al nombre antes de
  matchear, con lo que `read...` se resuelve solo.
- **Canal `analysis`: trazado, no cambiado.** `Channel::Unknown` se trata como
  visible a propósito, y está bien: invertirlo haría que un modelo sin header
  de canal produzca turnos mudos, que es peor que una filtración. Sin el header
  crudo no se puede distinguir "header no reconocido" de "el modelo puso su
  análisis en `final`", así que se agregó la traza que dejaría eso respondido
  la próxima vez, en vez de cambiar el default a ciegas.

## 9. Qué queda

**De la línea de trabajo:**

- `enable_project_memory` **sigue apagada por default**. Este ejercicio prueba
  que el flujo funciona, no que la palanca mejore un resultado — para eso está
  su propio A/B (`+ablate:project-memory`), que no se corrió.
- Medir la asimetría comprensión/emisión como tal, en vez de dejarla como
  observación de caso: barrido de clases de carácter (subíndices, superíndices,
  flechas, emoji, CJK, comillas anidadas) × modelos, midiendo tasa de
  round-trip verbatim. Es barato, es un fenómeno nuevo, y es exactamente el
  tipo de resultado que el paper de métodos podría cargar.
- La subdivisión de `roam-core` quedó **completa** (point, mcp, metrics, kde,
  csv). Deuda propia de roam que queda: la tarea #2 (`hello()` y el
  `use std::time::{SystemTime, UNIX_EPOCH}` sin usar que arrastra).

**En el paper**: § Discussion tiene el párrafo nuevo sobre comprensión vs.
emisión, contrastado con `zhu2026babeltele`. La entrada del `.bib` está
**pendiente de `/verify-refs`**.

**Del harness**: los cinco arreglos de § 8 están en main. Queda el interlock
duro —bloquear `write_file` sobre un archivo que acaba de fallar `edit_file`
dos veces— que necesita estado por turno en el engine y es una decisión de
diseño más grande que la redacción de la guarda.
