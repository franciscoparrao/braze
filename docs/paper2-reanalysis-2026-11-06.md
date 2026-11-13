# Reanálisis del Paper 2 (2026-11-06)

Generado por `paper2/analysis/reanalysis.py` desde los JSON de `docs/`. No editar a mano.

## Estudio 1 (gpt-oss:20b, exploratorio)

El oráculo del Estudio 1 NO corrió `cargo check` (el campo no existe en estos JSON); la corrección funcional aquí es archivo esperado + sin timeout.

### Éxito: oficial frente a corrección funcional

| Tarea | Pass oficial (none→pb) | p Fisher | Holm | Corrección funcional (none→pb) | p Fisher | Holm | Fallas por tope de tokens (none→pb) | Fallas por tope de rondas (none→pb) | Timeouts (none→pb) |
|---|---|---|---|---|---|---|---|---|---|
| original | 14/20→16/20 | 0.716 | 1.000 | 16/20→18/20 | 0.661 | 1.000 | 3→2 | 1→0 | 0→0 |
| loop | 15/20→14/20 | 1.000 | 1.000 | 15/20→14/20 | 1.000 | 1.000 | 0→0 | 0→0 | 0→0 |
| move | 18/20→15/20 | 0.407 | 1.000 | 18/20→17/20 | 1.000 | 1.000 | 0→2 | 0→0 | 0→0 |

### Economía por tarea (rondas, tokens, break-even)

| Tarea | ΔR (IC 95 %) | p Holm ΔR | T_base | T_mem | ΔT | memory_tokens | ΔT residual | R_mem | ΔR* = ΔT·R_mem/T_base | Net Δtokens (IC 95 %) | p Holm net |
|---|---|---|---|---|---|---|---|---|---|---|---|
| original | +0.95 (+0.25, +1.65) | 0.027 | 1406 | 1590 | +184 | 299 | -115 | 5.75 | 0.75 | -304 (-1521, +912) | 0.614 |
| loop | +0.15 (-0.40, +0.70) | 0.583 | 1383 | 1625 | +243 | 299 | -56 | 5.45 | 0.96 | +1076 (+11, +2141) | 0.096 |
| move | +0.35 (+0.02, +0.68) | 0.072 | 1394 | 1662 | +268 | 299 | -31 | 6.05 | 1.16 | +1132 (+560, +1704) | 0.001 |

Nota: ΔR > 0 = el playbook ahorra rondas. `memory_tokens` es la estimación del bench (caracteres / 4, `crates/braze-bench/src/memory.rs::estimate_tokens`), no un conteo del tokenizador del modelo; por eso «ΔT residual» NO separa inyección de trayectoria y no debe reportarse.

### Dificultad sin ayuda (brazos `none` del Estudio 1, gpt-oss:20b)

| Tarea | Pass | Rondas medias | Timeouts |
|---|---|---|---|
| original | 14/20 | 6.70 | 0 |
| loop | 15/20 | 5.60 | 0 |
| move | 18/20 | 6.40 | 0 |

### Llamadas a herramientas en las tareas frescas (Estudio 1)

Definición: llamadas antes del primer `edit_file` (sin contar el edit); corridas sin `edit_file` excluidas de esa columna.

| Tarea | Corridas con edit (none→pb) | p Fisher | Llamadas antes del primer edit (none→pb) | p Mann-Whitney | Llamadas totales (none→pb) | p Mann-Whitney |
|---|---|---|---|---|---|---|
| loop | 16/20→19/20 | 0.34 | 2.75→2.47 | 0.481 | 4.60→4.45 | 0.795 |
| move | 20/20→20/20 | 1.00 | 3.40→3.05 | 0.023 | 5.40→5.05 | 0.029 |

## Réplica (ornith:9b, pre-registrada)

### Éxito: oficial frente a corrección funcional (con cargo check)

| Tarea | Pass oficial (none→pb) | p Fisher | Holm | Corrección funcional (none→pb) | p Fisher | Holm | Fallas por tope de tokens (none→pb) | Fallas por tope de rondas (none→pb) | Timeouts (none→pb) |
|---|---|---|---|---|---|---|---|---|---|
| original | 16/20→11/20 | 0.176 | 0.176 | 17/20→15/20 | 0.695 | 1.000 | 1→7 | 0→0 | 0→1 |
| loop | 12/20→5/20 | 0.054 | 0.107 | 12/20→13/20 | 1.000 | 1.000 | 0→12 | 0→0 | 0→0 |
| move | 10/20→2/20 | 0.014 | 0.041 | 11/20→3/20 | 0.019 | 0.056 | 4→13 | 1→4 | 2→5 |

### Economía por tarea

| Tarea | ΔR (IC 95 %) | p Holm ΔR | T_base | T_mem | ΔT | memory_tokens | ΔT residual | R_mem | ΔR* = ΔT·R_mem/T_base | Net Δtokens (IC 95 %) | p Holm net |
|---|---|---|---|---|---|---|---|---|---|---|---|
| original | -0.50 (-1.17, +0.17) | 0.140 | 1756 | 2154 | +398 | 299 | +99 | 4.45 | 1.01 | +2709 (+999, +4419) | 0.006 |
| loop | -1.55 (-2.16, -0.94) | 0.000 | 1750 | 2332 | +582 | 299 | +283 | 5.05 | 1.68 | +5794 (+4011, +7577) | 0.000 |
| move | -1.75 (-3.26, -0.24) | 0.049 | 1826 | 2353 | +527 | 299 | +228 | 6.15 | 1.77 | +6635 (+2031, +11239) | 0.006 |

Nota: ΔR > 0 = el playbook ahorra rondas. `memory_tokens` es la estimación del bench (caracteres / 4, `crates/braze-bench/src/memory.rs::estimate_tokens`), no un conteo del tokenizador del modelo; por eso «ΔT residual» NO separa inyección de trayectoria y no debe reportarse.

## Estudio 2 (pre-registrado)

### Estudio 2: timeouts y rendimiento por brazo

| Brazo | Pass | Timeouts (900 s) | Pass entre no-timeouts | Mediana wall (s) | Mediana s/ronda | Media tokens de entrada |
|---|---|---|---|---|---|---|
| baseline | 59/102 | 38 | 59/64 | 589 | 100 | 13526 |
| empty | 68/102 | 23 | 68/79 | 384 | 58 | 18709 |
| seeded | 72/102 | 21 | 72/81 | 337 | 61 | 17559 |

### Estudio 2: McNemar con y sin pares que involucran un timeout

| Contraste | Discordantes (a gana / b gana) | p | Con timeout | Sin timeout: discordantes | p sin timeout |
|---|---|---|---|---|---|
| seeded − baseline | 18 / 5 | 0.011 | 19 | 2 / 2 | 1.000 |
| empty − baseline | 15 / 6 | 0.078 | 18 | 0 / 3 | 0.250 |
| seeded − empty | 14 / 10 | 0.541 | 18 | 5 / 1 | 0.219 |

### Barrido del piso de ruido (2026-09-12)

- Corridas: 95, aprobadas: 95, timeouts: 0.
- Pares dentro de tarea: 190; con distinto pass/fail: 0; con distinta huella de resultado: 56.
- Con 100 % de aprobación, un cambio de pass/fail es imposible por construcción: el barrido no puede medir la contribución del stack.

## Inventario

### Inventario de barridos del Estudio 1 (gpt-oss:20b)

| Archivo | Corridas | Commit | Huella de suite |
|---|---|---|---|
| `sweep-memory-distillation-gptoss20b-r10-2026-07-16.json` | 30 | 3ec35dc | 3da626b608ccdab9 |
| `sweep-memory-distillation-gptoss20b-r20-2026-07-16.json` | 60 | 3ec35dc | 3da626b608ccdab9 |
| `sweep-memory-distillation-gptoss20b-r5-2026-07-16.json` | 15 | ec61f5e | 3da626b608ccdab9 |
| `sweep-memory-distillation-gptoss20b-r5-nofriction-2026-07-16.json` | 15 | ec61f5e | 3da626b608ccdab9 |
| `sweep-memory-distillation-gptoss20b-r5-nofriction-v2-2026-07-16.json` | 15 | ec61f5e | 3da626b608ccdab9 |
| `sweep-memory-distillation-loopB-diagnostic-2026-07-16.json` | 10 | 3ec35dc | 183d43dfc755b801 |
| `sweep-memory-distillation-loopB-diagnostic-v2-2026-07-16.json` | 5 | 3ec35dc | 63214763c725c701 |
| `sweep-memory-distillation-loopB-mechanism-diagnostic-2026-07-17.json` | 25 | 3ec35dc | 85cb1e8bc9e58981 |
| `sweep-memory-distillation-loopB-smoke-2026-07-16.json` | 15 | 3ec35dc | 6755e21426861717 |
| `sweep-memory-distillation-loopB-smoke-v2-2026-07-16.json` | 15 | 3ec35dc | 183d43dfc755b801 |
| `sweep-memory-distillation-loopB-smoke-v3-2026-07-16.json` | 15 | 3ec35dc | 63214763c725c701 |
| `sweep-memory-distillation-loopB-smoke-v4-2026-07-16.json` | 15 | 3ec35dc | 85cb1e8bc9e58981 |
| `sweep-memory-distillation-moveB-smoke-2026-07-17.json` | 21 | 3ec35dc | a5fccc8f6a894b57 |
| `sweep-memory-distillation-r20-2026-07-16.json` | 100 | 3ec35dc | 85cb1e8bc9e58981 |
| `sweep-memory-distillation-r20-moveB-2026-07-17.json` | 140 | None | None |
| `sweep-memory-distillation-r20-moveB-batch0-2026-07-17.json` | 35 | 3ec35dc | a5fccc8f6a894b57 |
| `sweep-memory-distillation-r20-moveB-batch1-2026-07-17.json` | 35 | 3ec35dc | a5fccc8f6a894b57 |
| `sweep-memory-distillation-r20-moveB-batch2-2026-07-17.json` | 35 | 3ec35dc | a5fccc8f6a894b57 |
| `sweep-memory-distillation-r20-moveB-batch3-2026-07-17.json` | 35 | 3ec35dc | a5fccc8f6a894b57 |
| `sweep-memory-distillation-smoke-2026-07-16.json` | 3 | ec61f5e | 3da626b608ccdab9 |
| `sweep-memory-distillation-smoke-gptoss20b-2026-07-16-b.json` | 3 | ec61f5e | 3da626b608ccdab9 |
| `sweep-memory-distillation-smoke-gptoss20b-2026-07-16.json` | 3 | ec61f5e | 3da626b608ccdab9 |
