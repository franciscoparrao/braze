# Paridad LocalBackend vs OllamaBackend (Fase 1) — 2026-07-21

Suite `default.toml` (19 tareas), **qwen2.5:3b** por ambos backends,
mismo hardware (máquina local, CPU), `reps=1`. Direccional, no riguroso:
`reps=1` + máquina cargada (contaminación de latencia, no de pass rate).
Datos crudos: `sweep-localbackend-parity-2026-07-21.json`.

## Resultado

| backend | pass rate | schema_fail | rescues |
|---|---|---|---|
| ollama:qwen2.5:3b | 14/19 (74%) | 2 | 0 |
| local:qwen2.5:3b | 10/19 (53%) | 17 | 26 |

McNemar pareado: solo-control=5, solo-brazo=1, **p=0.22** (no significativo, n=19).

## Por skill (lo que importa)

| skill | ollama | local |
|---|---|---|
| single_tool | 6/7 | **6/7** (paridad exacta) |
| no_tool | 3/3 | **3/3** (paridad exacta) |
| multi_step | 2/3 | 0/3 |
| distractor_selection | 3/3 | 1/3 |
| error_recovery | 0/3 | 0/3 (muro de capacidad de qwen2.5:3b) |

## Lectura

- **Paridad exacta en lo fundamental** (single_tool, no_tool): el
  tool-calling y la abstención funcionan idéntico al backend nativo.
- El **gap** está en las skills multi-ronda (multi_step,
  distractor_selection), donde el `schema_fail=17` (argumentos de tool
  call mal formados) se compone entre rondas. Es el "format tax": el
  preámbulo de tools del LocalBackend da nombre+summary pero NO el schema
  de argumentos (los stubs son diferidos), así que qwen2.5:3b produce
  argumentos peores que en el modo nativo de Ollama.
- **error_recovery** es muro de capacidad del modelo (ambos 0/3), no
  diferencia de backend.

## Fix del gap (valida el plan por fases)

El gap es exactamente lo que la Fase 3 (GBNF/constrained decoding)
ataca: forzar argumentos válidos a nivel de token haría `schema_fail=0`.
Alternativa más barata para Fase 1: incluir el schema de argumentos en el
preámbulo (requiere resolver los schemas diferidos en el backend).

## Bug cazado por la paridad

`LlamaBackend::init()` es singleton global de proceso; braze-bench crea
un backend por tarea → `BackendAlreadyInitialized` en la 2da. Arreglado
con `shared_llama_backend()` (Mutex estático). Invisible en `braze run`.
