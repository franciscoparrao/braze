# Dimensionamiento del upgrade de Nitro (2026-07-23)

> Cierra el pendiente "dimensionar upgrade de hardware de Nitro (GPU
> 12-16GB vs 32GB RAM)". Escrito y luego **corregido** con las mediciones
> tomadas al intentar correr la redo del KDE de roam en gpt-oss:20b por el
> LocalBackend. El primer diagnóstico ("techo de hardware") resultó
> **equivocado**: los crashes eran un bug de software del LocalBackend,
> ya arreglado. Lo que queda es un techo de *velocidad*, no de capacidad.

## Estado actual medido

| Recurso | Valor | Fuente |
|---|---|---|
| GPU | RTX 3050 **6GB** Laptop | `nvidia-smi` |
| RAM sistema | **14 GiB** | `free -h` |
| Cores | 12 | `nproc` |
| CUDA / driver | 12.4 / 595.71.05 | `nvcc`, `nvidia-smi` |

`gpt-oss-20b-MXFP4.gguf` pesa **12 GB** — 2× la VRAM. No cabe entero en
la GPU; corre con offload parcial (N de 24 capas a GPU, resto en CPU).

## Lo que primero parecía techo de hardware, y no lo era

Los intentos iniciales crasheaban con `CUDA error: out of memory` en la
2ª ronda de una sesión agéntica. Se concluyó, mal, que 6 GB era un techo
insalvable. Pero **Ollama corre el mismo modelo en la misma máquina sin
crashear** — la pista de que el problema no era hardware. Al medir su
receta (`ollama ps`): VRAM **plana ~4,7 GB a cualquier num_ctx**, 8 capas
a GPU. Imposible si el KV cache estuviera en VRAM. Ollama **mantiene el KV
en el host (RAM)** y usa **micro-batches chicos**. El LocalBackend hacía
lo contrario por usar los defaults de llama.cpp.

**Tres bugs de software, arreglados** (commit `483f8e2`, `braze-model/src/local.rs`):

1. **KV cache en VRAM** (`offload_kqv=true` default) → crecía con el
   contexto y reventaba los 6 GB. Fix: `with_offload_kqv(false)` con
   offload parcial → KV en host.
2. **Compute buffer gigante** (`n_ubatch=512` default): lo dimensiona
   `n_ubatch × contexto` y vive en VRAM; crecía hasta abortar al llenarse
   el contexto. Fix: `n_ubatch=128` (`BRAZE_LOCAL_UBATCH`).
3. **`n_batch` mal** → `GGML_ASSERT(n_tokens_all <= n_batch)`. Fix:
   `n_batch = n_ctx` (braze decodifica el prompt entero de una).

**Resultado medido tras el fix:** VRAM **plana ~4,4–5,4 GB** a ctx 16384
(8–10 capas), sesiones de 20–45 min **sin un solo crash**. La máquina de
6 GB **sí corre** gpt-oss:20b agéntico. El diagnóstico de "hardware" era
un bug de gestión de memoria.

## El techo que SÍ es hardware: velocidad de generación

Con la memoria resuelta, la redo del KDE aún no aterrizó — por **tiempo**.
gpt-oss:20b eligió reescribir el archivo entero (~500 líneas ≈ 5000
tokens de generación), y a **10 de 24 capas en GPU (~42%)** la generación
es mayoritariamente CPU: no completó ni en 45 min. No crashea, no le
alcanza el reloj.

La velocidad de generación la fija cuánto del modelo vive en GPU. En 6 GB
solo caben ~10-11 capas de 24 (con el KV ya fuera de la VRAM), así que
>50% del cómputo sigue en CPU. Ese es el límite físico real, y es de
throughput, no de "no corre".

## Qué desbloquea cada upgrade

### Opción A — GPU de 16 GB (RTX 4060 Ti 16GB nueva, o 3090/4080 usada)

- gpt-oss:20b (12 GB) cabe **entero** + KV/buffers → las **24 capas en
  GPU**. Generación de orden ~10-20× más rápida que a 10/24 capas.
- La reescritura de archivo que hoy no cabe en 45 min pasaría a ~2-3 min.
  **Es la que destraba el trabajo agéntico pesado.** Fix de *velocidad*.
- Una **24 GB usada (RTX 3090, ~USD 600-700)** además deja espacio para
  el 26B-A4B MoE.

### Opción B — 32 GB de RAM

- Desbloquea modelos más grandes en CPU (26B-A4B IQ4_XS ~13,6 GB) y
  contextos grandes sin swap.
- **No arregla la velocidad**: sigue siendo inferencia CPU. El techo que
  bloquea la redo del KDE (throughput de generación) **persiste**.

## Recomendación

**La GPU de 16 GB (Opción A) es el upgrade de mayor palanca**, ahora por
la razón correcta: no porque la máquina "no dé" (sí da, tras el fix de
software), sino porque la **velocidad de generación** es el único límite
que queda, y meter las 24 capas en GPU la multiplica ~10×. Los 32 GB de
RAM desbloquean modelos más grandes pero dejan intacto el cuello de
botella. Una 3090 de 24 GB usada cubre ambos ejes por costo cercano al de
una 16 GB nueva.

**Mientras tanto, Nitro es plenamente usable** para tareas agénticas de
contexto/generación moderados (el LocalBackend ya no crashea); solo las
tareas de generación pesada (reescrituras grandes) topan con el reloj.

## Nota de método

La medición salió de una tarea real, no de un benchmark — la doctrina de
la bitácora harness↔modelo. Reveló, de paso, que un "techo de hardware"
aparente era un bug de software del propio harness: exactamente la clase
de hallazgo que el uso productivo expone y el benchmark no. El límite de
hardware que queda (velocidad) quedó aislado y cuantificado.
