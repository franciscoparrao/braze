# Operar gpt-oss:20b con braze — guía práctica

**Qué es esto**: qué se le puede pedir a gpt-oss:20b con expectativa
razonable de que salga bien, qué no, y cómo montar el trabajo para que la
diferencia importe. Todo lo de acá está **medido** entre el 2026-07-25 y el
28 sobre Nitro (RTX 3050 6GB), no estimado.

**Por qué existe**: la mitad de estas cosas se reaprendieron a golpes dos
veces en la misma semana.

---

## 1. Configuración: no es opcional

```bash
export LD_LIBRARY_PATH=$(ls -d ~/proyectos/braze/target/debug/build/llama-cpp-sys-2-*/out/build/bin | head -1)
export BRAZE_OLLAMA_NUM_CTX=32768     # el default de 8192 estrangula
export BRAZE_MAX_TOKENS=12288         # el default de 4096 corta tool calls
source ~/.cargo/env                   # sin esto el post-edit check se apaga EN SILENCIO
```

- **No fijes `BRAZE_LOCAL_GPU_LAYERS`.** El auto-fit elige 25 capas con KV en
  VRAM. El valor que se venía adivinando a mano era 8, y costaba 3.4× de
  velocidad.
- **`--task-timeout-secs 900`**, no 300. Un tope que muerde convierte ruido
  continuo de reloj en ruido binario de pass/fail: con 300s un mismo banco
  oscilaba 7/4/3 entre corridas idénticas; con 900s, 6/6/6.
- **`source ~/.cargo/env` importa más de lo que parece.** En un shell no
  interactivo `cargo` no está en el `PATH`, y entonces el post-edit check
  —el guardrail que le devuelve los errores de compilación al modelo— **se
  salta sin avisar**. El modelo trabaja sin red y el resultado parece
  incapacidad suya.

Con los defaults del proyecto, un refactor real muere por presupuesto, no por
capacidad. Eso se midió: el prompt llegaba a 6491 tokens dejando 1701 para
generar, y escribir 216 líneas de Rust no cabe.

---

## 2. Dónde es de fiar

Determinista en su banco base: **57/57 en `default.toml` con cero
discordancia** entre tres corridas idénticas. Y en la suite dura, cuatro
familias nunca oscilaron en tres réplicas:

| Tipo de tarea | Estabilidad | Costo típico |
|---|---|---|
| Lectura/consulta sobre código | 3/3 | segundos |
| Localizar y editar en archivo grande | 5/5 | ~1-2 min |
| Arreglar errores de compilación puntuales | 2/2 | ~1 min |
| Cambiar X sin tocar el Y parecido | 3/3 | ~1 min |

El caso más elocuente: **dos arreglos de una línea en un archivo de 469
líneas, 157 segundos, a la primera.**

---

## 3. Dónde no

| Falla | Evidencia | Señal |
|---|---|---|
| Trayectorias de más de ~6 rondas | 55-71% de inestabilidad | mismo prompt, distinto resultado |
| Movimiento masivo de código | falló mover 216 líneas, repetidamente | `edit_file` sin match exacto |
| Su propio reporte | afirmó haber borrado código con el archivo intacto | el resumen dice éxito |
| Copiar "verbatim" | corrompió 2 de 2 veces | **silenciosa** |

Sobre la última, que es la peligrosa: al copiar código que debía mover sin
cambios, rompió un `format!` (lo cazó el compilador, dos tareas después) y
**borró los subíndices de una fórmula en un doc comment**, dejándola
matemáticamente falsa. Eso compila, pasa los tests y pasa el post-edit check.

**Ningún gate automático detecta corrupción en comentarios, strings o
documentación.**

Tic recurrente sin consecuencia grave: alucina nombres de tools (`search`) y
emite rutas truncadas con puntos suspensivos (`"ro..."`). El harness responde
con la lista válida y el modelo se recupera solo.

---

## 4. La receta: partir en tres pasos acotados

Salió del ejercicio sobre roam, donde pedir el refactor entero falló dos
veces y partirlo funcionó. Cada paso con oráculo objetivo (el compilador):

1. **Crear** el archivo nuevo con el contenido → salió a la primera.
2. **Quitar del original y enlazar** el módulo → sale con presupuesto
   suficiente.
3. **Arreglar los errores de compilación** que queden → 157s, a la primera.

Esa secuencia produjo el refactor completo: 222 líneas movidas, 14/14 tests.
Lo que no funciona es pedir los tres juntos.

**Regla general**: si la tarea no cabe en ~6 rondas, pártela. No es
preferencia de estilo — arriba de ese umbral el resultado deja de ser
reproducible.

### Dos correcciones medidas el 28-jul

Una segunda extracción de módulo en roam (`metrics`, ver
`docs/roam-metrics-memoria-2026-07-28.md`) afinó la receta en dos puntos:

- **El paso 3 se puede eliminar.** Si el paso 1 pide de una vez el cambio de
  visibilidad que el refactor va a necesitar (`fn` → `pub(crate) fn` para lo
  que los tests llamen desde fuera del módulo nuevo), no queda ningún error
  de compilación que arreglar. El paso 3 no es una etapa: es la deuda de no
  anticipar la visibilidad.
- **En el paso 2, pásale el bloque exacto en el prompt.** Pedir "borra estas
  cinco funciones" quemó las 20 rondas del turno y dejó el archivo a medias:
  solo 2 rondas fueron ediciones productivas, el resto se fue en leer el
  archivo a tientas y en alucinar una tool `search` que no existe. Reintentado
  con el bloque literal embebido y la instrucción de no leer el archivo, salió
  en **2m00s a la primera**. Lo caro no es borrar 40 líneas — es buscarlas.

---

## 5. El flujo de contexto: sesión nueva + memoria de proyecto

`braze run` **abre sesión nueva en cada invocación**, así que el contexto
nace vacío sin que haya que vaciar nada. Eso es justo lo que conviene dado
el punto anterior: cada paso arranca limpio en vez de arrastrar el ruido del
anterior.

Para que no se pierda lo aprendido entre pasos existe la **memoria de
proyecto** (`enable_project_memory`, **apagada por default**):

- Al arrancar carga `.braze/memory.json` de la raíz del proyecto y lo
  renderiza en el system prompt con presupuesto de **400 tokens**.
- Durante el turno, un hook registra los archivos tocados por herramientas de
  escritura y persiste en background.
- Falla segura: un `memory.json` roto no bloquea el arranque, y una memoria
  cuyo `project_key` no corresponde al proyecto se descarta en vez de
  inyectar notas ajenas.

Se enciende de dos maneras. Por invocación, con la variable de entorno —lo
más práctico para un flujo de varios pasos, porque no cambia el default de
nada más:

```bash
export BRAZE_ENABLE_PROJECT_MEMORY=true
```

O de forma durable en el archivo de configuración, que es
`~/.config/braze/config.json` y es **JSON**, no TOML:

```json
{ "enable_project_memory": true }
```

Está apagada por default por política del proyecto —una palanca nueva entra
apagada y se promueve solo si su propio A/B la valida— así que **encenderla
es una decisión, no el default recomendado**. Lo que registra
automáticamente es modesto: qué archivos se tocaron, no un diario de
decisiones.

Con eso, el flujo *cargar contexto → ejecutar tarea → guardar → vaciar* ya
está implementado: es simplemente **un `braze run` por paso**, con la
memoria encendida.

---

## 6. Verificación: qué caza cada gate

| Gate | Caza | NO caza |
|---|---|---|
| `cargo check` post-edit | errores de sintaxis y tipos | archivos que ningún `mod` declara (ver abajo) |
| Tests del proyecto | regresiones de comportamiento | corrupción en comentarios/strings |
| Aserciones del bench | el resultado declarado | que las regiones no apuntadas se preservaran |
| **`git diff` humano** | **todo lo anterior** | — |

Sobre el post-edit check: si escribe un `.rs` que **ningún módulo declara**,
el compilador no lo mira y el check pasa en verde sobre un archivo roto. El
harness ahora avisa de eso, pero conviene saberlo porque es el patrón normal
de subdividir código: crear el archivo, enlazarlo después.

**Las dos reglas que resumen todo:**

1. **Trabajo acotado con oráculo objetivo** — compilador o tests, no criterio
   propio.
2. **Verifica con el diff, nunca con su resumen.** Es la única defensa contra
   el éxito falso y la corrupción silenciosa.

---

## 7. Evidencia

- `docs/local-backend-design-2026-07-20.md` — auto-fit, KV placement medido,
  sampling, el 3.4× de gpt-oss.
- `docs/roam-trajectory-exercise-2026-07-26.md` — el ejercicio del que salen
  la receta de tres pasos y las corrupciones silenciosas.
- `docs/roam-metrics-memoria-2026-07-28.md` — la segunda extracción de módulo:
  el flujo con memoria de proyecto verificado end-to-end, y las dos
  correcciones a la receta de § 4.
- `docs/noise-floor-2026-07-26.md` — piso de ruido por modelo; **consultar
  antes de interpretar cualquier A/B**.
- `docs/umbral-trayectoria-refutado-2026-07-28.md` — por qué el umbral de ~6
  rondas vale para gpt-oss y **no** generaliza a otros modelos.
- `crates/braze-bench/suites/fast-core.toml` — 13 tareas, ~15 min, para
  verificar que un cambio no rompió nada.
