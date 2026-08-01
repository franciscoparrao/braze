# Tarea B más difícil (loop): diseño, deuda de grading, y resultado final

Fecha: 2026-07-16/17
Contexto: seguimiento de `docs/decision-memory-distillation-pilot-2026-07-16.md`,
cuyo próximo paso explícito era diseñar una tarea B de la familia `rust_compile_repair`
que no estuviera en el techo de memorización (la original satura ~80-90% pass rate
independientemente de la memoria). Suite:
`crates/braze-bench/suites/memory-distillation.toml`, par
`rust_borrow_fix_loop_none`/`rust_borrow_fix_loop_human_playbook` (E0502 por mutar un
`Vec` mientras se itera; fix mínimo requiere restructurar con collect-then-extend).
Datos crudos finales: `docs/sweep-memory-distillation-r20-2026-07-16.json` (n=20,
suite completo). Estado: **CERRADO**.

## Camino hasta el resultado limpio (resumen; detalle línea a línea en los comentarios
del TOML y en el historial de commits)

Tres problemas de harness/grading se encontraron y corrigieron, en orden, cada uno
verificado con sesiones preservadas (`BRAZE_BENCH_KEEP_SESSIONS=1`) antes de aceptar el
siguiente número:

1. **Bug de escaping JSON** (no de la tarea): el primer diseño del bug usaba
   `format!("{word}{suffix}")` — el modelo, al citar ese código en `old_string`/
   `new_string` de `edit_file`, dejaba comillas sin escapar y rompía el JSON de su
   propia tool call (`model_backend_error`, 2/3 "none" en el primer smoke).
   Rediseñado sin `format!`/comillas embebidas (usa `.push_str`).
2. **Grader por substring demasiado específico**: con el bug libre de comillas, un
   diagnóstico (`BRAZE_BENCH_KEEP_SESSIONS=1`, n=2×2) mostró que **las 4 muestras
   compilaban correctamente** pero las 4 fallaban `expect_file_contains` — 3/4 usaban
   el fix canónico con la variable llamada `new_words` en vez de `additions`, y la 4ª
   usaba una restructuración por índices, también válida. Aflojados los substrings a
   `Vec::new()` / `self.words.extend(`.
3. **Bug en el needle aflojado**: `self.words.extend(` (con el paréntesis abierto)
   seguía fallando pese a estar literalmente presente en el archivo. Causa: el grader
   usa `contains_as_a_bounded_token`, no `str::contains` — exige que el carácter
   inmediatamente después del match sea no-alfanumérico. Un needle que termina justo
   antes de un identificador (`extend(` seguido de `new_words`) no cumple ese borde.
   Corregido a `self.words.extend` (sin el paréntesis) — verificado con una
   reimplementación standalone de la función antes de gastar otra corrida contra Nitro.

Con las tres correcciones, un smoke de verificación (n=3) dio 3/3 en ambas condiciones
de la tarea loop, confirmando que el grader por fin refleja la realidad.

## Resultado final (n=20, suite completo)

| Condición | Pass | Rounds (media) | Wall time (media) | Tokens/ronda |
|---|---|---|---|---|
| loop / none | 17/20 (85%) | 5.35 | 77.9s | 1318 |
| loop / human-playbook | 15/20 (75%) | 5.55 | **93.5s** | 1621 |

Fisher exact (pass rate) p=0.69 — sin diferencia significativa, y si acaso la dirección
se invierte levemente respecto de lo esperado. Rounds: t-test p=0.43, sin diferencia.
Wall time: t-test p=0.008, **significativo — pero en la dirección contraria** a la
tarea original: con playbook el tiempo de pared es *mayor*, no menor.

De referencia, en el mismo sweep la tarea original replicó el patrón ya visto: `none`
15/20, `human-playbook` 15/20 (empate, saturada), y el holdout 20/20 sin una sola
falla en las tres escalas de n probadas hasta ahora (5/10/20).

## Mecanismo (diagnosticado con sesiones preservadas, n=5 adicional seed=100)

La hipótesis inicial — que el playbook activamente confunde al modelo porque dice
*"avoid: do not add clone as the first fix"* y el fix de esta tarea sí necesita un
`.clone()` (`word.clone()`) — **no se sostuvo**: se inspeccionó la transcripción
completa de la única falla en la muestra de n=5 (`rep1`, `assertion_max_tokens`,
7 rounds, 101.9s) y el modelo aplicó el fix canónico correcto (`edit_file` con
`Vec::new()` + `.extend(...)`) sin vacilar ni desviarse por el consejo del playbook.
Falló por presupuesto, no por razonamiento.

La explicación real es más simple y se confirma en las 20 repeticiones, no solo en la
muestra de 5: **el playbook agrega un costo fijo de tokens por ronda (el contexto de
memoria se reenvía en cada ronda junto con el resto del historial), y ese costo solo
se amortiza si el playbook además reduce el número de rondas.**

- Tarea original: `none` 7.00 rounds → `human-playbook` 6.00 rounds (-1 ronda completa,
  -14%). El playbook sí acelera la convergencia lo suficiente para que, pese a costar
  ~200-300 tokens extra por ronda, el total de tokens de entrada *baje* (9953→9682).
- Tarea loop: `none` 5.35 rounds → `human-playbook` 5.55 rounds (sin diferencia, si
  acaso más rondas). El mismo costo fijo por ronda (1318→1621 tokens/ronda, la misma
  magnitud de tax que en la tarea original) ya no tiene nada que compensar, así que el
  total de tokens de entrada *sube* 27% (7097→9041) y el wall time sube con él.

En otras palabras: el playbook no está "confundiendo" al modelo en la tarea loop —
simplemente no le ahorra rondas (el fix ya era relativamente directo sin ayuda,
5.35 rounds en `none`), y sin ese ahorro de rondas, su costo fijo de contexto queda
sin contrapartida.

## Lectura para el Paper 2

1. El hallazgo de eficiencia de la tarea original (`docs/decision-memory-distillation-
   pilot-2026-07-16.md`) **no generaliza automáticamente** a una tarea de la misma
   familia y con el mismo playbook genérico. La condición para que la memoria
   procedural pague su propio costo de contexto es que reduzca rondas de forma
   confiable, no solo que sea "correcta" o "aplicable" según su `applies_when`.
2. Esto es evidencia a favor de reportar, junto a cualquier claim de mejora de
   eficiencia, la condición bajo la cual se sostiene (aquí: cuánto acorta la
   trayectoria) en vez de generalizar de una sola tarea a "la memoria procedural
   ahorra tokens".
3. El rediseño de la tarea sí logró su objetivo original (headroom en pass rate:
   85%/75% vs. el 100%/saturado de la tarea vieja), aunque a n=20 esa diferencia de
   pass rate específica no alcanza significancia — dejaría de ser sorprendente que
   escalar más n confirme un empate real ahí también, dado que el mecanismo que sí es
   significativo (tokens/wall time) apunta a una ausencia de beneficio, no a un techo
   por saturación de éxito.
