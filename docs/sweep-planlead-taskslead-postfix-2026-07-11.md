# Re-run del brazo tasks+lead con la escalación reactiva viva (post-fix J-1/J-2)

Fecha: 2026-07-12 (lanzado la noche del 07-11; archivos nombrados por la fecha de lanzamiento)
Contexto: la auditoría v7 (`docs/AUDITORIA-2026-07-v7.md`) encontró que en el
brazo `+plan+lead+ablate:task-list` del sweep original
(`docs/sweep-planlead-2026-07-11.md`) **la escalación reactiva estaba muerta
por construcción** (J-2: el resumen de la task list, re-inyectado como mensaje
user cada ronda, reseteaba la racha de fallos del `EscalatingBackend`), y que
el dedup de rondas podía replay-ar decisiones stale bajo compactación (J-1).
Eso dejaba ambigua la lectura del hallazgo central de ese sweep ("tasks+lead
RESTA"): ¿interferencia real de andamiajes, o el bug apagando el lead reactivo?

Este re-run responde esa pregunta: 2 brazos (ancla `+lead` + el brazo afectado)
× 19 tareas × 5 reps = 190 corridas contra Nitro, binario `605f8a1`
(Paquete 1 de v7: fixes J-1, J-2, J-5, J-6 warm-up, J-7, J-17), mismos specs,
seeds, temp 0.2 y suite `default.toml` del original. Cero fallos de red.
Datos: `docs/sweep-planlead-taskslead-postfix-2026-07-11.json`/`.log`.

## Resultados

| Brazo | Original (pre-fix) | Post-fix | Escalaciones reactivas |
|---|---|---|---|
| `3b+lead` (ancla) | 87/95 = 91.6% | 88/95 = **92.6%** [86,96] | 0 → 0 |
| `3b+plan+lead+tasks` | 82/95 = 86.3% | 79/95 = **83.2%** [74,89] | **0 → 9** |

Por skill (post-fix, brazo tasks+lead): single_tool 30/35, no_tool 15/15,
**multi_step 7/15** (8 de las 9 escalaciones dispararon aquí), error_recovery
13/15 (1 escalación), distractor_selection 14/15. Costo: 4.4 rondas promedio
vs 2.4 del ancla, 2.2× tokens de entrada, 1.85× wall-time.

## Hallazgos

1. **El veredicto "tasks+lead RESTA" se CONFIRMA con el mecanismo limpio.**
   Con la escalación reactiva viva (9 episodios reales — el fix J-2 funciona,
   verificado en la columna `escalat` que pasó de 0 a 9), el brazo sigue ~9pp
   debajo del ancla (83.2% vs 92.6%, ICs solapando solo marginalmente). La
   interferencia de andamiajes de estado (lista tipada re-inyectada + apertura
   proactiva del lead proveyendo el mismo servicio) era real, no el artefacto
   del bug. La lectura del paper queda más fuerte, no más débil.

2. **La escalación reactiva dispara exactamente donde debe y aún así no
   rescata.** Las 8 escalaciones de multi_step ocurren en el floundering real
   (8.8 rondas promedio en esa skill), pero multi_step quedó 7/15 (vs 9/15
   pre-fix, diferencia dentro del ruido con n=15). El lead entrando a mitad
   del pantano no deshace la confusión que el doble andamiaje ya creó — otra
   confirmación de que las palancas de CAPACIDAD rinden en rondas tempranas
   (apertura proactiva 92.6%) y poco tarde (reactivo puro 75.8% en el
   3-brazos de `docs/sweep-lead-3brazos-2026-07-10.md`).

3. **El ancla `3b+lead` replica por QUINTA vez en la banda 92-92.6%**
   (92.6 / 92.6 / 92 / 92 / 92.6 en cinco sweeps y cuatro binarios distintos,
   ahora también con warm-up J-6 activo). Sigue siendo el resultado más
   estable del proyecto y valida la comparabilidad del re-run.

4. **Nota de mecanismo**: en el ancla, `escalat = 0` como siempre — las
   tareas convergen dentro de la ventana de apertura proactiva (I-1 de v6).
   Los 2 `sumfall` del brazo tasks+lead indican turnos que necesitaron el
   summary fallback — coherente con el floundering de multi_step.

## Implicación para el paper

El caveat J-2 de § Threats to Validity se puede REEMPLAZAR por este resultado:
el confound existió pero el re-run controlado muestra que no explica el
efecto. La recomendación de configuración del sweep original queda vigente
sin cambios: task-list es la mejor entrega del plan SIN lead; con lead, no
combinarlas.

## Cómo reproducir

```bash
L="ollama:gemma4:e4b"
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 \
cargo run -p braze-bench -- crates/braze-bench/suites/default.toml \
  --backends "ollama:qwen2.5:3b+lead:$L,ollama:qwen2.5:3b+plan:$L+lead:$L+ablate:task-list" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-planlead-taskslead-postfix-<fecha>.json
```
