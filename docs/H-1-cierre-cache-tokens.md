# Reporte de cierre H-1: cache tokens en `TaskResult`

Fecha: 2026-07-08
Hallazgo: H-1 de `docs/AUDITORIA-2026-07-v5.md`
Estado: **CERRADO** — sin commitear (worktree dirty)
Verificación: `cargo test --workspace` → 779 tests verdes (+6 nuevos); `cargo clippy --workspace --all-targets -- -D warnings` → limpio.

## Qué era el problema

El WIP del log SI-2 (`docs/usability-log-2026-07-07-si2.md`) añadió `cache_read_tokens: Option<u32>` y `cache_write_tokens: Option<u32>` a los 2/3 superiores del stack:

- `CompletionEvent::Usage` (crates/braze-model/src/backend.rs:42-56) — los backends lo reportan.
- `Engine::RoundUsage` (crates/braze-engine/src/engine.rs:166) — struct que reemplazó el tuple.
- `AgentEvent::Usage` (crates/braze-events/src/event.rs:96-109) — se persiste al rollout log.

**Pero `braze-bench::metrics::compute_metrics`** — la función que convierte el log en el `TaskResult` que va al JSON del paper — no agregaba estos campos. El WIP estaba completo en 2/3 del stack y roto en la métrica final. Para un A/B paper "con vs sin prompt caching" (uso central en ese log), el bench no podía aislar.

## Qué se cambió

### `crates/braze-bench/src/metrics.rs`

1. **`TaskResult` gana dos campos** (`Option<u32>` ambos) justo después de `output_tokens`:
   - `cache_read_tokens` — tokens del prompt que hit cache, sumados por round.
   - `cache_write_tokens` — tokens nuevos escritos a cache.
   - Ambos con `#[serde(skip_serializing_if = "Option::is_none")]` para que backends que no reportan caching (Ollama, Anthropic-native, harness-error) no emitan campos null en el JSON — distinción clave preservada.

2. **`compute_metrics`** ahora agrega con la misma regla que `Engine::complete_with_best_of_n`'s `sum_optional_u32`:
   - `None` por ronda = "este backend no reporta caching para esta ronda" → contribuye 0 al sum, pero NO flipa `None`-overall a `Some`.
   - `Some(N)` por ronda → contribuye N al sum Y flipa overall a `Some`.
   - Resultado: overall `None` sólo si todas las rondas fueron `None` (ningún reporte); overall `Some(sum)` si al menos una ronda reportó, sumando todas las rondas (`None` agregan 0, `Some` agregan N).
   - Esta regla preserva `Some(0)` (genuinamente cero cache hits, reportado) distinguible de `None` (no reportado). **Crítico para el paper** — sin ella, una ablation "sin caching" vs "con caching pero zero hits" sería indistinguible.

3. **`harness_error_result`** inicializa ambos a `None` (la fila nunca corrió rounds → no hay reporte).

4. **`sum_optional_u32` helper privado** añadido — mirror del que ya vive en engine.rs. Mismo nombre, misma semántica, para que un futuro refactor que unifique funciones sea mechanical.

### `crates/braze-bench/src/external.rs`

Dos sitios de `TaskResult` construction en `external_outcome_to_task_result` actualizados a `None` ambos — un external baseline harness (mini-swe-agent eventual) no tiene `AgentEvent` log, así que no reporta cache tokens. Comentado in-line.

### `crates/braze-bench/src/report.rs`

El helper de test `result()` actualizado a `None` ambos.

## Tests nuevos (6)

Todos en `crates/braze-bench/src/metrics.rs` y `report.rs`:

1. `cache_tokens_are_summed_across_rounds_when_any_round_reports_them` — caso A/B del paper: round 1 escribe cache (Some(0) read, Some(9500) write), round 2 lee (Some(10100) read, Some(0) write). Assert total read Some(10100), write Some(9500).
2. `cache_tokens_stay_none_when_no_round_reports_them` — dos rounds ambas `None` → overall `None`. Cubre Ollama / Anthropic-native.
3. `cache_tokens_with_some_zero_is_distinguishable_from_not_reported` — un round `Some(0)`/`Some(0)` → overall `Some(0)`/`Some(0)`, asertar `!= None`. Pinned: si alguien refactoriza a `unwrap_or(0)` + `sum` (perdiendo la distinción), este testROMpe.
4. `cache_tokens_with_mixed_some_and_none_rounds_keep_the_reported_sum` — round 1 `Some(10000)`, round 2 (degraded fallback) `None` → overall `Some(10000)`. Cubre el caso best-of-n / summary fallback donde sólo algunas rondas reportan.
5. `harness_error_result_reports_none_for_cache_tokens` — harness error → `None`/`None`.
6. `task_result_skips_cache_token_fields_in_json_when_none` (en `report.rs`) — serialización: `None` → campos ausentes del JSON; `Some(N)` → campos presentes con integers. Pinned: si alguien quita `#[serde(skip_serializing_if = ...)]`, este testROMpe.

## Por qué `Option<u32>` y no `u32`

`u32` colapsaría tres estados distintos en uno:
- Backend no reporta caching (Ollama, Anthropic-native hoy): no sabe.
- Backend reporta caching, no hubo hits: Some(0).
- Backend reporta caching, hubo N hits: Some(N).

Con `u32`, "no sabe" y "zero hits" ambos 成为 `0`. El paper necesita distinguir — un A/B "con vs sin `enable_prompt_caching=true`" sobre `z-ai/glm-5.2` (que no soporta caching explícito) debe reportar `None` (no impactado) distinto de "glm con caching habilitado pero zero hits" que sería `Some(0)`. La semántica `None` vs `Some(0)` ya está baked en `AgentEvent::Usage` del WIP previo; esto sólo laextiende al bench.

Para análisis numérico del JSON, los consumers pueden hacer `.unwrap_or(0)` llegado el caso — pero el bench no debe colapsar antes.

## Out of scope (H-1 sólo cubrió `TaskResult`)

Lo que NO se hizo y está en backlog:

- **H-2** `BackendSpec::build` no invoca `with_prompt_caching_enabled` en el bench — sigue default-on, no hay `+ablate:no-caching`. Necesita H-2 cerrarse para que este H-1 sirva en un ablation real "con vs sin caching".
- **H-3** métricas de rescates / escalaciones / compaction count / summary fallbacks no trackeadas como AgentEvent variants. Estructura lista para `AgentEvent::TextualRescueApplied`/`EscalationToLead`/etc cuando se decida.
- **H-18** Anthropic-native cache tokens (`usage.cache_read_input_tokens`/`cache_creation_input_tokens`) NO capturados — sólo OpenRouter está cerrado. `anthropic_wire.rs:324-332` los deja `None`.
- **v4 P0.2 TurnBudget** — coste estimado (que requiere estos cache tokens) no se computa todavía. Necesita pricing-table.
- **`RunMetadata`** (metadata.rs): los cache tokens son por-row (`TaskResult`), no agregados a `RunMetadata`. Si se quieren totales sweep-level en el JSON top-level, hay que añadir summ. Por ahora la agregación se hace en el análisis del JSON de results; es consistente con cómo `input_tokens`/`output_tokens` se manejan (también por-row, no en RunMetadata).

## Cómo verificar el cierre

```bash
cargo test -p braze-bench cache_tokens 2>&1 | tail -10
cargo test -p braze-bench task_result_skips 2>&1 | tail -5
cargo test --workspace --no-fail-fast 2>&1 | grep -E "^test result:" | awk '{s+=$4} END {print "Total tests: "s}'
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

Esperado: 6 tests `cache_tokens*` + 1 `task_result_skips*` verdes; total workspace 779 tests verdes (773 + 6); clippy limpio.

## Próximo paso recomendado

**Commit**. El WIP total (20 archivos, 1608 insertions pre-H-1) + este H-1 (~80 líneas) es coherente temáticamente: "Cierre del log SI-2: prompt caching OpenRouter, rescate GLM arg-tags, strip leaked tool-calls, compaction calibrada, read_file clamp al budget, spinner/banner/bordes TUI, **cache tokens agregados al bench**". 

Squash suggests: un solo commit "Close v5 H-1 + log SI-2 (prompt caching, GLM rescue, strip leaked, compaction budget, read_file clamp, TUI polish)" o partir en 3-4 commits temáticos si se quiere bisectabilidad fina.

Después de commitear, siguiente backlog (v5 roadmap Paquete 1):
1. v4 P0.4 SI-2 como TaskDef permanente en suite `self_improvement.toml` (con `+lead:` ya cerrado).
2. H-3 métricas de palancas SLM (`TextualRescueApplied`, `EscalationToLead`, `CompactionOccurred`, `SummaryFallbackAttempted`).
3. H-2 `+ablate:no-caching` parser en bench.
4. H-18 Anthropic-native cache tokens (path Anthropic-direct).
5. Matriz executor solo / +planner / +lead / +ambos publicación primer resultado.