# Estudio: qué incorporar de DeepInfer (deepseek-ai/DeepInfer) en braze — 2026-10-20

Fuente: lectura del código (clone `--depth 1`, sin ejecutar nada) hecha por un subagente.
Los submódulos `vendor/FlashMLA` y `vendor/DeepGEMM` venían vacíos, así que los kernels
batch-invariant no se pudieron ver por dentro. Las rutas `archivo:línea` de DeepInfer son
del subagente. Las de braze las verifiqué yo donde se indica.

## Conclusión
DeepInfer es un motor para DeepSeek-V4.1-Flash en GPUs SM100 o Ascend y no es usable en
Nitro. Tres de sus patrones sí sirven para braze; ninguno es el motor en sí:

| # | patrón | dónde va en braze | costo | experimento |
|---|---|---|---|---|
| 1 | **RNG con contador**: u = hash(seed, posición) con SplitMix64 e inversión de la CDF en fp64 (`tilelang_ops/random_sampling.py`). Invariante al batch y reproducible por posición. | LocalBackend (`local/sampling.rs`, `decode.rs`) | S | flujos independientes por ronda; requisito del re-run de round-economics con TEMP>0 |
| 2 | **Sonda de invarianza con trayectoria forzada**: re-correr forzando los tokens de una corrida "golden" y comparar logprobs (`tests/models/test_batch_invariant.py`). | braze-bench | S-M | variar gpu_layers/ubatch/threads en greedy y medir el primer token divergente y los flips de outcome; separa el ruido numérico del de muestreo (el piso del Paper 2 se midió por Ollama, no por el LocalBackend) |
| 3 | **Confianza → presupuesto de verificación (DSpark)**: la confianza por token predice la aceptación y reparte el cómputo de verificación (`schedule_mtp_varlen.py`, `csrc/engine/mtp_varlen.hpp`). | engine (escalación al lead, TTC), alimentado desde el LocalBackend | M | Fase 1 solo observación: confianza por tool call vs éxito de la ronda (AUROC ≥ 0,70 como gate). Fase 2: escalación pre-hoc vs la reactiva actual (McNemar + pass^k), pre-registrada |
| 4 | Structured outputs (xgrammar, trigger lazy por token-ids, rollback) | — | — | ya existe en lo esencial (stencil GBNF; su A/B empató ×3). A lo más, cachear la gramática compilada |
| 5 | Decodificación especulativa de largo variable | LocalBackend | M-L | solo latencia. DeepInfer exige apagarla para el sampling determinista (README) |

No trasladable:
- **SWA bounded replay**: el KV que produce es aproximado y depende de la arquitectura
  YOCO/CED con capas SWA.
- **Async scheduling (ZOS)**: braze decodifica una sola secuencia y las tools ya corren en
  background.
- **DP/CP/EP + EPLB, FlashMLA, DeepGEMM**: son para multi-GPU de datacenter.

## Verificación propia en braze (2026-10-20)
- `local/sampling.rs:212`: con temperatura > 0, la cadena usa `LlamaSampler::dist(s.seed)`.
  **VERIFICADO.**
- `decode.rs`: el sampler se construye por generación (`free_sampler`, al inicio de
  `generate_blocking`) y se reconstruye en los swaps del stencil. **VERIFICADO.**
- Consecuencia: el default es `RANDOM_SEED = 0xFFFFFFFF`, que llama.cpp interpreta como
  semilla aleatoria por generación. Ahí cada ronda recibe un flujo nuevo y no hay problema.
  **Con una semilla fija** (bench con `--seed`, que pasa `seed + rep`, o
  `BRAZE_LOCAL_SEED`), todas las rondas de una sesión reinician el MISMO flujo de
  uniformes. Las réplicas sí difieren entre sí (cada una tiene su semilla), pero las rondas
  dentro de una sesión quedan correlacionadas por construcción. **INFERIDO** (muy
  probable): no se probó en vivo que `dist(seed)` re-siembre su generador al construirse.
- Corrección al informe del subagente: esto **no** explica la v9 L-9 ("réplicas = copias"
  en round-economics). Esas réplicas se corrieron con TEMP = 0 (greedy), donde la semilla
  es irrelevante. El RNG con contador es un arreglo de validez para las corridas con
  TEMP > 0 y semilla fija, no la causa de L-9.

## Recomendación
1. RNG con contador en el LocalBackend (S): semilla derivada de (seed, ronda, posición).
   Va antes del re-run de round-economics con TEMP > 0.
2. Sonda de invarianza en el bench (S-M): sirve de control de ruido para el Paper 3 y
   para round-economics.
3. Calibración de confianza, solo observación (M): instrumentar primero y decidir la
   palanca después, con pre-registro.

## Verificación en vivo del RNG por contador (2026-10-22, commit 18ec409)
En Nitro: `braze run --backend local` con gemma-4-E4B-it QAT (CUDA), `BRAZE_LOCAL_TEMP=0.8`,
config vacía (`XDG_CONFIG_HOME` temporal), mismo prompt. Tres corridas: semilla 42, 42 y 7.
- Las dos corridas con semilla 42 son **idénticas salvo el `session_id`**: 3 rondas, 4.838
  tokens de entrada, 315 de salida, el mismo texto y las mismas tool calls (`write_file`
  denegado sin TTY, y reintentos). La sesión multi-ronda es reproducible bit a bit.
- La corrida con semilla 7 produce otra salida (otros nombres, 409 bytes contra 1.360).
- Lo que no se puede ver desde afuera es que cada ronda use un flujo distinto; eso lo cubren
  los tests unitarios de `keyed`.

Gotchas del build CUDA en Nitro que aparecieron en el camino:
1. Tras un `cargo clean -p llama-cpp-sys-2`, quedaban en `target/release{,/deps,/examples}`
   symlinks `lib*.so` rotos. El `build.rs` de llama-cpp-sys-2 (0.1.152, línea ~1310) chequea
   `dst.exists()`, que da falso para un symlink roto, y su `hard_link` revienta con
   `File exists`. Arreglo: `find target/release -maxdepth 2 -xtype l -name 'lib*.so*' -delete`.
2. El binario `braze` con `local-cuda` no tiene RUNPATH hacia las `.so` de llama.cpp:
   correrlo con `LD_LIBRARY_PATH=target/release/build/llama-cpp-sys-2-*/out/build/bin`.
