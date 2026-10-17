# Bonsai 2 (ternario), su base Qwen3.5 y LFM2.5-24B-A2B en default.toml — 2026-10-17

Todo en Nitro: `default.toml` × 3 repeticiones, seed 7, timeout 300 s, temperatura 0,2,
braze `5084d4a`. Los modelos GGUF se sirven con `llama-server` del fork PrismML
(`0082ac24a`, CUDA, `-c 16384 --jinja`) por el backend `zen`, y los de Ollama con 0.32.1.
Cada brazo corrió solo en el nodo.

| brazo | stack | aprobadas (funcional) | estricta | s/tarea |
|---|---|---|---|---|
| qwen3.5:9b (Q4_K_M, blob de Ollama) | Ollama | 48/56 | 48 | 21,7 |
| Qwen3.5-9B Q4_K_M (unsloth), `-ngl 26` | llama-server | 57/57 | 54 | 14,7 |
| Ternary-Bonsai-2-9B PQ2_0, `-ngl 99` | llama-server (fork) | 57/57 | 57 | 4,7 |
| qwen3.5:4b | Ollama | 54/57 | 54 | 13,0 |
| Ternary-Bonsai-2-4B PQ2_0 | llama-server (fork) | 54/57 | 54 | 3,1 |
| LFM2.5-24B-A2B QAD-Q4_0, `-ngl 99 --n-cpu-moe 30` | llama-server (fork) | 57/57 | 57 | 19,7 |

Pareos (McNemar exacto sobre (tarea, rep)):

- **Serving stack, mismo modelo y cuantización:** Ollama 48 vs llama-server 56 (n = 56),
  discordantes 0 / 8, p = 0,008. Los 8 fallos de Ollama son errores de su parser de tool
  calls del lado del servidor: 5 `XML syntax error … element <function> closed by
  </parameter>` y 3 del circuit breaker abierto después. Todos caen en
  `distractor_selection_*`. El re-muestreo de `5084d4a` no los cubrió.
- **Ternarización, mismo stack:** Qwen3.5-9B Q4_K_M 57 vs Bonsai-2-9B 57, sin
  discordantes. En la 4B, 54 contra 54 (discordantes 2 / 2, Ollama contra fork).
- default.toml satura en los tres modelos servidos por llama-server: no discrimina entre
  ellos. Hace falta `discriminating.toml` para comparar capacidad.

Caveat: el blob de Ollama y el GGUF de unsloth son ambos Q4_K_M, pero no
necesariamente idénticos byte a byte (cuantizadores distintos). Los fallos son de
parseo, no de contenido, así que la atribución al stack es casi segura; la parte de
capacidad, si la hubiera, no se separa con este diseño.

LFM2.5-24B-A2B emite sus tool calls en formato Python entre `<|tool_call_start|>` y
`<|tool_call_end|>`, y `llama-server --jinja` las convierte a tool calls estructuradas
(smoke verificado), así que la escalera de rescate de braze no intervino.
