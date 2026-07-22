# Ranking SML por LocalBackend: gemma-4-12B vs gpt-oss:20b — 2026-07-21

**Pregunta**: ¿gpt-oss:20b es superior al gemma-4-12B recién instalado?
Suite `default.toml` (19×3=57 por brazo), seed 42, Nitro. Configs por
viabilidad (no idénticas — divulgado): gpt-oss CPU puro; 12B con 14/48
capas GPU (VRAM compartida con otras cargas) y timeout 360s (los 180s
default censuraban la mitad de sus corridas; el primer intento con 12
capas/180s quedó como procedencia: 29/57 timeouts).

## Resultado

| | gpt-oss:20b (CPU) | gemma-4-12B (GPU 14/48, 360s) |
|---|---|---|
| pass rate | **57/57 (100%)** [94,100] | 30/57 (53%) [40,65] |
| pass^3 | **100%** | 52.6% |
| timeouts | 0 | **26/57** |
| pass condicional (sin timeout) | 100% | **30/31 (97%)** |
| schema_fail / rescues | 0 / 0 (por diseño Harmony) | 0 / 71 |
| avg por tarea | 41s | 175s |

McNemar pareado: solo-12B=0, solo-oss=27, **p=1.5e-08** — decisivo.

## Lectura

1. **Operacionalmente en Nitro, gpt-oss:20b es superior sin discusión**
   — pero por throughput, no por inteligencia: MoE de 3.6B activos vs
   denso de 12B es ~4-8× de velocidad, y el workload agéntico es
   multi-ronda y latency-bound. Duplicar el timeout apenas movió la
   censura (29→26 timeouts): el 12B denso no cabe en el presupuesto de
   tiempo de este hardware con offload parcial.
2. **En capacidad pura están casi empatados**: 97% condicional del 12B
   (un solo fallo real en 31 corridas completadas: una aserción de
   edit_file) vs 100% — indistinguibles a este n. El 12B "sabe" hacer
   las tareas; no alcanza a hacerlas.
3. **La lección generaliza la tesis SML del proyecto**: en hardware
   modesto, los parámetros ACTIVOS dominan la utilidad agéntica. El
   campeón local sigue siendo gpt-oss:20b — ahora con su mejor forma de
   correr: el LocalBackend Harmony (57/57, pass^3=100%, el mejor número
   del proyecto; mismo modelo vía Ollama: 98.9% n=95).
4. El brazo gpt-oss fue además la primera suite completa del camino
   Harmony — verificación a escala de la Fase 2b con puntaje perfecto.

Datos: `sweep-rank-oss.json`, `sweep-rank-12b-v3.json` (v1 censurado y
v2 abortado por OOM quedan en Nitro como procedencia).
