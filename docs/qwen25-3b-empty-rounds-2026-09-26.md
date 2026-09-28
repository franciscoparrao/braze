# Rondas vacías de qwen2.5:3b vía Ollama: el parser server-side se traga la tool call (2026-09-26)

## Síntoma

Entre el 21 y el 22-09, en enclave-server con `qwen2.5:3b` vía Ollama
0.32.1 (Nitro), cuatro prompts distintos que pedían una acción concreta
terminaron en `EmptyModelResponse`: rondas con 15-24 tokens generados y
**nada** entregado — sin `content`, sin `tool_calls`, sin `thinking`. El
harness reintenta con la nota `[harness] empty_round` (hasta el tope) y el
turno muere. Prompts: "…Responde solo el número, sin usar herramientas"
(con contexto RAG inyectado, 5 veces), "Ejecuta con shell_exec exactamente:
curl …" y "…rm -rf /tmp/x".

## Reproducción (Ollama 0.20.4 local, sin Nitro, sin contaminar el sweep)

Proxy de captura (`ollama_tap.py`) entre `braze run` y Ollama para tener el
request EXACTO de braze (system prompt de 1130 chars, 6 tools con schema,
`num_ctx` 8192, `num_predict` 4096, temp 0,2) y la respuesta NDJSON cruda.

| experimento | resultado |
|---|---|
| `braze run` "curl…" vía proxy | ronda 1: **1 chunk, `eval_count=23`, content vacío, sin tool_calls** — la ronda vacía, reproducida; ronda 2 (tras la nota del harness): tool call válida |
| request 03 capturado, `/api/chat` `stream=false`, seeds 1-10 | **10/10 vacías**, siempre 23 tokens, `done_reason=stop` |
| mismo system/user/tools, `/api/generate` **`raw=true`** con ChatML renderizado a mano, seeds 1-6 | **6/6 tool call bien formada** (`<tool_call>{"name":"shell_exec","arguments":{"command":["curl","http://example.com"]}}</tool_call>`), 27 tokens |
| ídem con render compacto estilo Go (1244 tokens de prompt vs 1211 de Ollama) | 3/3 válidas, 27 tokens |
| prompt "caja fuerte" con contexto RAG, `/api/chat` | `4471` (no reprodujo aquí; en Nitro falló 5 veces) |

Lectura: el modelo SÍ produce una tool call cuando se ve su salida cruda.
Por `/api/chat`, con el prompt que Ollama renderiza (1211 tokens, 33 menos
que mi mejor aproximación), el modelo emite 23 tokens que **el parser de
tool calls de Ollama descarta sin entregar ni el texto ni la llamada**.
Determinista (10/10) y presente ya en 0.20.4: no es la versión de Ollama
de Nitro. La diferencia de 4 tokens (27 vs 23) encaja con un `command`
emitido como string en vez de array, o con los tags omitidos — la forma
exacta requiere el prompt renderizado por Ollama (`OLLAMA_DEBUG=1`), que
no se alcanzó a capturar: la instancia de debug murió por timeout con la
máquina de trabajo a carga 70-90 por procesos ajenos.

## Por qué importa

Es la clase #1/#17 del proyecto (parser server-side de Ollama) en una
variante nueva: no revienta con 500, **se traga la salida en silencio**.
Ninguna palanca del harness puede actuar sobre tokens que nunca ve: la
escalera de rescate (que sabe leer `<tool_call>` y JSON desnudo) no llega
a ejecutarse. Para un SLM tool-tuned es la peor combinación: emite el
formato entrenado y el servidor lo pierde. Tercera demostración en vivo
del argumento estratégico del LocalBackend (el harness dueño de los
tokens): con `raw` el mismo modelo acierta 9/9.

## Palanca propuesta (pre-registrar antes de medir)

**Modo `raw` para el OllamaBackend**: braze renderiza el ChatML (ya tiene
la plantilla qwen/ChatML del LocalBackend) y manda `/api/generate` con
`raw: true`; el texto vuelve íntegro y la escalera de rescate hace el
parsing, como en el LocalBackend. Ollama queda como servidor de tokens
tonto. Alternativa mínima: cuando una ronda devuelve `eval_count > 0` y
nada, reintentar ESA ronda en `raw` para recuperar el texto. Medir: A/B
`default.toml` + `discriminating.toml` con qwen2.5:3b, `chat` vs `raw`
(hipótesis: `raw` elimina la clase `EmptyModelResponse` y sube el pass
rate del léxico chico; riesgo: perder el parsing de tools de Ollama para
familias sin plantilla propia — gpt-oss vía Ollama).

## Pendiente concreto

1. Capturar el prompt renderizado por Ollama con `OLLAMA_DEBUG=1` (Nitro
   tras el sweep, o local con la máquina descargada) y guardar los 23
   tokens exactos en este doc.
2. Confirmar 10/10 en Nitro (0.32.1) con el request 03 capturado.
3. Pre-registro del A/B `raw`.

Artefactos: `scratchpad/tap-local/{03-req.json,03-resp.ndjson}` (request y
ronda vacía capturados), `qwen_probe.py`, `ollama_tap.py`.
