# Reanálisis del Paper 2 (2026-11-06; ampliado 2026-11-14)

Generado por `paper2/analysis/reanalysis.py` desde los JSON de `docs/`. No editar a mano.

## Estudio 1 (gpt-oss:20b, exploratorio)

El oráculo del Estudio 1 NO corrió `cargo check` (el campo no existe en estos JSON); la corrección funcional aquí es archivo esperado + sin timeout.

### Éxito: oficial frente a corrección funcional

| Tarea | Pass oficial (none→pb) | Δ Newcombe 95 % | p Fisher | Holm | Corrección funcional (none→pb) | Δ Newcombe 95 % | p Fisher | Holm | Fallas por tope de tokens (none→pb) | Fallas por tope de rondas (none→pb) | Timeouts (none→pb) | Errores de backend (none→pb) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| original | 14/20→16/20 | +0.10 [-0.17, +0.35] | 0.716 | 1.000 | 16/20→18/20 | +0.10 [-0.13, +0.33] | 0.661 | 1.000 | 3→2 | 1→0 | 0→0 | 0→1 |
| loop | 15/20→14/20 | -0.05 [-0.31, +0.22] | 1.000 | 1.000 | 15/20→14/20 | -0.05 [-0.31, +0.22] | 1.000 | 1.000 | 0→0 | 0→0 | 0→0 | 0→0 |
| move | 18/20→15/20 | -0.15 [-0.38, +0.09] | 0.407 | 1.000 | 18/20→17/20 | -0.05 [-0.27, +0.17] | 1.000 | 1.000 | 0→2 | 0→0 | 0→0 | 0→0 |

### Economía por tarea (rondas, tokens, break-even)

| Tarea | ΔR (IC 95 %) | p Holm ΔR | T_base | T_mem | ΔT (IC 95 % bootstrap) | memory_tokens | R_mem | ΔR* = ΔT·R_mem/T_base | Net Δtokens (IC 95 %) | p Holm net | Identidad ΔT·R_mem − ΔR·T_base | Wall s (none→pb) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| original | +0.95 (+0.25, +1.65) | 0.027 | 1414 | 1595 | +181 (+142, +222) | 299 | 5.75 | 0.73 | -304 (-1521, +912) | 0.614 | -304 | 57.4→40.6 |
| loop | +0.15 (-0.40, +0.70) | 0.583 | 1396 | 1632 | +236 (+186, +291) | 299 | 5.45 | 0.92 | +1076 (+11, +2141) | 0.096 | +1076 | 49.5→51.2 |
| move | +0.35 (+0.02, +0.68) | 0.072 | 1395 | 1663 | +268 (+232, +303) | 299 | 6.05 | 1.16 | +1132 (+560, +1704) | 0.001 | +1132 | 48.5→47.0 |

Notas: ΔR > 0 = el playbook ahorra rondas. T = razón de medias (total de tokens de entrada / total de rondas del brazo); con ese estimador la columna «Identidad» reproduce el Δ neto exacto (Ec. 1). La versión del 2026-11-06 usaba la media de las razones por corrida, con la que la identidad no cierra (error de 2 a 200 tokens). `memory_tokens` es la estimación del bench (caracteres / 4, `crates/braze-bench/src/memory.rs::estimate_tokens`), no un conteo del tokenizador: ΔT no separa inyección de cambio de trayectoria. IC de ΔT: bootstrap percentil, 10.000 réplicas, semilla 0, remuestreo de corridas dentro de cada brazo.

### Dificultad sin ayuda (brazos `none` del Estudio 1, gpt-oss:20b)

| Tarea | Pass | Rondas medias | Timeouts |
|---|---|---|---|
| original | 14/20 | 6.70 | 0 |
| loop | 15/20 | 5.60 | 0 |
| move | 18/20 | 6.40 | 0 |

### Llamadas a herramientas en las tareas frescas (Estudio 1)

Definición: llamadas antes del primer `edit_file` (sin contar el edit); corridas sin `edit_file` excluidas de esa columna.

| Tarea | Corridas con edit (none→pb) | p Fisher | Llamadas antes del primer edit (none→pb) | p Mann-Whitney | Holm | Llamadas totales (none→pb) | p Mann-Whitney | Holm |
|---|---|---|---|---|---|---|---|---|
| loop | 16/20→19/20 | 0.34 | 2.75→2.47 | 0.481 | 0.481 | 4.60→4.45 | 0.795 | 0.795 |
| move | 20/20→20/20 | 1.00 | 3.40→3.05 | 0.023 | 0.045 | 5.40→5.05 | 0.029 | 0.059 |

### Holdout (gpt-oss:20b)

- Con playbook, sin brazo pareado: 18/20 aprobadas, 3.85 rondas medias.

## Réplica (ornith:9b, pre-registrada)

### Éxito: oficial frente a corrección funcional (con cargo check)

| Tarea | Pass oficial (none→pb) | Δ Newcombe 95 % | p Fisher | Holm | Corrección funcional (none→pb) | Δ Newcombe 95 % | p Fisher | Holm | Fallas por tope de tokens (none→pb) | Fallas por tope de rondas (none→pb) | Timeouts (none→pb) | Errores de backend (none→pb) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| original | 16/20→11/20 | -0.25 [-0.49, +0.04] | 0.176 | 0.176 | 17/20→15/20 | -0.10 [-0.34, +0.15] | 0.695 | 1.000 | 1→7 | 0→0 | 0→1 | 0→1 |
| loop | 12/20→5/20 | -0.35 [-0.58, -0.04] | 0.054 | 0.107 | 12/20→13/20 | +0.05 [-0.23, +0.32] | 1.000 | 1.000 | 0→12 | 0→0 | 0→0 | 1→0 |
| move | 10/20→2/20 | -0.40 [-0.61, -0.12] | 0.014 | 0.041 | 11/20→3/20 | -0.40 [-0.62, -0.10] | 0.019 | 0.056 | 4→13 | 1→4 | 2→5 | 2→5 |

### Economía por tarea

| Tarea | ΔR (IC 95 %) | p Holm ΔR | T_base | T_mem | ΔT (IC 95 % bootstrap) | memory_tokens | R_mem | ΔR* = ΔT·R_mem/T_base | Net Δtokens (IC 95 %) | p Holm net | Identidad ΔT·R_mem − ΔR·T_base | Wall s (none→pb) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| original | -0.50 (-1.17, +0.17) | 0.140 | 1778 | 2187 | +409 (+315, +497) | 299 | 4.45 | 1.02 | +2709 (+999, +4419) | 0.006 | +2709 | 56.5→66.7 |
| loop | -1.55 (-2.16, -0.94) | 0.000 | 1762 | 2369 | +607 (+512, +697) | 299 | 5.05 | 1.74 | +5794 (+4011, +7577) | 0.000 | +5794 | 64.5→83.3 |
| move | -1.75 (-3.26, -0.24) | 0.049 | 1935 | 2463 | +528 (+303, +748) | 299 | 6.15 | 1.68 | +6635 (+2031, +11239) | 0.006 | +6635 | 86.9→158.6 |

Notas: ΔR > 0 = el playbook ahorra rondas. T = razón de medias (total de tokens de entrada / total de rondas del brazo); con ese estimador la columna «Identidad» reproduce el Δ neto exacto (Ec. 1). La versión del 2026-11-06 usaba la media de las razones por corrida, con la que la identidad no cierra (error de 2 a 200 tokens). `memory_tokens` es la estimación del bench (caracteres / 4, `crates/braze-bench/src/memory.rs::estimate_tokens`), no un conteo del tokenizador: ΔT no separa inyección de cambio de trayectoria. IC de ΔT: bootstrap percentil, 10.000 réplicas, semilla 0, remuestreo de corridas dentro de cada brazo.

### Holdout (ornith:9b)

- Con playbook, sin brazo pareado: 18/20 aprobadas, 3.35 rondas medias.

## Mecanismo (§6.1)

### Llamadas por herramienta por corrida (none→pb), ambos ejecutores

| Ejecutor | Herramienta | original | loop | move |
|---|---|---|---|---|
| gpt-oss:20b | `edit_file` | 0.80→0.95 (Holm 0.648) | 0.80→0.95 (Holm 0.491) | 1.05→1.00 (Holm 0.648) |
| gpt-oss:20b | `glob` | 0.60→0.60 (Holm 1.000) | 0.55→0.55 (Holm 1.000) | 0.65→0.40 (Holm 0.365) |
| gpt-oss:20b | `read_file` | 2.05→1.20 (Holm 0.000) | 1.55→1.15 (Holm 0.209) | 1.85→1.65 (Holm 0.209) |
| gpt-oss:20b | `search` | 0.05→0.00 (Holm 1.000) | 0.00→0.00 (Holm 1.000) | 0.00→0.00 (Holm 1.000) |
| gpt-oss:20b | `shell_exec` | 1.90→1.95 (Holm 1.000) | 1.50→1.75 (Holm 0.488) | 1.85→2.00 (Holm 0.488) |
| gpt-oss:20b | `write_file` | 0.30→0.10 (Holm 0.369) | 0.20→0.05 (Holm 0.369) | 0.00→0.00 (Holm 1.000) |
| ornith:9b | `edit_file` | 1.00→1.00 (Holm 1.000) | 0.80→1.15 (Holm 0.194) | 1.25→2.00 (Holm 0.151) |
| ornith:9b | `glob` | 0.15→0.05 (Holm 0.931) | 0.00→0.00 (Holm 1.000) | 0.10→0.05 (Holm 1.000) |
| ornith:9b | `read_file` | 1.05→1.15 (Holm 0.620) | 1.00→1.45 (Holm 0.013) | 1.55→1.75 (Holm 0.620) |
| ornith:9b | `shell_exec` | 0.80→1.70 (Holm 0.009) | 0.55→1.70 (Holm 0.000) | 0.50→1.65 (Holm 0.001) |
| ornith:9b | `write_file` | 0.00→0.00 (Holm 1.000) | 0.15→0.15 (Holm 1.000) | 0.10→0.25 (Holm 1.000) |

- Estudio 1, corridas aprobadas de loop y move (ambos brazos): 62; con exactamente un `edit_file`: 56.

### Potencia y efectos mínimos detectables (α = 0,05, potencia 80 %)

Tasa de éxito, Fisher bilateral con n = 20 por celda (simulación, 4.000 réplicas, semilla 1), base 0,70:

| Caída de la tasa | Potencia |
|---|---|
| 0.2 | 0.17 |
| 0.3 | 0.34 |
| 0.4 | 0.59 |
| 0.5 | 0.84 |

Rondas: efecto mínimo detectable por tarea, (z₀.₉₇₅ + z₀.₈₀) × EE de Welch:

| Ejecutor | original | loop | move |
|---|---|---|---|
| gpt-oss:20b | 0.95 | 0.76 | 0.45 |
| ornith:9b | 0.93 | 0.84 | 2.09 |

## Estudio 2 (pre-registrado)

### Estudio 2: timeouts y rendimiento por brazo

| Brazo | Pass | Timeouts (900 s) | Pass entre no-timeouts | Mediana wall (s) | Mediana s/ronda | Mediana s/ronda sin timeouts | Media tokens de entrada | Media rondas |
|---|---|---|---|---|---|---|---|---|
| baseline | 59/102 | 38 | 59/64 | 589 | 100 | 89.7 | 13526 | 5.5 |
| empty | 68/102 | 23 | 68/79 | 384 | 58 | 53.8 | 18709 | 6.7 |
| seeded | 72/102 | 21 | 72/81 | 337 | 61 | 54.8 | 17559 | 6.1 |

### Estudio 2: McNemar con y sin pares que involucran un timeout

| Contraste | Discordantes (a gana / b gana) | p | Con timeout | Sin timeout: discordantes | p sin timeout |
|---|---|---|---|---|---|
| seeded − baseline | 18 / 5 | 0.011 | 19 | 2 / 2 | 1.000 |
| empty − baseline | 15 / 6 | 0.078 | 18 | 0 / 3 | 0.250 |
| seeded − empty | 14 / 10 | 0.541 | 18 | 5 / 1 | 0.219 |

### Estudio 2: unidades, Holm y pruebas a nivel de tarea

Unidad del McNemar: celdas (tarea, repetición), 102 por brazo (34 tareas × 3). Las diferencias de pass se cuentan en celdas.

| Contraste | Δ celdas (pp) | p McNemar | Holm (2 contrastes vs baseline) | Signo exacto (tareas, ceros fuera) | Wilcoxon exacto por enumeración (ceros fuera) | Wilcoxon scipy wilcox/aprox. | Wilcoxon scipy pratt/aprox. | Wilcoxon scipy zsplit/aprox. |
|---|---|---|---|---|---|---|---|---|
| seeded − baseline | +13 (+12.7) | 0.011 | 0.021 | 9 vs 2, p = 0.065 | p = 0.051 (n = 11) | 0.042 | 0.033 | 0.067 |
| empty − baseline | +9 (+8.8) | 0.078 | 0.078 | 9 vs 4, p = 0.267 | p = 0.156 (n = 13) | 0.122 | 0.145 | 0.192 |
| seeded − empty | +4 (+3.9) | 0.541 | — | 8 vs 5, p = 0.581 | p = 0.602 (n = 13) | 0.543 | 0.431 | 0.481 |

Nota: la variante primaria del paper es el Wilcoxon exacto condicional por enumeración de signos con ceros descartados; las variantes de scipy (aproximación normal) se listan porque cambian el lado del 0,05.

- Efecto mínimo detectable del McNemar seeded − baseline con 23 discordantes: 17 vs 6, es decir Δ = 11 celdas (10.8 pp).

## Estudio 2, re-corrida intercalada (pre-registrada 2026-11-17, enmienda 1)

### Validez de la corrida (criterio 1)

- Metadata: commit `0fe2725`, huella de suite `3daaf2e779f06c8f`, orden `interleaved`, semilla de intercalado 20261117, 3 repeticiones, timeout 900 s, motor `llama-cpp-2 0.1.152+cuda`.
- Entorno registrado: {'BRAZE_LOCAL_FAMILY': 'harmony', 'BRAZE_LOCAL_KV_OFFLOAD': 'host', 'BRAZE_MAX_TOKENS': '12288', 'BRAZE_OLLAMA_NUM_CTX': '32768'}.
- Filas: 306 (baseline 102, empty 102, seeded 102); fallos de infraestructura: 0 (0.0 %, umbral 10 %).
- Corridas con `cargo check` aprobado: 259 de 306 (control del instrumento tras la enmienda 1).
- Mediana de segundos por ronda: baseline 31.2, empty 32.6, seeded 33.9; diferencia máxima entre brazos 8.7 % (umbral de declaración 20 %).
- Hueco máximo entre filas consecutivas del `run_log`: 0.0 min (umbral de declaración 10 min). Duración total: 29.7 h.

### Estudio 2: timeouts y rendimiento por brazo

| Brazo | Pass | Timeouts (900 s) | Pass entre no-timeouts | Mediana wall (s) | Mediana s/ronda | Mediana s/ronda sin timeouts | Media tokens de entrada | Media rondas |
|---|---|---|---|---|---|---|---|---|
| baseline | 77/102 | 7 | 77/95 | 268 | 31 | 30.5 | 25149 | 8.4 |
| empty | 71/102 | 8 | 71/94 | 283 | 33 | 31.5 | 26809 | 8.7 |
| seeded | 70/102 | 12 | 70/90 | 234 | 34 | 32.7 | 25951 | 7.8 |

### Estudio 2: McNemar con y sin pares que involucran un timeout

| Contraste | Discordantes (a gana / b gana) | p | Con timeout | Sin timeout: discordantes | p sin timeout |
|---|---|---|---|---|---|
| seeded − baseline | 13 / 20 | 0.296 | 13 | 9 / 11 | 0.824 |
| empty − baseline | 10 / 16 | 0.327 | 7 | 6 / 13 | 0.167 |
| seeded − empty | 12 / 13 | 1.000 | 11 | 8 / 6 | 0.791 |

### Estudio 2: unidades, Holm y pruebas a nivel de tarea

Unidad del McNemar: celdas (tarea, repetición), 102 por brazo (34 tareas × 3). Las diferencias de pass se cuentan en celdas.

| Contraste | Δ celdas (pp) | p McNemar | Holm (2 contrastes vs baseline) | Signo exacto (tareas, ceros fuera) | Wilcoxon exacto por enumeración (ceros fuera) | Wilcoxon scipy wilcox/aprox. | Wilcoxon scipy pratt/aprox. | Wilcoxon scipy zsplit/aprox. |
|---|---|---|---|---|---|---|---|---|
| seeded − baseline | -7 (-6.9) | 0.296 | 0.592 | 8 vs 10, p = 0.815 | p = 0.175 (n = 18) | 0.178 | 0.406 | 0.428 |
| empty − baseline | -6 (-5.9) | 0.327 | 0.592 | 5 vs 9, p = 0.424 | p = 0.300 (n = 14) | 0.237 | 0.263 | 0.308 |
| seeded − empty | -1 (-1.0) | 1.000 | — | 6 vs 7, p = 1.000 | p = 1.000 (n = 13) | 0.782 | 0.782 | 0.804 |

Nota: la variante primaria del paper es el Wilcoxon exacto condicional por enumeración de signos con ceros descartados; las variantes de scipy (aproximación normal) se listan porque cambian el lado del 0,05.

- Efecto mínimo detectable del McNemar seeded − baseline con 33 discordantes: 23 vs 10, es decir Δ = 13 celdas (12.7 pp).

### Re-corrida intercalada: timeouts, costo y deriva

| Contraste | Timeouts (a vs b) | p Fisher |
|---|---|---|
| seeded − baseline | 12 vs 7 | 0.336 |
| empty − baseline | 8 vs 7 | 1.000 |
| seeded − empty | 12 vs 8 | 0.481 |

| Brazo | Mediana s/ronda (todas) | Mediana s/ronda (sin timeouts) | Media tokens de entrada | Media rondas | Media wall (s) |
|---|---|---|---|---|---|
| baseline | 31.2 | 30.5 | 25149 | 8.43 | 341 |
| empty | 32.6 | 31.5 | 26809 | 8.74 | 353 |
| seeded | 33.9 | 32.7 | 25951 | 7.82 | 353 |

- Piso de ruido dentro del sweep: 26 de 102 celdas (25 %) difieren entre `empty` y `baseline`, cuyos prompts son idénticos.

Deriva: mediana de s/ronda por tercio del sweep (orden del `run_log`) y brazo.

| Tercio | baseline | empty | seeded |
|---|---|---|---|
| 1 | 31.2 (n=34) | 31.8 (n=34) | 36.1 (n=34) |
| 2 | 34.5 (n=34) | 33.8 (n=34) | 33.4 (n=34) |
| 3 | 30.2 (n=34) | 33.4 (n=34) | 33.4 (n=34) |

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
