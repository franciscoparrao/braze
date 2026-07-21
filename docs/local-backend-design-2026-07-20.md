# Diseño: `LocalBackend` — inferencia in-process sobre `llama-cpp-2`

> **Estado (2026-07-21):** EN CONSTRUCCIÓN — **Fase 1 funcional**. El
> `LocalBackend` (quinto `impl ModelBackend`, feature `local`) carga el
> GGUF de Ollama in-process y hace el **loop agéntico completo** sobre
> qwen2.5:3b sin Ollama: tool call → ejecución → respuesta (commits
> `9329283`, `f0094e9`). El "format tax" se resolvió reproduciendo el
> preámbulo de tools NATIVO de qwen (no el chat template embebido —
> `apply_chat_template` de llama-cpp-2 0.1.151 no soporta tools). Falta
> para cerrar Fase 1: paridad de bench vs `OllamaBackend` sobre
> qwen2.5:3b (`g10-weak-skills`) + sampling. Fase 2: GPU/CUDA + harmony
> (gpt-oss:20b). Historial: spike exitoso; reabre la decisión de
> 2026-07-13 sobre la justificación nueva que aquel cierre anticipó.

## Por qué se reabre (justificación nueva, no la vieja)

El `docs/local-backend-stencil-design.md` se cerró el 2026-07-13 porque
su justificación era de **capacidad** ("¿un backend local hace al modelo
mejor?") y esa pregunta murió: `gpt-oss:20b` ya gana desde Ollama sin
construir nada. Correcto entonces, y sigue correcto: **este documento no
reabre el eje de capacidad.**

La justificación nueva es de **robustez de runtime y control del
decoder**, que aquel cierre foreseía palabra por palabra:

> "Si en el futuro cambia el panorama […] o el proyecto necesita control
> del decoder por una razón distinta a tool-calling, este documento sirve
> como registro" — cierre del doc anterior, 2026-07-13.

Lo que cambió el panorama: el testbed roam × braze
(`docs/bitacora-harness-modelo.html`) produjo **dos hallazgos seguidos
que son culpa del runtime de Ollama, no de la interfaz harness↔modelo**
—el sujeto del experimento—:

- **#1** (HTTP 500 "error parsing tool call"): Ollama parsea el canal
  harmony de gpt-oss server-side y revienta cuando el razonamiento se
  filtra. Resuelto por infra (0.30.7 → 0.32.1), pero es un bug de
  *versión del runtime*.
- **#17** (mismo error, variante mid-stream): sobrevivió al upgrade;
  braze lo re-muestrea (commit `2eebc91`) pero la causa raíz sigue en
  Ollama.

Estos son **contaminantes metodológicos**: hallazgos de "nuestro
middleware de inferencia tiene bugs", valiosos operacionalmente pero
*fuera de tesis*. Ollama se mete de intermediario a **pre-parsear las
tool calls**, y ahí viven los bugs — cuando braze **ya tiene su propio
parser** (la escalera de rescate, `crates/braze-engine/src/rescue.rs`).
El middleman es en parte redundante *y* buggy.

Un `LocalBackend` que reciba **texto crudo** y deje que la escalera de
rescate haga el parsing elimina **toda esa clase de bug por
construcción**: no hay parser de tool-calls server-side que romper.

## Qué probó el spike (2026-07-20)

Crate throwaway (`scratchpad/braze-local-spike`), no en el workspace.
Objetivo: responder la única pregunta genuinamente incierta — *¿puede un
binding Rust de llama.cpp cargar el GGUF que Ollama ya tiene y producir
texto crudo?* Resultado **sí**, punto por punto:

1. **Toolchain presente**: `libclang-18`, `cmake`, `cc`/`gcc` en la
   máquina local (y en Nitro: `cc`/`gcc`/`cmake`, 12 cores, GPU CUDA).
2. **`llama-cpp-2` compila** (utilityai, trackea upstream): ~5.5 min
   compilando llama.cpp desde fuente, una sola vez.
3. **Carga el GGUF de Ollama directo**: los blobs de Ollama son GGUF
   estándar (`magic 47 47 55 46`), `-rw-r--r--` (world-readable; el
   usuario está en el grupo `ollama`). Se abre por ruta al blob
   `sha256-…`, **sin Ollama, sin server, reusando los pesos bajados**
   (qwen2.5:3b en el spike; gpt-oss:20b es el mismo formato).
4. **Infiere en CPU y produce texto crudo** con una tool call.
5. **La escalera de rescate real cierra el loop**: verificado contra
   `rescue.rs` (funciones `pub(crate)`, test temporal removido). La forma
   estándar `[read_file(path="Cargo.toml")]` parsea a
   `("read_file", {"path": "Cargo.toml"})`. (La salida cruda del spike
   fue una variante sin corchetes que la escalera *no* cubre — artefacto
   de un prompt ad-hoc; con el addendum prompt-tools real de braze el
   modelo emite un formato cubierto.)

**Dato de binding, no trivial:** `edgenai/llama_cpp` 0.3.2 **falló** —
vendoriza una llama.cpp vieja que ni lee el GGUF de qwen2.5 ("internal
assertion failed"). `llama-cpp-2` funcionó. **El binding correcto es
`llama-cpp-2`.**

## El seam de integración (sin cambios respecto al doc anterior)

Punto de enganche: **`trait ModelBackend`** en `braze-model`. Dyn-dispatch
(`Box<dyn ModelBackend>` en el engine), con cuatro implementadores hoy
(`AnthropicBackend`, `OllamaBackend`, `OpenRouterBackend`, y
`EscalatingBackend` que compone) → **enchufa un quinto sin tocar el
engine**. Plantilla: `ollama.rs` + `ollama_wire.rs` (ya trae
`with_prompt_tools`/`with_constrained_tools`, y el priming del resample
de #17 que es reusable).

Contrato que el backend debe cumplir (confirmado por la auditoría del doc
anterior, sigue vigente):

1. Devolver `Stream<CompletionEvent>` con
   `TextDelta` / `ToolCallRequested{id,name,arguments}` / `Usage` / `Done`.
   **Invariante: terminar en `Done` o `Err`.**
2. Traducir `Vec<Message>` (bloques `Text`/`ToolUse`/`ToolResult`) a la
   plantilla de chat del modelo (ChatML para qwen, harmony para gpt-oss).
3. Consumir `ToolStub` + resolver schema on-demand vía
   `ToolProvider::resolve_schema`.
4. Reportar `Usage` (local = $0; el catch-all de pricing ya existe).

Registro: nombre nuevo en `Config::default_backend` + composition root de
`braze-cli`. Permisos son agnósticos al backend — sin cambios.

**Diferencia arquitectónica clave vs. Ollama:** el `OllamaBackend` recibe
`tool_calls` **ya parseados** por el server y sólo en `--prompt-tools`
usa la escalera de rescate. El `LocalBackend` recibe **siempre texto
crudo** (tokens → string) y **siempre** parsea con la escalera de rescate.
Es decir, `LocalBackend` es prompt-tools nativo y total: no existe la ruta
del parser server-side, así que #1/#17 no pueden ocurrir.

## Qué construir

### 1. `LocalBackend` — quinto `impl ModelBackend` sobre `llama-cpp-2`
El glue: cargar el GGUF, mantener el `LlamaContext`, y el loop de
generación token → `TextDelta` acumulado → al cierre del turno, pasar el
texto por la escalera de rescate para emitir `ToolCallRequested`. El
grueso del trabajo es el **contrato de streaming** (cancelación, `num_ctx`,
sampling knobs, la invariante `Done`/`Err`), no la numérica — esa la hace
llama.cpp.

Sub-piezas:
- **Localización del GGUF**: resolver `modelo:tag` → blob de Ollama vía el
  manifest (`…/manifests/…/library/<modelo>/<tag>`, capa `mediaType`
  `…model` → `sha256-<digest>` en `…/blobs/`), o ruta directa a un
  `.gguf`. Reusar los pesos ya bajados; sin re-download.
- **GPU offload (CUDA)** para gpt-oss:20b en Nitro: `llama-cpp-2` lo
  soporta vía `n_gpu_layers` + feature `cuda` en el build. Es la pieza
  que hace el 20B viable en Nitro (el spike fue CPU/3b — offload es
  integración conocida, no incógnita).
- **Plantilla de chat por familia**: ChatML (qwen), harmony (gpt-oss). El
  `LocalBackend` la aplica él mismo (no delega en el server), lo que da
  control total y quita la capa donde Ollama fallaba.

### 2. `stencil` — constrained decoding GBNF (la pieza novedosa, opcional)
Con el decoder propio, resucita lo que el doc anterior descartó *sólo
porque no se podía hacer sobre HTTP*: una **gramática GBNF** que enmascara
los logits token a token para que la sintaxis de la tool call sea
**imposible de romper antes de emitirse**. `llama-cpp-2` ya expone
`LlamaSampler` con soporte de gramática — no hay que escribir el
enmascarador desde cero, sólo la gramática afinada al envelope de braze.

Esto no *sobrevive* el error de #17 (como hace el resample) — lo hace
**imposible de generar**. Es la palanca de capacidad que ningún backend
HTTP puede ofrecer, y la expresión más pura de la tesis (el harness dueño
de todo desde los tokens hacia arriba). Es el diferencial publicable.

## Sustrato: `llama-cpp-2` (actualiza la recomendación del doc anterior)

El doc anterior recomendaba **mistral.rs** (teórico, no validado).
El spike cambió la evidencia:

| Sustrato | Estado | Nota |
|---|---|---|
| **`llama-cpp-2`** | **validado por el spike** | Carga el GGUF de Ollama, infiere, trackea upstream (soporta modelos nuevos), CUDA, GBNF. FFI a C++ (`unsafe`), API de bajo nivel. **Recomendado.** |
| `edgenai/llama_cpp` | **descartado** | Falla al leer GGUF de qwen2.5 (llama.cpp vendorizada vieja). |
| `mistral.rs` | no evaluado | Rust puro, MoE offload, grammar decoding — pero más joven y no validado contra los GGUF de Ollama. Alternativa si el FFI de llama.cpp molesta. |
| `candle` | descartado para esto | Soporte GGUF parcial, más trabajo, sin ventaja clara. |

**Recomendación:** `llama-cpp-2`. Es el que el spike probó, el que
trackea upstream (crítico: gpt-oss/harmony y modelos futuros), y el que
ya trae CUDA + GBNF. El costo es el FFI (build de C++, `unsafe`), pagado
una vez.

## Alcance V1 (a congelar al arrancar, si se arranca)

**Dentro:** `LocalBackend` (impl `ModelBackend` sobre `llama-cpp-2`,
streaming token → `TextDelta`, rescate → `ToolCallRequested`, `Usage` $0,
invariante `Done`/`Err`) + localización de GGUF desde blobs de Ollama +
GPU offload CUDA para Nitro + plantilla ChatML/harmony.

**Fuera de V1 (fase 2):** `stencil`/GBNF (el diferencial, pero no
bloqueante — el rescate ya cubre el parsing), streaming NVMe, multi-GPU.

## Plan por fases (con costo honesto)

1. **Fase 0 — spike (HECHO).** Factibilidad probada. ~1 sesión.
2. **Fase 1 — `LocalBackend` CPU mínimo.** Crate nuevo, impl del trait,
   streaming + rescate, un modelo chico (qwen2.5:3b) end-to-end en
   `braze run`. Verificación: paridad de comportamiento con el
   `OllamaBackend` sobre el mismo modelo en `g10-weak-skills`.
   **~1 semana.**
3. **Fase 2 — GPU/Nitro + gpt-oss:20b.** Feature CUDA, `n_gpu_layers`,
   tuning de throughput. Verificación: sweep `default.toml` con paridad
   de pass rate vs. el `OllamaBackend` actual, y **cero de la clase de
   error #1/#17**. **~1 semana.**
4. **Fase 3 — `stencil`/GBNF (opcional).** Gramática del envelope →
   `schema_fail + rescues ≈ 0`. Es el A/B publicable. **~1-2 semanas.**

Total realista para paridad operativa (fases 1-2): **~2 semanas**. El
diferencial (fase 3): **+1-2 semanas.**

## Riesgos / caveats honestos

- **FFI a C++**: `llama-cpp-2` compila llama.cpp (build lento la primera
  vez, `unsafe` en la frontera, dependencia pesada en un workspace que se
  mantuvo lean). Mitigación: está aislado en un crate, detrás del trait.
- **Fast-moving target**: cada arquitectura nueva puede exigir upstream
  nuevo. Mitigación: `llama-cpp-2` trackea llama.cpp de cerca (por eso se
  descartó edgenai) — el mantenimiento lo hacen ellos.
- **Scope creep sobre el paper**: el paper es sobre harness engineering,
  no motores de inferencia. Mitigación: encuadrar el runtime-ownership
  como *parte* del harness ("el harness es dueño desde los tokens"), lo
  que el constrained decoding hace verdad. Si no se cree ese encuadre, es
  un desvío — decisión honesta del autor.
- **La causa raíz de #1/#17 es de Ollama, no de braze**: construir esto
  para escaparle es válido, pero dos hallazgos en 16 sesiones **no es una
  crisis**. Ver § Decisión.
- **GPU en Nitro**: el spike fue CPU/3b. El 20B necesita CUDA + VRAM
  (Nitro la tiene, Ollama la usa hoy). Riesgo bajo pero no cero hasta la
  fase 2.

## Decisión: construir ahora vs. palanca diferida

Dos caminos honestos:

- **Construir ahora.** Justificable si el runtime-ownership + constrained
  decoding se ve como parte central de la tesis (el argumento más fuerte:
  la fase 3 hace #17 *imposible*, no sólo recuperable, y eso ningún
  backend HTTP lo da). Costo: ~2-4 semanas, desvía del paper.
- **Palanca diferida (recomendado por disciplina).** El spike ya
  de-riesgó la decisión: sabemos que es viable. Dejarlo validado y
  **disparar la construcción cuando aparezca un tercer hallazgo que sea
  culpa del runtime de Ollama** (no del harness ni del modelo). Dos
  hallazgos con 0.32.1 ya arreglando #1 y #17 sobrevivido no es urgencia;
  un tercero sí sería señal de que Ollama-como-middleman es un costo
  recurrente que justifica las semanas.

En cualquiera de los dos, el spike y este documento son el punto de
partida real, no teórico — a diferencia del cierre de 2026-07-13, que se
apoyaba en mistral.rs sin validar.

## Referencias

- Spike: `scratchpad/braze-local-spike/` (throwaway, fuera del workspace).
- Decisión anterior (cerrada, capacidad): `docs/local-backend-stencil-design.md`.
- Hallazgos que motivan: `docs/bitacora-harness-modelo.html` (#1, #17).
- Contrato del backend: `ollama.rs`/`ollama_wire.rs` como plantilla;
  escalera de rescate en `braze-engine/src/rescue.rs`.
