# Ejercicio roam × braze: subdividir `Trajectory` — 2026-07-26

**Qué se probó**: braze + gpt-oss:20b (LocalBackend, 25 capas GPU, KV en VRAM)
haciendo un refactor real sobre `roam-core`: mover `struct Trajectory` y sus
11 métodos (216 líneas) de `lib.rs` a un `trajectory.rs` nuevo.

**Por qué roam y no `default.toml`**: la suite está **saturada** — gpt-oss saca
57/57. Un banco donde el modelo ya rinde el máximo no puede detectar ni mejora
ni regresión. roam tiene repo real, archivos grandes, compilación real y
ediciones multi-archivo; es donde el harness se rompe.

Rindió: **cuatro tareas, ~66 min de cómputo, tres bugs de harness, una guarda
nueva y dos límites de dimensionamiento** — ninguno detectable con la suite.

## Cronología: cada intento descartó una causa distinta

| # | Falló por | Naturaleza |
|---|---|---|
| A1 | `NoKvCacheSlot` mataba el turno entero | **Bug** — arreglado |
| A2 | Tool call truncada por presupuesto (ctx 8k) | Dimensionamiento |
| A3 | Nudge de repetición negaba lo que el colapso ACI había borrado | **Bug de interacción** — arreglado |
| A4 | `edit_file` sin match exacto + guarda de encogimiento mal redactada | **ACI** — ajustado |
| B1 | `max_tokens=4096` cortaba la tool call en construcción | Dimensionamiento |
| B2 | ✅ `lib.rs` limpio (222 líneas), pero el crate no compilaba | Parcial |
| C | ✅ **Compila, 14/14 tests** | Éxito |

## Los tres bugs de harness

### 1. Quedarse sin contexto mataba el turno

`ctx.decode` devolvía `NoKvCacheSlot` a mitad de generación y el backend hacía
`bail!`, matando el turno. Pero eso no es un fallo del backend: es el contexto
lleno. Ahora cierra la ronda como `length`, que es lo que deja al engine ver un
turno truncado y compactar.

Además faltaba la guarda preventiva: solo se verificaba que el **prompt**
entrara en `n_ctx`, así que un prompt de `ctx_limit - 1` dejaba lugar para
generar UN token. Ahora el presupuesto se recorta al contexto disponible, con
aviso.

### 2. Dos palancas correctas que se trababan entre sí

La secuencia real que lo destapó:

```
1 read_file offset=1   → líneas 1-200 ✓
2 read_file offset=201 → líneas 201-400 ✓
4 read_file offset=401 → líneas 401-469 ✓
6 read_file offset=1   → BLOQUEADO por el nudge
8 grep                 → ✓ encuentra Trajectory en línea 18
9,10,11 read_file offset=1 → BLOQUEADO ×3 → abandona
```

- El **colapso ACI** reduce observaciones viejas a una línea salvo las últimas
  5 → para la llamada 9, el contenido de la lectura 1 ya no estaba en contexto.
- El **nudge de llamada repetida** respondía *"ya llamaste a esto, usá el
  resultado que ya tenés"*.

Pero **ya no lo tenía**: la primera palanca se lo borró y la segunda se negaba
a devolvérselo. El modelo quedó atrapado con el plan correcto en la mano.

**Arreglo**: `seen_calls` pasó de `HashSet` a mapa `(nombre, args) →
Option<contenido>`. La tool sigue sin re-ejecutarse —esa es la intención
anti-loop— pero la repetición se responde **con el resultado anterior**,
etiquetado como caché. `None` (dos idénticas en la misma ronda, sin resultado
todavía) mantiene el nudge original. Solo se cachean resultados exitosos.

### 3. Una redacción que inducía el error

La guarda de encogimiento de `write_file` decía *"retry this exact write_file
call with `allow_shrink`: true"*. El modelo reintentó la llamada **exacta**,
sin agregar el campo, y cayó en el guard de repetición.

**Arreglo**: imperativo, el campo como algo que hay que AGREGAR, y aviso de que
repetir igual vuelve a fallar.

## La guarda nueva: el verde en falso del post-edit check

En la tarea A el guardrail reportó **`cargo passed`** sobre un `trajectory.rs`
que tenía un `format!` con error de sintaxis. ¿Por qué? El archivo estaba
**huérfano**: ningún `mod` lo declaraba, así que el compilador nunca lo miró.

Es un agujero general y cae en el patrón MÁS común de subdividir código: crear
el archivo primero, enlazarlo después. El error viajó dos tareas y ~30 minutos.

**Arreglo**: `orphan_module_note` — si se escribe un `.rs` que ningún `mod`
declara, el resultado lleva una nota diciendo que el check **no lo compiló**.
Solo informa, nunca falla la edición, y se calla ante raíces (`lib.rs`,
`main.rs`, `mod.rs`, `build.rs`).

## Dimensionamiento: los defaults son de juguete

- `ollama_num_ctx = 8192` → el prompt llegaba a 6491 tokens dejando 1701 para
  generar; escribir 216 líneas de Rust no cabe.
- `max_tokens = 4096` → cortaba la tool call **mientras se construía**.

Con `BRAZE_OLLAMA_NUM_CTX=32768` + `BRAZE_MAX_TOKENS=12288` la tarea avanzó. El
auto-fit absorbió el contexto mayor sin ceder capas (gpt-oss sigue en 25 con KV
en VRAM a 32k), así que subirlo salió gratis.

## Capacidad de gpt-oss:20b: el tamaño de la edición es el cuello de botella

- **Ediciones masivas: mal.** Dos `edit_file` fallidos por no poder reproducir
  un `old_string` exacto de 216 líneas; una reescritura completa cortada por
  presupuesto.
- **Ediciones quirúrgicas: bien.** La tarea C —dos arreglos de una línea— salió
  en **157s** y a la primera.

**Consecuencia operativa**: darle el trabajo en trozos pequeños. La subdivisión
de roam debería reestructurarse en pasos de esa escala en vez de módulos
enteros.

### Reportó éxito en falso

En la tarea A el modelo afirmó *"removed the previous `struct Trajectory` block
and its `impl`"* — y `lib.rs` tenía el md5 **idéntico al baseline**. Para un
harness agéntico ese es el modo de falla peligroso: el resumen se lee como
éxito.

### Corrupción silenciosa por transcripción — el hallazgo que ningún gate caza

**Dos veces** en la misma sesión, copiando código "verbatim":

```rust
// 1. Sintaxis (la cazó el compilador, tarde)
format!("cannot read '{}'": {}, e)          // escrito
format!("cannot read '{}': {}", path, e)    // correcto

// 2. Un doc comment (NO la caza nadie)
/// ... Σᵢ exp(-0.5 * ((x-xᵢ)² + (y-yᵢ)²) / h²)   // original
/// ... Σ  exp(-0.5 * ((x-x)²  + (y-y)²)  / h²)   // resultado
```

Los subíndices `ᵢ` desaparecieron y la fórmula documentada quedó
**matemáticamente falsa**. Compila, pasa los 14 tests, pasa el post-edit check.

**La lección**: el compilador caza la corrupción de sintaxis; comentarios,
strings y documentación son exactamente donde sobrevive a todos los controles
automáticos. Un refactor "verbatim" de un modelo chico necesita **diff review**,
no solo verde de compilación. Es una palanca de harness que no existe todavía.

## Estado final

Refactor completo y funcionando: `lib.rs` con 222 líneas menos y el módulo
enlazado, `trajectory.rs` con los 11 métodos, crate compilando, 14/14 tests. El
resto de `lib.rs` idéntico al original. Las únicas desviaciones del "puro
movimiento" son las dos corrupciones de arriba más el `pub(crate)` de
`haversine`, que sí es necesario: mover código cruza frontera de módulo y eso
**no es neutral en visibilidad** — algo que ni el modelo ni los criterios de
juicio iniciales anticiparon.

## Nota de método

Los criterios de aceptación se fijaron ANTES de ver resultados (compila, 14
tests, diff de puro movimiento, alcance respetado). Falla de diseño detectada
sobre la marcha: en la tarea A **tres de los cuatro pasaron vacuamente** porque
no se hizo nada — estaban escritos para detectar *si rompió algo*, no *si hizo
algo*. A un criterio de aceptación le falta evidencia positiva de que el
trabajo ocurrió.
