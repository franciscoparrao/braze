# Perfil de tiempo: Bonsai-2-9B y Qwen3.5-9B en discriminating.toml (2026-10-21)

Fuente: logs de `llama-server` (fork PrismML, `print_timing` por request y `f_sim_best` LCP)
de los sweeps del 2026-10-20 (`discriminating.toml` × 3, seed 42, ctx 32768), cruzados con
`docs/sweep-{bonsai2-9B,qwen35-9b-q4km-llamacpp}-discriminating-2026-10-20.json`.
El request de smoke inicial queda excluido.

| | Bonsai-2-9B (GPU completa) | Qwen3.5-9B Q4_K_M (`-ngl 24`) |
|---|---|---|
| corridas / rondas = requests | 102 / 626 | 102 / 520 |
| tiempo de pared total | 1,4 h | 3,2 h |
| **generación** | **1,07 h (76 %)** | **2,82 h (87 %)** |
| prefill | 0,32 h (23 %) | 0,39 h (12 %) |
| harness + tools + espera | 0,02 h (2 %) | 0,02 h (<1 %) |
| tokens de prefill procesados / input_tokens del bench | 1,06 M / 2,50 M → **reuso de caché ≈ 58 %** | 0,70 M / 2,04 M → **≈ 66 %** |
| similitud LCP entre rondas (mediana) | 0,77 | 0,76 |
| tokens generados (server = bench) | 169.449 | 119.878 |
| velocidad de generación | 44,1 tok/s | **11,8 tok/s** |
| velocidad de prefill | 929 tok/s | 495 tok/s |
| tokens por request: mediana / p90 / máx | 138 / 526 / 4096 | 138 / 437 / 3071 |
| requests ≥ 1000 tok | 4,2 % → **32 % del tiempo de generación** | 2,3 % → **22 %** |
| requests en el tope (≥ 4000 tok) | 3 | 0 |

## Lectura
1. **La generación domina** (76-87 %). El prefill pesa 12-23 %, y el harness con las tools,
   1-2 %: optimizar el loop de braze no mueve el tiempo de pared.
2. **Qwen es lento por velocidad, no por volumen.** Genera menos tokens que Bonsai (120 mil
   contra 169 mil), pero a 11,8 contra 44 tok/s, porque solo 24 capas van en la GPU. La
   hipótesis previa ("el razonamiento domina los 114 s/tarea") no se sostiene en el volumen.
   La fracción de razonamiento dentro de lo generado sigue sin medirse: el bench cuenta todo
   como `output_tokens`.
3. **El caché del prompt ya funciona.** `llama-server` reusa por LCP entre el 58 y el 66 % de
   los tokens del prompt. Lo que queda (similitud ≈ 0,76) es contenido que cambia entre
   rondas; una compactación que preserve el prefijo podría recortar parte de un 12-23 % del
   tiempo.
4. **La cola larga es cara.** Entre un 2 y un 4 % de los requests (≥ 1000 tokens) se lleva
   entre un 22 y un 32 % del tiempo de generación, y 3 requests de Bonsai llegaron al tope de
   4096. No está clasificado cuánto de eso es escritura legítima de archivos y cuánto es
   degeneración.

## Palancas, por rendimiento esperado
1. Velocidad de generación en Qwen: una cuantización que quepa entera en la VRAM (IQ4_XS o
   Q3_K_M) o decodificación especulativa por prompt lookup. Hay que medir el costo en
   capacidad.
2. Cola larga: clasificar los requests ≥ 1000 tokens (archivo vs degeneración) antes de
   tocar `max_tokens` o agregar un corte por repetición.
3. Medir la fracción de razonamiento (contar `reasoning_content` aparte en el backend zen)
   antes de diseñar un presupuesto de razonamiento.
4. Prefill: compactación con prefijo estable (ganancia acotada a una parte del 12-23 %).
