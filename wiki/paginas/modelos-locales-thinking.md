---
type: wiki-page
created: 2026-07-14
tags: [modelos-locales, ollama, gotcha]
---

# Modelos locales: thinking vs no-thinking

## Qué es

Los modelos servidos por Ollama en Nitro se dividen en dos categorías
según si devuelven un campo `message.thinking` separado de
`message.content` en `/api/chat` (confirmado empíricamente, no
documentación de Ollama):

| Con `thinking` | Sin `thinking` |
|---|---|
| `gpt-oss:20b` | `gemma4:e4b` |
| `qwen3.5-coder` | `qwen2.5:3b` |
| `qwen3.5:9b` | `llama3.2:1b` |
| `gemma4:12b` | `ministral-3:3b` |

## Por qué existe

Surgió investigando dos crashes duros de `gpt-oss:20b` en una sesión
de uso abierto (`braze-playground/log-insights`, ver
`docs/usability-log-gptoss20b-playground-2026-07-13.md`, hallazgos
U-21/U-22): `HTTP 500: error parsing tool call: raw='We need to...'`
— el razonamiento crudo del modelo apareciendo donde se esperaba un
tool call. La hipótesis: en sesiones largas, con contexto cerca del
límite (`ollama_num_ctx` default 8192), la generación se corta a mitad
de la transición entre el bloque de thinking y el formato "harmony" de
tool-calling nativo que Ollama parsea server-side — produciendo un tool
call roto que Ollama rechaza con 500 antes de que `braze` vea nada.

## Detalles

### Comparación directa (mismo sweep, seed 42, n=95 c/u)

`docs/sweep-gemma4e4b-vs-gptoss20b-2026-07-13.json`:

| Modelo | Pass rate | Wilson 95% CI |
|---|---|---|
| `gemma4:e4b` (no-thinking) | 96.8% (92/95) | [91.1, 98.9] |
| `gpt-oss:20b` (thinking) | 100.0% (95/95) | [96.1, 100.0] |

Delta +3.2pp, Newcombe 95% CI [−1.2, +8.9] — cruza cero, no
distinguible del ruido. En la suite scripteada corta (timeout 180s por
tarea) ninguno de los dos crasheó — el modo de falla de U-21/U-22
necesita la presión de contexto de una sesión larga que la suite no
genera.

### Asimetría en uso abierto real

`gemma4:e4b` acumuló cientos de corridas entre las Fases 1-3 de
[[hallazgo-composicion-basta]] sin un solo crash de esta clase.
`gpt-oss:20b` tuvo 2 crashes duros + 1 agotamiento de las 20 rondas sin
converger en una sola sesión de playground. No es concluyente (falta
someter a `gemma4:e4b` a una sesión de playground igual de larga y
abierta), pero es la primera evidencia real, no solo la hipótesis, de
que la vía no-thinking no cuesta capacidad medible en la suite
scripteada.

### Palancas existentes para mitigar el crash de `gpt-oss:20b` (no implementadas)

1. **Subir `ollama_num_ctx`** (default 8192) — ya configurable vía
   `BRAZE_OLLAMA_NUM_CTX`, cero código nuevo.
2. **Modo `prompt_tools`** (`OllamaBackend::with_prompt_tools`) — evita
   el parseo "harmony" server-side de Ollama. Ya implementado (brazo B
   del A/B de constrained-decoding) pero solo expuesto vía
   `+ablate:prompt-tools` en `braze-bench`, no en `braze chat`
   interactivo — wiring nuevo chico.
3. **Reabrir la política de "Ollama nunca reintenta"**
   (`crates/braze-model/src/retry.rs`, decisión H-19/v5) — esa decisión
   asume que un 500 de Ollama es agotamiento de recursos; el 500 de
   U-21 es un fallo de parseo de contenido, categoría distinta donde un
   reintento podría simplemente funcionar.

### Nota de referencia: Bonsai 27B (PrismML) — sin probar, tool-calling es lo que más degrada bajo compresión

Anuncio 2026-07-14: **Bonsai 27B** (PrismML, investigadores de Caltech,
respaldo Khosla/Google/Samsung) — compresión ternaria/1-bit de
`Qwen3.6 27B`, no MoE. Es "thinking" (modo de razonamiento explícito),
así que entraría en la misma categoría de riesgo de arriba si alguna
vez se prueba con `braze`.

Dato relevante de su propio benchmark de 15 pruebas — **tool-calling es
la capacidad que más se degrada bajo compresión extrema**, más que
matemáticas o código:

| Capacidad | Ternary (5.9GB) | 1-bit (3.9GB) | Baseline full-precision |
|---|---|---|---|
| Matemáticas | 93.4 (98% retenido) | 91.7 (96%) | 95.3 |
| Código | 86.0 (97%) | 81.9 (92%) | 88.7 |
| **Tool-calling** | **74.0 (92.5%)** | **66.0 (82.5%)** | **80.0** |

Evidencia independiente (compresión de pesos, no parámetro-count nativo
chico — un eje de "smallness" que este proyecto no toca) del mismo
síntoma que motiva la tesis del paper: el tool-calling es la capacidad
más frágil cuando el presupuesto es chico, exactamente donde un harness
tiene más margen para compensar.

**No disponible vía Ollama** — solo MLX (Apple) y CUDA, "custom low-bit
kernels" (no GGUF, no compatible con candle/mistral.rs sin trabajo
serio). Sí disponible como API cloud vía Together.ai — ver
`crates/braze-model/src/openrouter.rs` (`with_base_url`, ya genérico
para cualquier endpoint compatible con Chat Completions) y
`BRAZE_OPENROUTER_BASE_URL`/`BRAZE_OPENROUTER_API_KEY` — probarlo así
no requiere backend nuevo, solo apuntar `braze` a
`https://api.together.xyz/v1` con el modelo correspondiente.

## Relacionado

- [[hallazgo-composicion-basta]] — `gemma4:e4b` es también el modelo
  central de ese hallazgo

## Referencias

- `docs/usability-log-gptoss20b-playground-2026-07-13.md` (hallazgos
  U-21, U-22, la comparación completa)
- `docs/sweep-capacity-hardware-2026-07-13.md` (por qué `gpt-oss:20b`
  es el modelo local recomendado del proyecto)
- `docs/sweep-gemma4e4b-vs-gptoss20b-2026-07-13.json`
- `crates/braze-model/src/ollama.rs` (`with_prompt_tools`)
- `crates/braze-model/src/retry.rs` (política de retry de Ollama)
