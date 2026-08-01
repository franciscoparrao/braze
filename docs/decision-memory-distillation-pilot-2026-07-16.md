# Decisión: piloto M1 de memoria procedimental (none vs human-playbook)

Fecha: 2026-07-16
Línea: paper2-learning
Documentos relacionados: `docs/hypothesis-2026-07-16-memory-distillation.md`,
`docs/paper2-memory-distillation-protocol-2026-07-16.md`,
`docs/sweep-memory-distillation-pilot-2026-07-16.md`

## Decisión

Adoptar **eficiencia (`turns_to_success`, `wall_time_ms`, `output_tokens`)** como el endpoint
primario reportable de este piloto para la familia `rust_compile_repair`, no `success_rate`.
Continuar la línea de investigación (Paper 2) — no se cumple el criterio de retiro — pero corregir
el diseño de tarea antes de invertir en más n o en automatizar la destilación: la tarea `B` actual
está memorizada por el modelo y no puede discriminar transferencia en `success_rate` por techo.
Próximo paso técnico: diseñar una tarea `B` de la misma familia (`rust_compile_repair`, mismo
`applies_when` del playbook: E0499/E0502/E0382) con un patrón de bug menos canónico, para dar
espacio a un efecto de éxito medible antes de escalar n de nuevo.

## Evidencia

- `docs/sweep-memory-distillation-gptoss20b-r20-2026-07-16.json` (n=20, condición limpia post-fix
  de harness, commit `3ec35dc`): `none` 16/20 pass, `human-playbook` 16/20 pass (Fisher p=1.0,
  `transfer_gain=0`); rounds 7.05→5.85 (t-test p=0.00083, Cohen d=1.15); wall time 56.6s→44.2s
  (p=0.0086, d=0.88); output_tokens d=1.05.
- Progresión n=5→n=10→n=20 documentada en `docs/sweep-memory-distillation-pilot-2026-07-16.md`:
  el delta de pass rate visto a n=10 (+20pp) se disolvió a empate exacto a n=20 — confirma que era
  ruido, no descarta el efecto de eficiencia (que se volvió *más* significativo, no menos, al subir n).
  holdout 20/20 en las tres escalas, sin degradación.

## Métricas

Primarias (según `docs/paper2-memory-distillation-protocol-2026-07-16.md`):
`success_rate` (sin efecto, transfer_gain=0), `turns_to_success` (efecto grande, p<0.001).
`leader_escalations` y `estimated_cost_usd` no aplican a este piloto (M1 no involucra escalación
cloud ni condición `lead-fallback`).

Secundarias: `output_tokens`/`input_tokens` bajan en la condición con memoria (no suben — descarta
la regla anti-deriva #7 del framework), `permission_denials`/`schema_validation_failures` en ruido
de fondo bajo (≤7 en 60 filas a n=20, sin patrón por condición).

## Scope donde aplica

- Familia `rust_compile_repair`, bugs de la clase E0499/E0502/E0382 que el modelo ya resuelve con
  alta probabilidad sin ayuda (tarea saturada/memorizada): la memoria procedimental reduce el costo
  de llegar a la solución (rounds, wall time, tokens) de forma robusta y con effect size grande.
- Backend `ollama:gpt-oss:20b` contra Nitro, bajo el harness `braze-bench` post-fix (commit `3ec35dc`).

## Scope donde NO aplica (todavía)

- No hay evidencia de que la memoria procedimental mejore `success_rate` en absoluto — ni en esta
  tarea (saturada) ni en tareas más difíciles (sin medir aún).
- No se probó `procedural` (auto-destilado), `episodic` ni `summary` — el resultado es específico a
  `human-playbook` como techo; no se sabe si un playbook auto-destilado conserva el beneficio de
  eficiencia.
- Una sola familia de tarea, un solo backend, un solo par A/B concreto — sin evidencia de
  generalización entre familias (`tool_schema_repair`, `multi_file_edit` siguen sin pilotearse).

## Riesgos

- El beneficio de eficiencia podría ser específico a que el playbook coincide semánticamente con
  la estrategia que el modelo ya usa (narrow-scope-then-clone) — no necesariamente prueba que
  cualquier procedimiento correcto acelera al modelo; podría ser un artefacto de *este* playbook
  particular reforzando *este* patrón particular.
- Sin condición `procedural` medida, el claim de paper no puede ir más allá de "un playbook humano
  bien escrito ayuda a la eficiencia" — insuficiente para el claim central del protocolo ("amortizar
  escalación cloud como memoria reusable") sin la etapa de auto-destilación.
- El grader (`expect_file_contains` por substring literal) puede generar falsos negativos frente a
  fixes semánticamente correctos pero con forma textual distinta — riesgo conocido, no cuantificado
  en este piloto (se revisó una vez en un sweep anterior y no se encontró evidencia de que estuviera
  ocurriendo, pero no se descartó sistemáticamente).

## Estado nuevo

`experimental` — la línea sigue activa (Paper 2), pero el resultado de este piloto concreto
(tarea `rust_borrow_fix_*`) queda **cerrado y archivado** como evidencia de eficiencia, no de
transferencia de éxito. No se promueve a `LearnedPlaybookStore`/integración live todavía: falta
(a) una tarea `B` que no esté en el techo de memorización para medir `success_rate` de verdad, y
(b) la etapa de destilación automática (`procedural`) para saber si esto sobrevive sin curación humana.

## Lecciones

1. No declarar señal desde n=10 en binarios con esta varianza — el efecto de pass rate observado
   ahí no sobrevivió a duplicar la muestra. Para métricas binarias con esta tarea, n=10 es
   insuficiente incluso para descartar ruido de ±20pp.
2. Verificar y eliminar fricción de harness (permission denials, schema failures) *antes* de correr
   el piloto que decide algo — de lo contrario cualquier comparación mide el harness, no la hipótesis.
3. Preferir tareas donde el baseline (`none`) no esté ya en el techo antes de diseñar el experimento
   — la elección de la tarea determina qué métrica puede mostrar señal. Este error de diseño no
   invalidó el piloto (la eficiencia sí discriminó), pero fue suerte de tener una métrica primaria de
   respaldo, no un diseño que lo garantizara.
4. Ollama con `--seed` fijo no es bit-exacto reproducible (13/15 de coincidencia entre corridas con
   el mismo seed derivado) — no asumir determinismo perfecto al diseñar checks de reproducibilidad
   para este pipeline.
