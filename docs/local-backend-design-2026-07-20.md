# Diseño: `LocalBackend` — inferencia in-process sobre `llama-cpp-2`

> **Estado (2026-07-21):** **Fase 1 CERRADA + Fase 2 GPU CONSEGUIDA** (funcional + paridad
> medida). El `LocalBackend` (quinto `impl ModelBackend`, feature
> `local`) carga el GGUF de Ollama in-process y hace loops agénticos
> completos sobre qwen2.5:3b sin Ollama (commits `9329283`, `f0094e9`).
> **Paridad vs `OllamaBackend`** (`default.toml`, qwen2.5:3b, mismo
> hardware — `docs/sweep-localbackend-parity-2026-07-21.md`):
> **paridad EXACTA en single_tool (6/7) y no_tool (3/3)**; gap en
> multi-ronda (10/19 vs 14/19 total, McNemar p=0.22 no significativo)
> por el "format tax" (`schema_fail=17`: el preámbulo da nombre+summary,
> no el schema de argumentos). La paridad además cazó y arregló un bug
> real (`LlamaBackend` singleton global). Cierra el gap: Fase 3 (GBNF)
> o schemas en el preámbulo. **Próximo: Fase 2** (GPU/CUDA + harmony
> para gpt-oss:20b en Nitro). Historial: spike exitoso; reabre la
> decisión de 2026-07-13 sobre la justificación que aquel cierre
> anticipó.

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

## Fase 2 — GPU en Nitro: CONSEGUIDA (2026-07-21)

El LocalBackend compilado con CUDA corre en la GPU de Nitro (RTX 3050
6GB). Verificado en vivo: `BRAZE_LOCAL_GPU_LAYERS=999` con qwen2.5:3b →
**`offloaded 37/37 layers to GPU`** (modelo entero en VRAM) + loop
agéntico completo y correcto. Falta para gpt-oss:20b: el preámbulo/
plantilla **harmony** (independiente del build) y ver el offload parcial
en 6GB (como Ollama).

## Fase 2b — Harmony para gpt-oss (2026-07-21)

Implementado en `braze-model`: `harmony.rs` (módulo **puro**, compila y
testea sin el feature `local`) + integración en `local.rs`.

- **Plantilla**: system canónico de Harmony (identidad "You are
  ChatGPT" — cambiarla es format tax —, cutoff, fecha, `Reasoning:` con
  default `medium` y override `BRAZE_LOCAL_REASONING`, canales válidos),
  developer message (`# Instructions` = system prompt de braze +
  `# Tools` como namespace TypeScript generado del `input_schema`),
  historial mapeado por bloque (ToolUse → `commentary
  to=functions.<name>`; ToolResult → mensaje del rol
  `functions.<name>`, con el nombre recuperado del id), y
  `<|start|>assistant` abierto. Sin BOS (`add_bos=false` en el GGUF).
- **Parsing en el backend, no en el engine**: los marcadores de Harmony
  (`<|channel|>`, `<|message|>`, `<|call|>`, `<|return|>`, …) son tokens
  **especiales** — no sobreviven `token_to_piece(special=false)` y la
  escalera de rescate del engine jamás los vería. `local.rs` resuelve
  sus ids en el vocabulario al cargar (error temprano si el GGUF no es
  harmony) y una máquina de estados (`HarmonyParser`) interpreta el
  stream: `final` y `commentary` sin destinatario fluyen como
  `TextDelta`, `analysis` se suprime, `commentary to=functions.X` se
  acumula y emite como `ToolCallRequested` (args por la escalera de
  reparación compartida, ids sintéticos nonce+contador como los wires).
  `stop_reason` honesto: `tool_use`/`stop`/`length`. Degradación
  elegante: un modelo que ignore Harmony fluye como texto y el rescate
  del engine sigue aplicando río arriba.
- **Detección de familia**: `general.architecture` del GGUF (`gptoss` /
  `gpt-oss`) o el label del modelo; override `BRAZE_LOCAL_FAMILY=
  harmony|chatml`.

**Hallazgo: el blob gpt-oss de Ollama NO es un GGUF de llama.cpp.**
"Reusar los GGUF de Ollama" vale para qwen (Fase 1) pero no para
gpt-oss: Ollama convirtió gpt-oss para su engine propio — el blob
declara `general.architecture = "gptoss"` (llama.cpp espera
`"gpt-oss"`), prefija la metadata como `gptoss.*` y nombra tensores
distinto (p.ej. `blk.N.attn_out` vs. el `blk.N.attn_output` de
llama.cpp). llama.cpp lo rechaza con `unknown model architecture:
'gptoss'`. Parchear el blob exigiría reescribir los 13GB (strings
length-prefixed) — frágil. Salida adoptada: el GGUF canónico
`ggml-org/gpt-oss-20b-GGUF` (MXFP4, ~12GB, `~/models/` en Nitro) por la
ruta directa `.gguf` que el LocalBackend ya soporta.

### Receta de build CUDA en Nitro (Ubuntu 26.04) — costó descubrirla

Prerrequisitos (una vez):
- `rustup` (user-level) **y** `rustup component add rustfmt` (el
  `--profile minimal` lo omite; el `build.rs` de llama-cpp-sys lo usa).
- CUDA toolkit: `sudo apt install nvidia-cuda-toolkit` (da `nvcc`; el
  distro trae las libs **dinámicas** de CUDA, no las estáticas).
- **`sudo apt install libclang-18-dev`** — clave: **NO usar libclang-21**,
  cuya detección de *resource dir* está rota en Nitro (bindgen no
  encuentra `stdbool.h` por más que se le apunte). La 18 funciona.

Build:
```
cd ~/braze
env CUDACXX=/usr/bin/nvcc \
    CMAKE_CUDA_ARCHITECTURES=86 \
    LIBCLANG_PATH=/usr/lib/llvm-18/lib \
    LLAMA_BUILD_SHARED_LIBS=1 \
    cargo build -p braze-cli --features local-cuda
```
- `LLAMA_BUILD_SHARED_LIBS=1` (valor **`1`**, no `ON`) → linkeo dinámico:
  el distro no trae `cublas_static`/`culibos`, así que el linkeo estático
  default falla; shared usa las `.so` que sí existen.
- `CMAKE_CUDA_ARCHITECTURES=86` = compute capability del RTX 3050.

Runtime:
```
LD_LIBRARY_PATH=~/braze/target/debug/build/llama-cpp-sys-2-*/out/build/bin \
BRAZE_LOCAL_GPU_LAYERS=999 \
BRAZE_OLLAMA_MODELS_ROOT=/usr/share/ollama/.ollama \
  braze run --backend local --model qwen2.5:3b "..."
```

Trampa depurada: un proceso `build-script` colgado de un intento previo
(con libclang-21) mantenía cacheada la falla de bindgen; matarlo + build
limpio fue lo que destrabó. Si bindgen falla con `stdbool.h` pese a
libclang-18, matar procesos `cargo`/`build-script` viejos y `cargo clean`.

## Fase 3 — `stencil` GBNF: IMPLEMENTADA (2026-07-21)

`stencil.rs` en braze-model (puro, mismo patrón que `harmony.rs` — 8
tests corren sin el feature) + integración en el loop de `local.rs`.
**Laziness manual**, no `grammar_lazy` de llama.cpp: siendo dueños del
loop de decode, el sampler se swapea a `chain(gramática GBNF, greedy)`
exactamente cuando empieza una tool call y vuelve a libre al completarse
el envelope. El modelo escribe texto libre antes y después; solo la call
está estencilada.

- **ChatML/qwen**: tras el literal `<tool_call>` (rolling tail), el
  envelope completo — `{"name": <uno-del-inventario>, "arguments":
  <objeto JSON>}` + `</tool_call>` garantizado. Orden de claves fijo
  (formato entrenado), nombres alucinados mueren en el sampler.
- **Harmony/gpt-oss**: al fijar el header un destinatario
  (`<|message|>` con `to=functions.X`), los args se estencilan a objeto
  JSON válido; al cerrar (JsonCursor a profundidad 0) se libera y el
  modelo emite su `<|call|>`.
- Kill-switch **`BRAZE_LOCAL_GRAMMAR=off`** = brazo de ablación del A/B.
- Verificado en vivo (2026-07-21, Nitro): activación/cierre trazados en
  ambas familias, tareas correctas, y `=off` limpio.

**Bug depurado en el camino (latente desde Fase 1):**
`llama_sampler_sample()` ya hace `accept` internamente; nuestro
`sampler.accept(token)` explícito era un **double-accept** — inofensivo
con greedy (stateless), fatal con gramática (el stack GBNF avanzaba dos
veces → `GGML_ASSERT(!stacks.empty())`, SIGABRT). La gramática fue lo
que lo hizo visible.

**A/B ejecutado (2026-07-21, `docs/sweep-stencil-ab-2026-07-21.md`):**
41/57 vs 40/57, McNemar p=1.0 — sin diferencia de pass rate y **sin
constraint tax**. La hipótesis "schema_fail+rescues→0" resultó mal
planteada: rescues cuenta la extracción normal (no puede ser 0) y el
schema_fail del bench es conformidad de args, que el envelope no ataca
(y cuya clase de envelope el preámbulo de Fase 1 ya había vaciado). El
valor demostrado es la garantía por construcción a costo cero; el
próximo paso con señal esperable es la gramática **derivada del
schema** (json-schema → GBNF por tool). El proceso destapó y corrigió 3
bugs latentes de Fase 1 (double-accept, prompt>n_batch, token de
control espurio) — ver el doc del sweep.

## Auto-fit de `n_gpu_layers`: IMPLEMENTADO (2026-07-25)

Palanca **#1** de `docs/inference-runtimes-audit-2026-07-25.md` — la que la
auditoría marcó como de mayor impacto, y la única de las tres accionadas que
efectivamente rindió (#2 KV-quant no ayuda a gpt-oss, #4 `-march` ya estaba
resuelto).

**Hallazgo que cambió el plan**: el plan era *portar* el greedy de
`auto_device_map.rs` de mistral.rs o de `common/fit.cpp`. No hizo falta:
`llama-cpp-2` **ya envuelve `common_fit_params`** de libcommon en
`LlamaModelParams::fit_params` — el mismo algoritmo que corre `llama-cli
--fit`, detrás del feature `common`, que está en los **default features** y
por lo tanto ya venía compilado en braze. Costo de build: cero. Esto corrige
la § 7 del doc de auditoría, que listaba solo los primitivos sueltos
(`list_llama_ggml_backend_devices`, `buft_overrides`) y concluía "hay que
construirlo"; en realidad el algoritmo entero está expuesto.

**Qué hace** (`resolve_model_params` en `local.rs`): carga el modelo con
`no_alloc=true` (probe sin pesos), mide VRAM libre por device, llena capas
densas back-to-front dejando el margen, y manda los tensores MoE sobrantes a
RAM vía `tensor_buft_overrides`.

**Precedencia**: `BRAZE_LOCAL_GPU_LAYERS` explícito > auto-fit > CPU. El env
sigue siendo el escape hatch y el brazo de ablación; `BRAZE_LOCAL_AUTOFIT=off`
es el kill-switch. Cualquier fallo del fit degrada a CPU con un `warn`, nunca
crashea. Margen por device configurable con `BRAZE_LOCAL_VRAM_MARGIN_MB`
(default 1 GiB = el de llama.cpp upstream, cuyo `fit_params` también es
`true` por default).

**Verificado en vivo en Nitro (RTX 3050 6GB, 2026-07-25)**:

- `qwen2.5:3b` → `gpu_layers=-1` (entero en GPU), `fitted_n_ctx=8192`.
- `gpt-oss-20b-MXFP4` (12GB de pesos en una tarjeta de 6GB) → **25 capas**
  (el modelo completo: 24 + output), pico de VRAM **4827 MiB de 6144**, sin
  OOM. El valor que se venía adivinando a mano era **8/24**.
- A/B de throughput con generación larga (el decode domina; walltime incluye
  la carga, así que son cotas *inferiores* para el auto-fit):

  | Config | Walltime | Chars | Chars/s |
  |---|---|---|---|
  | Auto-fit (25 capas) | 121s | 6405 | **52.9** |
  | Manual 8 capas | 135s | 6066 | 44.9 |
  | CPU (0 capas) | 173s | 6485 | 37.5 |

  **+18% vs la adivinanza manual, +41% vs CPU.** n=1 por brazo y `chars` es
  proxy de tokens — direccional, no un número de paper.

**Dos interacciones que el diseño tuvo que cubrir** (ambas salieron de mirar
el código, no de suponer):

1. **El probe debe medir con los cparams reales.** El fit calcula contra un
   contexto concreto; si el probe usara otros parámetros que la generación,
   repartiría capas contra un consumo de VRAM que no es el real. Por eso
   `build_ctx_params` es una sola función usada por ambos lados, y
   `gpu_layers` viaja resuelto desde la carga hasta el hilo de generación en
   `GenParams` en vez de releerse del entorno. Hay un test que fija la
   invariante (`el_kv_en_host_se_activa_exactamente_cuando_hay_offload_a_gpu`).
2. **KV-quant podía costar la GPU entera.** El KV cuantizado requiere
   flash-attn, que gpt-oss/Harmony no soporta → el probe del fit falla al
   crear su contexto. Sin cubrirlo, un `BRAZE_LOCAL_KV_TYPE` no soportado
   hacía fracasar el fit y degradaba a CPU por una palanca ortogonal. El fit
   reintenta con f16, igual que ya hacía la creación real del contexto.

**Efecto colateral corregido**: la condición de KV-en-host era `gpu_layers >
0` leyendo un `u32` del entorno. Con la convención de llama.cpp (`-1` = todas
las capas) ese `> 0` trataba "todo en GPU" como CPU puro. Ahora es `!= 0`
sobre un `i32`.

**Nota de infra**: Nitro resuelve `llama-cpp-sys-2` **0.1.152** (la máquina de
trabajo, 0.1.151); `fit_params` existe igual en ambas. El build en Nitro
**exige** la receta completa de arriba — omitir `LLAMA_BUILD_SHARED_LIBS=1`
con un build dir cacheado hace que `build.rs` busque `.a` donde hay `.so` y
reviente con `assert_ne!(llama_libs.len(), 0)`, que no se parece en nada a la
causa real.

## Caché de modelo cargado (2026-07-25)

Sale de una observación del sweep de gemma-12B: la VRAM caía a ~177 MiB entre
tareas y volvía a ~4.7GB en cada una. braze-bench crea un `LocalBackend` por
tarea, así que un sweep de 57 tareas pagaba **57 veces** el probe del auto-fit
(que carga el modelo con `no_alloc`) **más** 57 recargas del GGUF completo con
su re-subida de capas a la GPU.

**Por qué se cachea el modelo y no el fit.** Cachear solo el resultado del fit
no alcanza: además de `n_gpu_layers`, `fit_params` deja `tensor_split` y
`tensor_buft_overrides` dentro del struct de params, y los overrides son
punteros crudos a memoria que ese struct posee (son los que mandan los
expertos MoE a RAM — lo que le dio a gpt-oss el modelo entero en GPU). No hay
forma portable de leerlos y re-aplicarlos. Cachear el `LlamaModel` ya cargado
resuelve las dos cosas de una, y es semánticamente seguro: los pesos son
read-only (por eso ya viajaban en `Arc`) y el contexto se sigue creando fresco
por generación, así que no se comparte estado de inferencia entre tareas.

**Diseño**: `MODEL_CACHE` de capacidad **1**, con eviction antes de cargar el
reemplazo — dos modelos de 6-12GB vivos a la vez revientan la RAM/VRAM de
Nitro, que es justo el fallo que el auto-fit vino a eliminar. La clave es
`(ruta canonicalizada, n_ctx, snapshot de las 6 env vars que el fit consulta)`:
cambiar `BRAZE_LOCAL_GPU_LAYERS` o el margen fuerza recarga en vez de devolver
un modelo repartido con la configuración anterior. El lock se sostiene durante
la carga a propósito, para que dos hilos pidiendo el mismo modelo no carguen
un duplicado. Kill-switch: `BRAZE_LOCAL_MODEL_CACHE=off`.

**Corrección al caveat original (2026-07-25, mismo día).** Se anotó acá que el
caché "rompe la comparabilidad de `wall_time_ms`" porque solo la primera tarea
paga la carga. **Es falso**, y lo desmintió leer el bench: `runner.rs` hace
`let started = Instant::now()` **después** de construir engine y backend, así
que la carga del modelo y el probe del fit **nunca estuvieron dentro de la
ventana medida**. El caché no toca `wall_time_ms` ni la comparabilidad contra
sweeps viejos.

Lo que el caché sí cambia es la **duración total** del sweep (deja de pagar
~57 cargas de 6-12GB) — valioso, pero es tiempo de pared del operador, no una
métrica del experimento. `BRAZE_LOCAL_MODEL_CACHE=off` sigue existiendo como
kill-switch.

**Estado**: implementado y con tests unitarios (la clave distingue modelo y
contexto), pero **sin verificación en vivo todavía** — requiere braze-bench con
varias tareas en un proceso, y Nitro estaba ocupado con el sweep de gemma-12B
cuando se escribió esto. Verificar antes de confiar en él para un sweep.

## KV placement por medición, no por regla (2026-07-25)

Continuación directa del auto-fit, y el cambio que más rindió del día. Salió
de investigar por qué el primer sweep de gemma-12B con auto-fit dio **más
lento** que el sweep del 21-jul, pese a offloadear 33 capas contra 14.

### La investigación (dos hipótesis mías refutadas antes de dar con la buena)

1. **"El KV en host hace que más capas cuesten más round-trips PCIe"** —
   refutada. Test controlado, mismo binario, 3 reps: con prompt de 6283 chars
   y salida de una palabra (prefill puro), 33 capas tarda 17-18s contra 26s de
   14 capas. Más capas ayuda en prefill **y** en decode.
2. **"El probe del auto-fit corre por tarea y se come la ganancia"** —
   refutada por el propio código del bench: `runner.rs` hace
   `let started = Instant::now()` **después** de construir engine y backend,
   así que la carga y el fit quedan fuera de `wall_time_ms`.

La causa real la dio `git log -S`: el commit **`483f8e2` (23-jul)** introdujo
`with_offload_kqv(false)` + `n_ubatch=128` como defensa contra un OOM de VRAM
con gpt-oss, aplicándolos **siempre que hubiera offload**. El sweep del 21-jul
corrió antes de eso. No era la librería (0.1.152) ni las capas: era la defensa.

Confirmado reproduciendo la config del 21-jul con el binario de hoy — un solo
flag, porque `kv_on_host` gateaba también el micro-batch.

### El cambio

`kv_on_host = gpu_layers != 0` (regla fija) pasa a `KvPlacement` resuelto por
el fit: se prueba primero `Device` (KV en VRAM + batches default, el camino
rápido de llama.cpp) y **solo se cae a `Host` si con el KV en VRAM no entra
ninguna capa**. Misma jugada que la palanca #1 hizo con `n_gpu_layers`:
cambiar un supuesto por una medición.

- El placement viaja resuelto de la carga al hilo de generación (como
  `gpu_layers`), no se relee del entorno.
- `context_ladder()` es la red de seguridad: degrada primero el KV cuantizado
  (necesita flash-attn), después la VRAM. Desde `Host` nunca vuelve a
  proponer `Device` — si ya se midió que no entra, es chocar con la misma
  pared. Cubre que la medición se quede corta **o** que las capas vengan
  fijadas a mano por env sin medición ninguna.
- `BRAZE_LOCAL_KV_OFFLOAD` acepta ahora `host` además de `gpu`, para ablacionar
  sin recompilar.

**Cambio de comportamiento declarado**: con `BRAZE_LOCAL_GPU_LAYERS` explícito
ya no se fuerza `Host`; arranca en `Device` y confía en la escalera. Los brazos
corridos con capas fijas antes de este commit necesitan
`BRAZE_LOCAL_KV_OFFLOAD=host` para reproducirse.

### Medición (5 brazos, `default.toml`, seed 42, 19 tareas comparables)

Walltime sobre las **9 tareas completadas en los cinco** (comparar promedios
crudos mete el cap de 360s como si fuera una medición):

| Brazo | Walltime |
|---|---|
| v3 21-jul, 14 capas, KV-VRAM (pre-`483f8e2`) | 16.1s |
| 25-jul, 33 capas, KV-host (auto-fit v1) | 20.7s |
| 25-jul, 14 capas, KV-host (regla vieja) | 29.2s |
| 25-jul, 14 capas, KV-VRAM (repro del v3) | 17.2s |
| **25-jul, 21 capas, KV-VRAM (medido)** | **15.0s** |

**1.38× sobre el auto-fit v1, 1.95× sobre la regla vieja**, y mejor que el
baseline histórico. Pass rate sin cambios (9/19; timeouts 8 vs 9). En vivo el
fit elige distinto según el régimen: gemma-12B **21 capas/Device**, gpt-oss
**25 capas/Device** (sus overrides MoE liberan bastante VRAM para que el KV
también quepa).

### Efecto en gpt-oss:20b, el modelo default del proyecto

Re-corrido con parámetros **idénticos** a `sweep-rank-oss.json` (misma suite y
fingerprint, 3 reps, seed 42, timeout 180); la única diferencia es que aquel
brazo fue CPU puro y este usa el reparto que decide el fit (25 capas, KV en
VRAM):

| | CPU (referencia) | GPU 25c KV-VRAM (medido) |
|---|---|---|
| pass rate | 57/57 | 57/57 |
| pass^k | 19/19 | 19/19 |
| timeouts | 0 | 0 |
| **avg walltime** | **41.4s** | **12.1s** |

**3.4× más rápido con McNemar p=1: cero pares discordantes**, las 57 tareas
dieron exactamente el mismo resultado. El mejor número del proyecto no se
movió; solo tarda un tercio.

Dos precisiones para no sobrevender: el 3.4× es el efecto **combinado** de
pasar a GPU *habilitado por* el auto-fit y el placement medido (antes la GPU
exigía adivinar capas y arriesgar OOM), no la atribución limpia del placement
—esa la dio el estudio de 5 brazos de gemma. Y el `pass^k=19/19` hay que
leerlo junto con el hallazgo de sampling de abajo: con greedy las
repeticiones son casi deterministas.

### Segundo bug de la misma clase, cazado en vivo

Con el placement medido, una máquina **sin GPU** hacía: fit `Device` → 0 capas
→ probar `Host` → 0 capas → devolver **`Host`**. Es decir `offload_kqv(false)`
y micro-batch de 128 en CPU puro: la misma regresión que se había arreglado
horas antes normalizando el `-1`, reintroducida por otro camino. Ahora 0 capas
siempre termina en el camino rápido.

La clase de bug a vigilar: **decidir el placement sin preguntar si hay GPU**.
Mordió dos veces el mismo día, las dos veces la destapó correr `braze tune` en
la máquina sin GPU — nunca los tests ni un sweep en Nitro.

## `braze tune` (2026-07-25)

Idea **#8** de la auditoría, construida sobre el auto-fit: corre el fit contra
un GGUF y reporta el reparto **sin cargar los pesos para inferencia** (el probe
usa `no_alloc`, así que es barato). Su valor es *fitear una vez y fijar el
número*: exportando el `BRAZE_LOCAL_GPU_LAYERS` que imprime, un sweep se ahorra
el fit por tarea y queda reproducible en vez de re-adivinado — que era
exactamente el dolor "reproducibilidad, re-adivinar" de la tabla.

```
braze tune ~/models/gpt-oss-20b-MXFP4.gguf --n-ctx 8192
braze tune qwen2.5:3b --emit-config fit.toml   # ref de Ollama tambien sirve
```

El reporte incluye el **placement del KV** además de las capas: desde que lo
decide el fit midiendo, sin ese dato el reporte no describiría la
configuración que realmente se va a correr.

`--emit-config` escribe TOML (o a stdout con `-`). **braze no lee ese
archivo**: es un registro reproducible del fit, y los comentarios mapean cada
valor a su variable de entorno, que es la vía de consumo real. Se decidió así
para no meter plumbing de config nuevo por una feature cuyo valor es el número.

Requiere `--features local`; sin él el subcomando existe pero devuelve el mismo
error explicativo que el backend `local`.

### Bug cazado por verificarlo en vivo

La primera corrida de `tune` en la máquina **sin GPU** reportó `capas a GPU -1`
y "el modelo entero entró en la GPU" — falso. Sin device, `common_fit_params`
no toca `n_gpu_layers` y lo deja en su default `-1`, que significa "todas las
que quepan"… o sea **cero** cuando no hay ninguna.

El mensaje engañoso era lo de menos. `kv_on_host()` trata cualquier valor
distinto de 0 como "hay offload", así que en CPU puro se estaba activando el
`n_ubatch=128` y el `offload_kqv(false)` pensados para cuidar VRAM —
**frenando el prefill en una máquina donde no hay VRAM que cuidar**, justo el
perfil del i7 sin GPU de Claudio.

Corregido normalizando el `-1` a `0` cuando `supports_gpu_offload()` es falso.
En Nitro el `-1` de qwen2.5:3b sigue siendo legítimo (ahí sí caben todas), así
que la normalización solo actúa donde corresponde. Es un caso de manual de por
qué en este proyecto compilar ≠ funcionar: el bug pasó clippy, 217 tests y un
sweep entero en GPU sin manifestarse, porque solo aparece sin GPU.

## Sampling: DRY + min-p, y un hallazgo incómodo (2026-07-25)

Ideas **#6** de la auditoría (`sampler.rs` de mistral.rs: DRY anti-repetición
por n-gramas + min-p). Al ir a implementarlas apareció algo más grande.

### El hallazgo: el LocalBackend nunca sampleó

`CompletionRequest` **no lleva temperatura**, `local.rs` nunca la consultó, y
en el bench `build_local()` **ni siquiera recibe** el parámetro `sampling` que
sí reciben los demás backends. O sea: el LocalBackend siempre usó
`LlamaSampler::greedy()`. Consecuencias, en orden de gravedad:

1. **`braze-bench --temperature` ha sido un no-op en todo brazo local.** La
   garantía **N-34** ("un solo régimen de sampling por sweep",
   `backend_spec.rs:427`) no se cumple para `local`. **Hueco abierto**: se
   decidió no taparlo en el mismo commit, porque plomarlo cambia el régimen de
   todos los brazos locales futuros y los vuelve incomparables con lo medido.
2. **El estudio de paridad LocalBackend vs Ollama** (McNemar p=0.22, arriba en
   este doc) comparó **greedy contra temp 0.2** — es un confound de esa
   conclusión, no un empate limpio.
3. **`pass^k` en brazos locales mide menos de lo que aparenta.** Con greedy las
   repeticiones son deterministas salvo no-determinismo de punto flotante en
   GPU, y los seeds por repetición no hacen nada porque greedy los ignora. El
   `pass^3=100%` de gpt-oss —que se presenta como el hallazgo de confiabilidad
   del proyecto— es en buena parte **determinismo, no robustez**. Encaja con
   algo que se vio sin darle importancia en el sweep de gemma: las 3 reps de
   una tarea daban resultados casi calcados.

### Lo implementado

`LocalSampling` (temperatura, min-p, top-k, DRY con sus parámetros, seed),
configurable por entorno (`BRAZE_LOCAL_TEMP`, `_MIN_P`, `_TOP_K`, `_DRY`,
`_DRY_BASE`, `_DRY_ALLOWED`, `_DRY_LAST_N`, `_SEED`) o por
`LocalBackend::with_sampling()`. El régimen vive en el **backend**, mismo
patrón que `AnthropicBackend::with_temperature` — no toca `CompletionRequest`,
que es contrato congelado.

**El default sigue siendo greedy**, y hay un test que lo fija con la razón
escrita: todo lo medido del LocalBackend salió con greedy, así que si alguien
cambia el default debe romperse el test antes que la comparabilidad de estos
documentos. DRY/min-p entran como palanca que **se gana su default por bench**,
misma doctrina que KV-quant y el stencil.

Orden de la cadena, el canónico de llama.cpp: **DRY → top-k → min-p →
temperatura → `dist`**. Invertirlo cambia qué distribución ve cada etapa.

### El detalle de corrección que el diseño tuvo que cubrir

El stencil **swapea el sampler** cada vez que abre y cierra una tool call
(cuatro sitios: apertura harmony, cierre harmony por marcador, cierre harmony
por cursor de args, y cierre del envelope qwen). Con greedy es inocuo —no
tiene estado—, pero **DRY lleva la historia de n-gramas generados**: un
sampler nuevo la perdería en cada tool call y DRY quedaría medio apagado justo
en las generaciones largas, que son las que degeneran.

`rebuild_free_sampler()` re-alimenta los tokens ya emitidos con `accept_many`,
y **solo cuando DRY está activo** — acumular tokens y re-aceptarlos cuesta, y
no compra nada para samplers sin estado.

Verificado en vivo en Nitro (gpt-oss): el camino default y el de
`DRY=0.8 MIN_P=0.05 TEMP=0.7` generan ambos bien y producen salidas
**distintas**, o sea que la cadena está activa y no ignorada en silencio.

**Pendiente**: el A/B que le dé o le quite el default. El candidato no es
gpt-oss (satura en 57/57, no tiene dónde mejorar) sino **gemma4:e4b**, cuyos 3
fallos sistemáticos de `single_tool` son la clase donde la degeneración muerde.

## Referencias

- Spike: `scratchpad/braze-local-spike/` (throwaway, fuera del workspace).
- Decisión anterior (cerrada, capacidad): `docs/local-backend-stencil-design.md`.
- Hallazgos que motivan: `docs/bitacora-harness-modelo.html` (#1, #17).
- Contrato del backend: `ollama.rs`/`ollama_wire.rs` como plantilla;
  escalera de rescate en `braze-engine/src/rescue.rs`.
