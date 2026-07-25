# Auditoría de runtimes de inferencia local — ideas para el LocalBackend de braze

> **Estado (2026-07-25):** Auditoría de 8 proyectos (mistral.rs, server de
> llama.cpp, vLLM, TGI, Cortex, LocalAI, llamafile, koboldcpp, candle,
> +menciones) como fuente de ideas de desarrollo para el **LocalBackend**
> de braze (wrapper Rust in-process sobre llama.cpp vía `llama-cpp-2`).
> Investigación paralela por subagentes contra fuentes reales (código,
> READMEs, docs), cada afirmación marcada verificado/inferido. Cerrado con
> verificación propia de la superficie de API de `llama-cpp-2 v0.1.151`
> instalado — qué es implementable **hoy** sin tocar el FFI. Protagonista:
> **mistral.rs**. Complementa `docs/local-backend-design-2026-07-20.md`.

## 0. TL;DR — las 5 ideas de mayor ROI

Todas **implementables hoy** salvo donde se indica; el `llama-cpp-2 v0.1.151`
que ya usamos expone los primitivos (verificado, § 7).

1. **Auto-fit de capas GPU** — matar el `BRAZE_LOCAL_GPU_LAYERS` adivinado
   (que me costó el mis-diagnóstico de "techo de HW" y los 29/57 timeouts
   del sweep gemma-4-12B). Existe como algoritmo greedy simple en DOS
   fuentes: `mistralrs::tuning.rs`+`auto_device_map.rs` y
   `llama.cpp::common/fit.cpp`. Los inputs (VRAM libre por device, bytes
   por capa del GGUF, KV por token) los da `llama-cpp-2`. **Dolor #1.**
2. **KV cuantizado (`q8_0`/`q4_0`)** — `with_type_k`/`with_type_v` YA están
   en `llama-cpp-2`. **IMPLEMENTADO Y VERIFICADO EN VIVO (2026-07-25, rama
   `local-kv-quant`)** vía `BRAZE_LOCAL_KV_TYPE`. **Hallazgo que corrige la
   promesa:** el KV cuantizado **requiere flash-attn, que gpt-oss/Harmony NO
   soportan (attention sinks)** → `new_context` devuelve null. Funciona con
   **qwen2.5:3b y familia estándar**; con gpt-oss braze **degrada con gracia
   a f16** (sin ganancia). O sea NO acelera roam ni los sweeps (usan
   gpt-oss). El throughput real de gpt-oss en 6GB lo dan la idea #1 (auto-fit,
   más capas) y la #4 (`-march`/tinyBLAS en CPU), no el KV-quant.
3. **Evitar el re-prefill del loop agéntico** — cada ronda de braze
   re-envía system+tools+historial; hoy probablemente re-prefilea. Dos
   caminos: mantener el `llama_context` vivo entre rondas (prefix reuse de
   KV) y/o **ContextShift** de koboldcpp (`kv_cache_seq_rm`+`shift`). Gran
   ahorro de latencia, sobre todo en CPU.
4. **CPU: `-march`/tinyBLAS — VERIFICADO (2026-07-25), es un no-problema.**
   El build de `llama-cpp-sys-2` NO es ggml genérico: compila con
   `-mavx -mavx2 -mbmi2 -mf16c -mfma -msse4` + `GGML_CPU_REPACK:ON` (el
   sgemm/tinyBLAS). AVX512 OFF (irrelevante para el i7 Intel de Claudio,
   fusionado apagado en 12–14ª gen) y `GGML_NATIVE:OFF` (decisión de
   portabilidad; delta ~nulo sobre AVX2+FMA). **Sin acción de código.**
   Queda como experimento OPCIONAL la contraintuición: en loops prompt-heavy
   en CPU, **F16 puede ganarle a Q4 en walltime** — un A/B en braze-bench.
5. **Speculative decoding (prompt-lookup)** — `llama-cpp-2` tiene
   `speculative.rs`. El prompt-lookup (mira el prompt, cero modelo extra)
   es ideal para el dominio de edición de código de braze (el output repite
   fragmentos del input).

**Corrección honesta de dos hipótesis mías previas** (§ 1): llama.cpp **sí**
maneja harmony/gpt-oss y GBNF-para-tools nativo. braze **no** está adelante
ahí. Su diferenciador real es el **acceso al sampler in-process** (ablación
por token) y ser un **harness agéntico**, no "ser dueño del parser vs
llama.cpp" (lo es vs **Ollama**, que era el buggy).

---

## 1. Encuadre y corrección de hipótesis

Contexto: el LocalBackend de braze reimplementó en Rust cosas que la capa
`common/` + `tools/server/` de llama.cpp ya resuelve en C++ **por encima
del boundary de `libllama`** — el mismo código que `llama-cpp-2` NO expone.
De ahí que braze tuviera que hacerlo a mano. Dos consecuencias auditadas:

- **Harmony/gpt-oss nativo: llama.cpp SÍ.** `common_chat_params_init_gpt_oss`
  (`chat.cpp:1111`), detectado por `<|channel|>` en el template, separa
  `analysis`/`commentary`/`final`, y hasta corrige quirks reales del modelo
  (regla `stray_commentary`, `chat.cpp:1186`). El `HarmonyParser` de braze
  **duplica** funcionalidad madura. [verificado]
- **GBNF para tool-calls: llama.cpp SÍ.** Grammar triggers (GBNF *lazy*
  activadas por regex) + un auto-parser que **deriva la gramática del
  template** renderizándolo dos veces y difeando (`docs/autoparser.md`). El
  stencil de braze no es único. [verificado]

**Lo que sí sostiene a braze** (y hay que decirlo con precisión): braze evita
el parser server-side de **Ollama** (el buggy, incidentes #1/#17), no el de
llama.cpp (robusto). Su valor real y verificado por diseño: **ablación por
token in-process** (ningún harness API-bound la tiene) y el **loop de
reparación agéntico** que ya absorbe los `schema_fail` río abajo. Eso es
laboratorio real; "llama.cpp no sabe parsear harmony" era falso.

---

## 2. mistral.rs — protagonista

`EricLBuehler/mistral.rs` · MIT · **100% Rust, sin llama.cpp** (sobre
`candle` + kernels CUDA/Metal propios) · 7.5k★, v0.9.0, push del 2026-07-23
(vivísimo). Es un **runtime de inferencia completo** (texto+visión+audio+
difusión), no un harness. El contraste de filosofía con braze es el eje:
**mistral.rs = dueños de todo el stack numérico; braze = dueños del harness,
motor numérico prestado (llama.cpp)**.

### 2.1 La mina de oro: auto-fit (`tuning.rs` + `auto_device_map.rs` + `memory_usage.rs`)

Lo que a braze le falta entero, ahí como algoritmo greedy portable:

- **`memory_usage.rs::MemoryUsage::query()`** → VRAM libre por device (CUDA
  `mem_get_info()`, Metal `recommended_max_working_set_size`, CPU `sysinfo`,
  iGPU con fracción `MISTRALRS_IGPU_MEMORY_FRACTION=0.75`).
- **`auto_device_map.rs::get_device_layers()`**: greedy — itera devices de
  mayor a menor, saca capas mientras
  `used + layer_size + kv_bytes_per_tok·layers ≤ device_cap`, donde
  `device_cap = available − max(2%·available, 512 MB)` (constantes
  `GPU_RESERVE_FRACTION=0.02`, `GPU_MIN_RESERVE_BYTES=512 MiB`). El device 0
  carga además lo no-mapeado (embeddings/lm_head). Capas sobrantes → CPU
  (no crashea; degrada a híbrido).
- **`tuning.rs::auto_tune()`** + comando `mistralrs tune`: enumera candidatos
  de quant por calidad descendente (`[None,Q8_0,Q6K,Q5K,Q4K,Q3K,Q2K]`),
  estima tamaño (`params × dtype_size ÷ pack_factor`), corre el fit → estado
  `Fits`/`Hybrid`/`TooLarge`, calcula **context-room**
  `ctx_max = (vram − model)/(kv_elems_per_tok × dtype × n_layers)`, y elige
  el mejor que quepa por perfil (`quality`/`balanced`/`fast`). Emite tabla +
  `recommended_command` + **TOML reproducible** (`--emit-config`) + `--json`.

  → **`braze tune`**: portar esto es la idea #1. braze solo necesita
  **calcular** `n_gpu_layers` (llama.cpp ya hace el split parcial); los inputs
  salen del header GGUF + `llama-cpp-2` (§ 7). El headroom `max(2%,512MB)`
  mata el OOM que hoy crashea. `--emit-config` a `.braze/local.toml` encaja
  con la doctrina de reproducibilidad.

### 2.2 Resto (secundario o fuera de scope)

- **ISQ** (in-situ quant al cargar, sin tocar checkpoint) + **UQFF**
  (artefacto pre-quantizado): irrelevante — braze usa GGUF ya cuantizado.
- **PagedAttention** (`mistralrs-paged-attn`) con **prefix caching** por hash
  de contenido + ref-count + LRU O(1) + reuso cross-secuencia, y **KV FP8**.
  Idea (no port): el prefijo system+tools se reusa entre turnos/tareas de un
  sweep. braze puede lograr ~80% reusando `llama_state_seq_*` sin
  reimplementar PagedAttention.
- **Tool parsers por familia** (`tools/parsers/`: harmony, qwen, gemma4,
  deepseek…) con `fix_broken_json()` (repara `"arguments":"{`), `flexible_args`
  (objeto o string), y **filtro de alucinaciones** contra `known_tool_names`.
  → **Robar directo**: el reparador de args + el drop de tools inexistentes,
  para la escalera de rescate de braze. Barato, sin gramática.
- **Grammar**: usa **`llguidance`** (Lark + JSON-schema, no GBNF crudo), con
  **strict-mode `anyOf` una variante por tool**. Es la evolución natural del
  stencil de braze (envelope único → gramática por tool). Idea de A/B.
- **Sampler** (`sampler.rs`): incluye **DRY** (anti-repetición por n-gramas),
  min-p, penalties asimétricas, logit bias. → sumar **DRY+min-p** ataca la
  degeneración de modelos chicos dentro del stencil.
- **AnyMoE** (denso→MoE en runtime, expertos LoRA), **cliente MCP** (3
  transportes, semáforo 10, timeout 30s), **code-exec sandboxed**:
  capacidades de laboratorio, no prioritarias.

### 2.3 Dónde braze está a la par o adelante

- **HarmonyParser propio "dueño desde el token"** (rescues=0, 57/57): braze
  está conceptualmente alineado con evidencia propia; mistral.rs no persigue
  ese objetivo de laboratorio (y su arquitectura server-side reintroduce la
  clase de bug que motivó salir de Ollama).
- **Foco agéntico como producto** (permisos, skills, compactación, TUI): braze
  claramente adelante; la capa agéntica de mistral.rs es accesoria.
- **Mantenimiento vía FFI**: braze hereda gratis el catálogo GGUF + kernels de
  llama.cpp sin mantener miles de líneas de CUDA/Metal. Para el alcance de
  braze (modelos chicos, no multimodal) es ventaja, no carencia.

---

## 3. Server de llama.cpp — el motor que ya usamos

`ggml-org/llama.cpp`, `tools/server/` + `common/`. **Lo relevante NO vive en
`libllama`** (lo que envuelve `llama-cpp-2`), sino en `common/` — o sea es
exactamente lo que braze reimplementó. Pero los **primitivos** que esa lógica
usa sí están a nivel librería (§ 7), así que los algoritmos son portables.

- **Auto-fit (`common/fit.cpp::common_fit_params`)** — la referencia canónica,
  gemela del `tuning.rs` de mistral.rs. Default `-ngl auto` + `-fit on`;
  algoritmo: (1) probe con `no_alloc=true` (solo metadata) → breakdown por
  device `{total,free,model,context,compute}` vía `ggml_backend_dev_memory`;
  (2) reduce contexto (interpolación lineal a múltiplo de 256, min 4096); (3)
  rellena devices back-to-front con capas densas; para MoE empuja expertos a
  RAM con `tensor_buft_overrides` (regex `blk\.N\.ffn_(gate|up|down).*`).
  Margen default **1 GiB libre/device**. Knobs MoE manuales: `-cmoe`/
  `-ncmoe N` (expertos de las primeras N capas a CPU).
- **KV/attn**: `-ctk/-ctv` (default f16; permite `q8_0`/`q4_0`), `-fa auto`
  (flash-attn; **requerido** para V-cache cuantizado), `--context-shift`,
  `--swa-full`, `-kvu` (KV unificado), `-cram` (prompt-cache en RAM),
  `--ctx-checkpoints`.
- **Slots/batching**: `-np auto`, `-cb` (continuous batching on), `-b`/`-ub`.
- **Tool-calling**: `--jinja` + parsers nativos (Llama/Hermes/Qwen/Functionary/
  **gpt-oss**/DeepSeek-R1…) + fallback genérico. `--reasoning-format`,
  `--reasoning-budget N`. **GBNF triggers** para el tool-call. **Auto-parser**
  diferencial (deriva parser+gramática de cualquier template).
- **OOM**: estrategia **proactiva** (estima con `no_alloc` + margen), estados
  tipados `SUCCESS/FAILURE/ERROR`. No un retry reactivo — "estima bien y deja
  margen, reduce ctx antes de fallar". → adoptar el estado tipado da el
  "fail-fast de brazo en el bench" que CLAUDE.md lista pendiente.

**Ideas top para braze**: portar `common_fit_params` (auto-fit + degradación
ctx→capas→MoE-a-CPU), exponer `-ncmoe`-equivalente, KV-quant, flash-attn
`auto`, y el arranque con estado tipado en vez de crash.

---

## 4. vLLM + TGI — scheduling/memoria (mayormente "si braze suma servidor")

braze es hoy single-session in-process; ~80% de estos dos aplica solo con
concurrencia real. Lo separo honesto:

**Aplicable hoy (single-session):**
- **Prefix reuse multi-turno = el loop de braze.** El caso estrella del
  automatic prefix caching de vLLM es conversación multi-turno donde el
  prefijo (system+historial) se reusa — precisamente braze. In-process:
  mantener el mismo `llama_context` vivo y no re-evaluar el prefijo que ya
  está en el KV. **Mayor ROI inmediato; verificar si el LocalBackend reusa o
  reconstruye el contexto entre rondas.**
- **KV quantization** (`kv_cache_dtype=fp8` en vLLM → `type_k/v` en braze).
- **Speculative decoding** (draft / n-gram / **prompt-lookup**) — reduce
  latencia hoy; es capacidad de llama.cpp, no reimplementar (§ 7 lo confirma).
- **Presupuesto de tokens por step** (`{req_id: num_tokens}` del scheduler V1)
  — modelo mental limpio para el split planner/executor o TTC de braze.

**Solo si braze suma modo servidor:**
- **PagedAttention / block manager**: no reimplementar — delegar en los slots
  de llama.cpp (`llama_batch` con múltiples `seq_id`) si algún día hay N
  sesiones.
- **Continuous batching + preemption** (recompute > swap, vLLM V1).
- **Arquitectura router-Rust + workers (TGI)**: el blueprint natural para un
  "braze servidor" — el borde Rust ya es su fortaleza. Verbos gRPC
  `prefill/decode/filter_batch` como diseño de API.
- **`gpu_memory_utilization` + profiling run**: llama.cpp ya reserva el KV en
  single-session; el `warmup()` de TGI es la versión mínima adoptable.

**Bonus verificado (TGI):** tool-calling = "grammar que enmascara tokens, tools
= elegir *one or none*" — idéntico en espíritu al stencil de braze, y TGI
confirma tu hallazgo: aun con grammar conviene poner el schema en el prompt.

---

## 5. Ollama-likes + landscape Rust

- **llamafile (Mozilla-Ocho, Apache-2)** — clave para "i7 sin GPU". **tinyBLAS**
  (kernels GEMM CPU): +30–500% prompt-eval con F16/Q8_0, ~40% con Q4; **F16/F32
  procesan prompts ~2× más rápido que los cuantizados** (prompt-eval); AVX512
  hasta 10× (Zen4). Se upstreó a llama.cpp (`sgemm.cpp`, PR #6414). → **braze
  probablemente ya lo hereda**, PERO hay que **verificar que el build FFI use
  `-march=native`/detección AVX512-NEON**; si no, deja 2–10× en la mesa. Y
  **A/B F16 vs Q4 en CPU** en braze-bench: en loops prompt-heavy F16 puede
  ganar walltime (contraintuitivo).
- **koboldcpp (AGPLv3 — solo ideas, no código)** — **ContextShift**: KV-cache
  shift (`llama_kv_cache_seq_rm`+`seq_shift`) para remover tokens viejos y
  agregar nuevos **sin reprocesamiento**. Es la contraparte *física* de la
  compactación *lógica* de braze (que igual paga re-prefill). + samplers
  **DRY**, min-p, XTC, Mirostat.
- **LocalAI (Go, MIT)** — patrón **backend como artefacto separado tras
  interfaz estable** (gRPC): un build roto de un engine no hunde el core. →
  aislar el LocalBackend como **sidecar out-of-process** (`braze-local-server`
  con feature `local-cuda`) que el core habla por IPC — ataca directo la
  "fragilidad de build FFI" y los "dos tropiezos por binarios desincronizados
  del 21-jul". braze ya tiene el trait `ModelBackend`; el sidecar encaja.
- **Cortex.cpp (archivado) / ramalama** — el gesto de producto `detect` +
  `activate`: introspección de GPU/VRAM → fijar `n_gpu_layers` automático (la
  aritmética la sacas del GGUF, ellos dan el UX). ramalama: detecta acelerador
  → elige artefacto/flags (para braze: feature `local` vs `local-cuda`).
- **candle (HF, Rust puro)** — **tu descarte sigue de pie**: no es cargador
  GGUF genérico (grafo por arquitectura a mano, sin tool-calling, sin
  plantilla-desde-metadata). PERO nicho real: **cero fragilidad FFI** → un
  *fallback pinned a qwen2.5:3b puro-candle que siempre compila*, bote
  salvavidas para "el binario CUDA se desincronizó otra vez". Secundario.
- **llama-cpp-python** — su handler genérico `chatml-function-calling` como
  alternativa a mantener 1 plantilla por familia; `response_format`=JSON-Schema
  como equivalente del stencil.

---

## 6. Temas transversales (dedup entre fuentes)

- **Auto-fit de VRAM aparece en 2 implementaciones maduras** (mistral.rs
  `tuning.rs`, llama.cpp `common_fit_params`) con el mismo esqueleto: probe →
  device memory → greedy con headroom → degradar ctx/capas/MoE-a-CPU. **Es la
  convergencia más clara de toda la auditoría.** braze debe portarlo.
- **KV-quant + flash-attn** aparece en todos (vLLM fp8, llama.cpp `-ctk`,
  mistral.rs FP8 paged) — y está en `llama-cpp-2` hoy.
- **Evitar re-prefill** (prefix caching vLLM/mistral, ContextShift kobold,
  ctx-checkpoints llama.cpp) — múltiples caminos al mismo ahorro; el loop
  agéntico de braze es el caso de uso ideal.
- **Ninguno tiene un auto-fit "mágico" mejor que Ollama** para el número
  exacto de capas; todos usan el mismo greedy con estimación GGUF. La solución
  de braze será la propia, portando ese greedy.

---

## 7. Verificación: qué expone `llama-cpp-2 v0.1.151` (implementable HOY)

Grep del crate instalado — **casi todas las ideas top no necesitan FFI nuevo**:

| Idea | API en `llama-cpp-2` | Estado |
|---|---|---|
| **KV cuantizado** | `LlamaContextParams::with_type_k(KvCacheType::Q4_0)` / `with_type_v` (`get_set.rs:519`) | ✅ expuesto |
| **Flash-attention** | `with_flash_attention_policy(llama_flash_attn_type)` (`get_set.rs:313`) | ✅ expuesto |
| **VRAM libre por device** | `list_llama_ggml_backend_devices()` vía `ggml_backend_dev_get_props` (`lib.rs:499`) | ✅ expuesto |
| **¿GPU offload disponible?** | `supports_gpu_offload()` (`llama_backend.rs:74`) | ✅ expuesto |
| **MoE → CPU (buft overrides)** | `buft_overrides: Vec<llama_model_tensor_buft_override>` (`model/params.rs:150`) | ✅ expuesto |
| **`n_gpu_layers`** | `with_n_gpu_layers` (ya lo usamos) | ✅ |
| **Speculative decoding** | módulo `speculative.rs` (MTP, `MtpSpeculativeParams`) | ✅ experimental |

**Conclusión de la verificación:** el auto-fit (device memory + n_gpu_layers +
buft_overrides), KV-quant, flash-attn y speculative son **todos implementables
sobre el crate actual**, sin tocar el `-sys`. El único que requeriría más
excavación es el **prefix-reuse/ContextShift** (`llama_kv_cache_seq_*`,
`llama_state_seq_*`) — verificar su wrapper en `llama-cpp-2` antes de accionar.

---

## 8. Tabla maestra priorizada

| # | Idea | Fuente(s) | Dolor | ¿Hoy? | ¿`llama-cpp-2` lo expone? | Esfuerzo |
|---|---|---|---|---|---|---|
| 1 | **Auto-fit `n_gpu_layers`** (greedy + headroom, degradar ctx/MoE) | mistral.rs `tuning.rs`, llama.cpp `fit.cpp` | capas manuales, OOM crashea | ✅ | ✅ (device mem + buft) | medio |
| 2 | **KV-quant `q8_0/q4_0` + flash-attn** | todos | throughput/VRAM 6GB | ✅ | ✅ | bajo |
| 3 | **Evitar re-prefill** (ctx vivo / ContextShift) | vLLM, mistral, kobold | latencia loop agéntico / CPU | ✅ | ⚠️ verificar `seq_*` | medio |
| 4 | **CPU `-march`/tinyBLAS + A/B F16 vs Q4** | llamafile | i7 sin GPU (Claudio) | ✅ | build flags | bajo (verif) |
| 5 | **Speculative (prompt-lookup)** | vLLM, llama.cpp | latencia edición de código | ✅ | ✅ (`speculative.rs`) | medio |
| 6 | **DRY + min-p en el sampler** | kobold, mistral | degeneración modelos chicos | ✅ | vía sampler API | bajo |
| 7 | **`fix_broken_json` + drop tools alucinadas** | mistral `tools/` | robustez rescate | ✅ | N/A (lógica Rust) | bajo |
| 8 | **`braze tune --emit-config` TOML** | mistral `tune` | reproducibilidad, re-adivinar | ✅ | (sobre #1) | bajo |
| 9 | **Estado tipado de arranque (no crash)** | llama.cpp `fit.h` | fail-fast de brazo en bench | ✅ | — | bajo |
| 10 | **Strict-mode `anyOf` por tool** | mistral llguidance | evolución del stencil | ✅ | (grammar API) | medio |
| 11 | **Sidecar out-of-process** | LocalAI | fragilidad build FFI | ✅ | — | medio-alto |
| 12 | **Fallback puro-candle (qwen)** | candle | bote salvavidas FFI roto | ✅ | (crate nuevo) | alto |
| — | Router Rust + workers, PagedAttention, continuous batching | TGI, vLLM | (solo si braze suma servidor) | ❌ | delegar en slots llama.cpp | — |

## 9. Secuencia recomendada de prototipado

0. ~~#2 KV-quant~~ **HECHO** (rama `local-kv-quant`) — pero verificado que NO
   ayuda a gpt-oss (degrada a f16); solo a qwen/estándar. No era el acelerador
   del 6GB que se creía. Lección: el in-vivo corrigió la asunción.
1. ~~#4 verificar `-march`~~ **VERIFICADO — no-problema**: el build ya lleva
   AVX2+FMA+F16C+BMI2+repack. Sin acción. (Queda opcional el A/B F16-vs-Q4 en
   CPU como dato de paper.)
2. **#1 (auto-fit)** — ahora el siguiente natural: la de mayor impacto y el
   **verdadero
   acelerador de gpt-oss en 6GB** (más capas → menos CPU). Portar el greedy de
   `fit.cpp`/`tuning.rs`; cierra el OOM-que-crashea y el `BRAZE_LOCAL_GPU_LAYERS`
   adivinado. Los primitivos (`list_llama_ggml_backend_devices`,
   `tensor_buft_overrides`) ya están en `llama-cpp-2`.
4. **#3 (re-prefill)** — verificar primero cómo el LocalBackend maneja el
   `llama_context` entre rondas; puede ser ganancia gratis.
5. El resto según prioridad (#5 speculative, #6 samplers, #7 rescate).

**Nota de procedencia:** los detalles de código de mistral.rs/llama.cpp/vLLM
provienen de lectura de fuentes reales por subagentes (marcado
verificado/inferido en los briefs originales); la superficie de `llama-cpp-2`
(§ 7) la verifiqué directo sobre el crate instalado. Los puntos "⚠️ verificar"
son los únicos abiertos antes de accionar.
