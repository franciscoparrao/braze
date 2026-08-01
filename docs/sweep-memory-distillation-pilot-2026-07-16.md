# Piloto M1 de memoria procedimental: none vs human-playbook (gpt-oss:20b)

Fecha: 2026-07-16
Contexto: primer piloto técnico del Paper 2 (`docs/hypothesis-2026-07-16-memory-distillation.md`,
`docs/paper2-memory-distillation-protocol-2026-07-16.md`), milestone M1 — playbook humano como
techo práctico, sin tutor cloud en el loop. Suite `crates/braze-bench/suites/memory-distillation.toml`
(3 tareas: `rust_borrow_fix_none`, `rust_borrow_fix_human_playbook`, `rust_non_borrow_holdout_with_playbook`),
backend `ollama:gpt-oss:20b` contra Nitro, `--task-timeout-secs 300`, `--seed 42` (cada repetición usa
`seed + repetición`). Binario `braze-bench` en release, commit `3ec35dc`. Datos crudos:
`docs/sweep-memory-distillation-gptoss20b-r5-nofriction-v2-2026-07-16.json` (n=5),
`docs/sweep-memory-distillation-gptoss20b-r10-2026-07-16.json` (n=10),
`docs/sweep-memory-distillation-gptoss20b-r20-2026-07-16.json` (n=20).
Estado: **CERRADO para este bug/tarea** — ver `docs/decision-memory-distillation-pilot-2026-07-16.md`.

## Precondición: fricción de harness eliminada antes de medir

Los primeros sweeps contra `gpt-oss:20b` (n=5, ver commit `3ec35dc`) mostraban 12-14
`permission_denials` y 14 `schema_validation_failures` por sweep de 15 filas — no eran señal de
capacidad del modelo, sino de dos fricciones del harness:

1. `gpt-oss:20b` envía **todo** shell command envuelto en `bash -lc "<cmd>"`; el carve-out de
   `cargo check/build/test` del bench (`is_benchable_cargo`) solo miraba `command[0] == "cargo"` y
   nunca disparaba.
2. El modelo agrega una propiedad `timeout` a `shell_exec` que la tool rechazaba por schema.

Ambas se corrigieron (unwrap estricto de `bash -lc` por whitelist de caracteres; `shell_exec` acepta
y honra `timeout` con clamp 1-3600s) antes de este piloto. Sin este paso, cualquier comparación
`none` vs `human-playbook` habría medido fricción de harness, no transferencia de memoria — ambas
condiciones sufrían la misma fricción por igual, así que el sesgo no favorecía a ninguna, pero
inflaba varianza y reventaba presupuestos (`expect_max_rounds`/`expect_max_tokens`) sin relación con
la pregunta experimental.

## Progresión n=5 → n=10 → n=20

Mismo código, mismo seed base, mismo binario. Cada escalón agrega repeticiones nuevas (no son
sweeps independientes con seeds distintos).

| n | none pass | human-playbook pass | holdout pass | Fisher p (pass rate) | rounds (none→pb) | rounds t-test p | wall (none→pb) | wall t-test p |
|---|---|---|---|---|---|---|---|---|
| 5 | 4/5 (80%) | 4/5 (80%) | 5/5 (100%) | — | 7.0 → 5.8 | — | 64.6s → 46.8s | — |
| 10 | 7/10 (70%) | 9/10 (90%) | 10/10 (100%) | 0.58 | 6.9 → 5.8 | 0.075 | 57.1s → 44.2s | 0.133 |
| 20 | 16/20 (80%) | 16/20 (80%) | 20/20 (100%) | 1.0 | 7.05 → 5.85 | **0.00083** | 56.6s → 44.2s | **0.0086** |

Effect size a n=20 (Cohen's d, none vs human-playbook): rounds d=1.15, wall time d=0.88,
output_tokens d=1.05 — grandes bajo cualquier convención (Cohen 1988: d≥0.8 = "large").

## Hallazgos

1. **El +20pp de pass rate visto a n=10 (70% vs 90%) era ruido, no señal.** Al duplicar n se
   disolvió a empate exacto (16/20 = 16/20, Fisher p=1.0). Lección metodológica explícita: con
   n=10 y esta varianza run-to-run, un delta de 20pp en un binario todavía no es distinguible de
   cero — no declarar victoria de pass rate sin al menos duplicar la muestra que primero la sugirió.

2. **`success_rate_B` — la métrica primaria pre-registrada en `docs/hypothesis-2026-07-16-memory-distillation.md`
   — no muestra efecto.** `transfer_gain = success_B(procedural) - success_B(none) = 0` a n=20. La
   tarea satura para ambas condiciones (~80%) porque el bug (borrow-then-mutate-then-return con
   `.cloned()`) es el patrón de borrow-checker más canónico de Rust — el modelo ya lo tiene
   memorizado sin ayuda, así que el playbook no puede mover una aguja que ya está en el techo.

3. **`turns_to_success` — también métrica primaria pre-registrada — sí muestra efecto real,
   grande y significativo.** rounds 7.05→5.85 (-17%, p=0.00083, d=1.15); wall time 56.6s→44.2s
   (-22%, p=0.0086, d=0.88). El playbook no cambia *si* el modelo resuelve la tarea, cambia *cuánto
   le cuesta* llegar ahí.

4. **El costo neto de tokens BAJA, no sube, a pesar de inyectar memoria.** output_tokens
   564→406 (n=20), input_tokens 9961→9321 — menos rounds implica menos re-lectura de contexto
   acumulado, y esa reducción supera el costo de los ~500 tokens del playbook inyectado. Relevante
   contra la regla anti-deriva #7 del framework ("una memoria que aumenta tokens sin mejorar éxito
   es ruido"): aquí la memoria *reduce* tokens netos, así que no aplica ese descarte incluso sin
   mejora de éxito.

5. **El holdout (`rust_non_borrow_holdout_with_playbook`) es el resultado más limpio y estable
   de los tres**: 5/5 → 10/10 → 20/20 sin una sola falla, 0 denials, 0 schema_fail en casi todo el
   rango. El playbook de borrow-checker no interfiere con una tarea no relacionada (`double(n) = n*2`)
   a pesar de estar inyectado — evidencia directa contra falsos positivos de retrieval en este par
   A/H, aunque el piloto no varía la condición de inyección (siempre se inyecta), así que esto mide
   "no daña" más que "se filtra correctamente".

6. **No determinismo residual de Ollama con seed fijo**: de las 15 filas que comparten
   `(task_id, repetition)` — y por tanto el mismo seed derivado — entre el sweep n=5 y las primeras
   5 repeticiones del sweep n=10, solo 13/15 coincidieron en `passed`. `--seed` en Ollama reduce
   varianza pero no la elimina; cualquier cálculo de poder para este pipeline debe asumir ruido
   genuino corrida-a-corrida incluso a seed fijo, no solo entre seeds distintos.

## Qué NO muestra este piloto

- No mide `procedural` (playbook auto-destilado) — solo `human-playbook` (techo humano) vs `none`.
  Sigue sin resolverse si un playbook destilado automáticamente reproduce este beneficio de
  eficiencia o si el `human-playbook` es un techo optimista.
- No mide `episodic` ni `summary` bajo presupuesto igualado — sin esas celdas no se puede evaluar
  `procedure_advantage` como define el protocolo.
- Una sola familia de tarea (`rust_compile_repair`) y un solo par A/B concreto — el bug es
  memorizable, así que este resultado de "eficiencia sí, éxito no" podría no generalizar a bugs
  menos canónicos donde el modelo realmente necesite el procedimiento para acertar.
