//! Renders a `Vec<TaskResult>` as a per-backend comparison table on
//! stdout, and optionally as JSON for later analysis.

use std::path::Path;

use crate::error::BenchError;
use crate::metrics::{FailureCause, TaskResult};

/// One backend's aggregated row in the printed table.
struct BackendSummary {
    backend: String,
    total: u32,
    /// Pass FUNCIONAL — la métrica oficial (decisión de banco
    /// 2026-08-12): ver `TaskResult::passed`.
    passed: u32,
    /// Pass ESTRICTO (funcional Y ruta de tool respetada) — la segunda
    /// métrica del reporte dual; difiere de `passed` exactamente en las
    /// filas clase e4b/ornith (logro por tool no listada).
    passed_strict: u32,
    /// Rows excluded from every other field below (N-37,
    /// docs/AUDITORIA-2026-07-v2.md) — reported here, not silently
    /// dropped, per the "no silent caps" principle: a harness-level
    /// failure (sandbox setup, reading back the session log, ...) isn't
    /// a model-capability result at all, and always carries
    /// `wall_time_ms: 0`/zeroed tokens, so folding it into the pass-rate
    /// denominator or the averages dilutes both with rows that measure
    /// nothing about the model.
    harness_errors: u32,
    avg_wall_time_ms: f64,
    /// Median wall-clock time, alongside the average (N-37,
    /// docs/AUDITORIA-2026-07-v2.md) — a handful of slow outliers (a
    /// model stuck near the timeout on one task) skew the average far
    /// more than the median, which is what most repetitions actually
    /// looked like.
    median_wall_time_ms: f64,
    avg_input_tokens: f64,
    avg_output_tokens: f64,
    /// Average number of model completion rounds per task — the central
    /// diagnostic for small models (converging in 2 rounds vs. 14; see
    /// `TaskResult::rounds`'s doc comment).
    avg_rounds: f64,
    schema_validation_failures: u32,
    tool_execution_failures: u32,
    permission_denials: u32,
    /// Totals (not averages, same as the 3 fields above) of the 4
    /// SLM-first levers (H-3, docs/AUDITORIA-2026-07-v5.md) — how many
    /// tasks in this group needed rescue, escalation, compaction, or a
    /// tools-free summary fallback to converge. The whole point of
    /// counting these: two backends tied on `pass_rate` for a skill can
    /// differ wildly in how many of these fired to get there, which is
    /// exactly the SI-2 A/B question (`docs/sweep-si2-lead-ab-2026-07-09.md`)
    /// that motivated this hallazgo in the first place.
    rescued_tool_calls: u32,
    leader_escalations: u32,
    compaction_count: u32,
    summary_fallbacks: u32,
    /// Sum of every row's `estimated_cost_usd` — `None` only when NO row
    /// reported a cost (unpriced models throughout), `Some(sum)` once at
    /// least one did (same aggregation contract as the cache-token
    /// fields; E5's cost/quality tradeoff needs the group total, not an
    /// average that dilutes across unpriced rows).
    total_cost_usd: Option<f64>,
    /// 95% Wilson score interval around `passed/total`, as explicit
    /// `[low, high]` bounds in percent. With `--repetitions 1` (or few
    /// repetitions) a small local model's pass rate is mostly noise, not
    /// signal — this makes the uncertainty visible instead of implying
    /// false precision. See docs/AUDITORIA-2026-07.md hallazgo F3.
    ///
    /// Explicit bounds rather than a `±half_width` suffix (J-5,
    /// docs/AUDITORIA-2026-07-v7.md): the Wilson interval is centered on
    /// the *Wilson center*, not on `passed/total`, and the two diverge
    /// exactly at the extremes this bench lives in — 6/6 has a Wilson
    /// interval of [61%, 100%], while "6/6 (±20pp)" read as if it were
    /// [80%, 120%]. Printing the bounds removes the ambiguity.
    ///
    /// Known remaining limitation (N-37, docs/AUDITORIA-2026-07-v2.md):
    /// this treats each repetition of the same task as an independent
    /// Bernoulli draw, but repeated runs of one task against one backend
    /// are correlated (the same prompt, same tools, same failure modes),
    /// not i.i.d. — the true interval is wider than what's shown here.
    /// Left as-is rather than attempting a clustered/cluster-robust
    /// correction, which is a larger statistical change than the rest of
    /// this fix; likewise full percentiles (p90/p99) beyond the median
    /// added here are deferred.
    pass_rate_ci_low_pct: f64,
    pass_rate_ci_high_pct: f64,
    /// Serie pass^k (k, valor en [0,1]) para k=2..=min(5, repeticiones)
    /// — ver [`pass_hat_k_series`]. Vacía con una sola repetición
    /// (pass^1 ES el pass-rate de arriba).
    pass_hat_k: Vec<(u32, f64)>,
}

/// 95% Wilson score interval for a binomial proportion — chosen over the
/// naive `p ± 1.96*sqrt(p(1-p)/n)` normal approximation because it stays
/// well-behaved at small `n` and at `p` near 0 or 1, both common here
/// (a handful of repetitions, and small models that either reliably pass
/// or reliably fail a given task). Returns `(center, half_width)` as
/// fractions in `[0, 1]`; `total == 0` returns `(0.0, 0.0)`.
fn wilson_interval(passed: u32, total: u32) -> (f64, f64) {
    if total == 0 {
        return (0.0, 0.0);
    }
    let n = total as f64;
    let p = passed as f64 / n;
    // z for 95% confidence.
    let z = 1.96_f64;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denom;
    let half_width = (z / denom) * ((p * (1.0 - p) / n) + z2 / (4.0 * n * n)).sqrt();
    (center, half_width)
}

/// Middle value of an already-sorted slice — the mean of the two middle
/// elements for an even length. `0.0` for an empty slice (mirrors
/// `wilson_interval`'s `total == 0` handling).
fn median(sorted: &[u128]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] as f64 + sorted[mid] as f64) / 2.0
    } else {
        sorted[mid] as f64
    }
}

fn summarize(backend: &str, results: &[&TaskResult]) -> BackendSummary {
    let harness_errors = results
        .iter()
        .filter(|r| r.failure_cause == Some(FailureCause::HarnessError))
        .count() as u32;
    // N-37: excluded from every field below — see `BackendSummary::harness_errors`.
    let counted: Vec<&&TaskResult> = results
        .iter()
        .filter(|r| r.failure_cause != Some(FailureCause::HarnessError))
        .collect();

    let total = counted.len() as u32;
    let passed = counted.iter().filter(|r| r.passed).count() as u32;
    let passed_strict = counted.iter().filter(|r| r.passed_strict).count() as u32;
    let sum_wall_time: u128 = counted.iter().map(|r| r.wall_time_ms).sum();
    let sum_input: u64 = counted.iter().map(|r| r.input_tokens as u64).sum();
    let sum_output: u64 = counted.iter().map(|r| r.output_tokens as u64).sum();
    let sum_rounds: u64 = counted.iter().map(|r| r.rounds as u64).sum();
    let n = total.max(1) as f64;
    let (center, half_width) = wilson_interval(passed, total);
    // The interval is centered on the Wilson center, NOT on passed/total
    // (J-5): clamp to [0, 1] and report both bounds explicitly.
    let ci_low = (center - half_width).max(0.0);
    let ci_high = (center + half_width).min(1.0);

    let mut wall_times: Vec<u128> = counted.iter().map(|r| r.wall_time_ms).collect();
    wall_times.sort_unstable();

    BackendSummary {
        backend: backend.to_string(),
        total,
        passed,
        passed_strict,
        harness_errors,
        avg_wall_time_ms: sum_wall_time as f64 / n,
        median_wall_time_ms: median(&wall_times),
        avg_input_tokens: sum_input as f64 / n,
        avg_output_tokens: sum_output as f64 / n,
        avg_rounds: sum_rounds as f64 / n,
        schema_validation_failures: counted.iter().map(|r| r.schema_validation_failures).sum(),
        tool_execution_failures: counted.iter().map(|r| r.tool_execution_failures).sum(),
        permission_denials: counted.iter().map(|r| r.permission_denials).sum(),
        rescued_tool_calls: counted.iter().map(|r| r.rescued_tool_calls).sum(),
        leader_escalations: counted.iter().map(|r| r.leader_escalations).sum(),
        compaction_count: counted.iter().map(|r| r.compaction_count).sum(),
        summary_fallbacks: counted.iter().map(|r| r.summary_fallbacks).sum(),
        total_cost_usd: sum_optional_f64(counted.iter().map(|r| r.estimated_cost_usd)),
        pass_rate_ci_low_pct: ci_low * 100.0,
        pass_rate_ci_high_pct: ci_high * 100.0,
        pass_hat_k: pass_hat_k_series(&counted),
    }
}

/// pass^k — la métrica de CONFIABILIDAD de tau-bench (Yao et al. 2024,
/// § pass^k), v8 § 6.11: la probabilidad de que k intentos i.i.d. de la
/// misma tarea pasen TODOS. Para un harness cuya tesis es confiabilidad
/// de modelos chicos es la métrica de la tesis: un brazo 80% de
/// pass-rate por moneda-al-aire y uno 80% por "resuelve el 80% de las
/// tareas siempre" son indistinguibles en pass@1 y opuestos en pass^k.
///
/// Estimador insesgado por tarea con n repeticiones y c pases:
/// C(c,k)/C(n,k) (la probabilidad de que k muestras sin reemplazo sean
/// todas pases), promediado sobre las tareas. Serie para k=2..=min(5,
/// reps máximas); k=1 es el pass-rate que la tabla ya reporta. Una
/// tarea con menos de k repeticiones queda fuera del promedio de ese k
/// (no hay estimador insesgado posible con n<k) — con repeticiones
/// homogéneas, el caso normal de braze-bench, no se excluye nada.
fn pass_hat_k_series(counted: &[&&TaskResult]) -> Vec<(u32, f64)> {
    let mut by_task: std::collections::HashMap<&str, (u32, u32)> = std::collections::HashMap::new();
    for row in counted {
        let entry = by_task.entry(row.task_id.as_str()).or_insert((0, 0));
        entry.0 += 1;
        if row.passed {
            entry.1 += 1;
        }
    }
    let max_reps = by_task.values().map(|(n, _)| *n).max().unwrap_or(0);

    (2..=max_reps.min(5))
        .filter_map(|k| {
            let estimates: Vec<f64> = by_task
                .values()
                .filter(|(n, _)| *n >= k)
                .map(|(n, c)| binomial(*c, k) / binomial(*n, k))
                .collect();
            if estimates.is_empty() {
                return None;
            }
            Some((k, estimates.iter().sum::<f64>() / estimates.len() as f64))
        })
        .collect()
}

/// C(n, k) como f64 por producto incremental — exacto para los n chicos
/// de un sweep (repeticiones ≤ ~20) sin riesgo de overflow entero.
fn binomial(n: u32, k: u32) -> f64 {
    if k > n {
        return 0.0;
    }
    (0..k).map(|i| (n - i) as f64 / (k - i) as f64).product()
}

/// Una comparación pareada brazo-vs-control (v8 K-19): los sweeps de
/// braze-bench comparten `seed + repetition` entre brazos, así que cada
/// (task_id, repetition) existe en ambos y forma un PAR — el diseño
/// pareado que el Wilson pooleado de arriba ignora. El test es McNemar
/// exacto: solo los pares DISCORDANTES (uno pasa, el otro no) llevan
/// información; bajo H0 se reparten Binomial(b+c, ½).
pub(crate) struct PairedComparison {
    pub(crate) arm: String,
    /// Pares donde ambos brazos tienen fila contable — los pares con
    /// `HarnessError` en cualquiera de los dos lados se excluyen y se
    /// reportan en `dropped_pairs` (no silent caps).
    pub(crate) n_pairs: u32,
    pub(crate) dropped_pairs: u32,
    /// Discordantes: solo el control pasó (`b`) / solo el brazo pasó (`c`).
    pub(crate) control_only: u32,
    pub(crate) arm_only: u32,
    pub(crate) p_exact: f64,
    /// Ajustado por Holm-Bonferroni sobre la familia "todos los brazos
    /// vs el control" de este sweep — sin esto, 4-5 comparaciones a
    /// α=0.05 esperan ≥1 falso positivo por sweep (v8 K-19).
    pub(crate) p_holm: f64,
}

/// Imprime una fila de comparación pareada — compartido por el pareo
/// intra-sweep y el cross-invocación de DBV (`crate::dbv`), para que las
/// dos tablas se lean idénticas.
pub(crate) fn print_comparison_row(c: &PairedComparison) {
    let significance = if c.p_holm < 0.05 { "  *" } else { "" };
    let dropped_note = if c.dropped_pairs > 0 {
        format!("  [pares sin contraparte: {}]", c.dropped_pairs)
    } else {
        String::new()
    };
    println!(
        "{:<24} pares={:<4} solo-control={:<3} solo-brazo={:<3} p={:.4} p_holm={:.4}{significance}{dropped_note}",
        c.arm, c.n_pairs, c.control_only, c.arm_only, c.p_exact, c.p_holm
    );
}

/// p exacto (dos colas) de McNemar: con `b`+`c` discordantes, bajo H0
/// `b ~ Binomial(b+c, ½)` — p = 2·P(X ≤ min(b,c)), capado a 1. Sin
/// discordantes no hay evidencia en ninguna dirección: p = 1.
fn mcnemar_exact_p(control_only: u32, arm_only: u32) -> f64 {
    let n = control_only + arm_only;
    if n == 0 {
        return 1.0;
    }
    let k = control_only.min(arm_only);
    let half_n = 0.5f64.powi(n as i32);
    let tail: f64 = (0..=k).map(|i| binomial(n, i) * half_n).sum();
    (2.0 * tail).min(1.0)
}

/// Holm-Bonferroni step-down sobre la familia de p-values: ordenados
/// ascendentes, `p_(i)` se ajusta a `max_{j<=i} (m-j)·p_(j)` (índice
/// 0-based), capado a 1 — controla FWER sin la sobre-corrección de
/// Bonferroni plano. Devuelve los ajustados EN EL ORDEN ORIGINAL.
fn holm_adjust(p_values: &[f64]) -> Vec<f64> {
    let m = p_values.len();
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| p_values[a].total_cmp(&p_values[b]));

    let mut adjusted = vec![0.0f64; m];
    let mut running_max = 0.0f64;
    for (rank, &original_index) in order.iter().enumerate() {
        let stepped = ((m - rank) as f64) * p_values[original_index];
        running_max = running_max.max(stepped.min(1.0));
        adjusted[original_index] = running_max;
    }
    adjusted
}

/// Construye las comparaciones pareadas de cada brazo contra el PRIMER
/// brazo del sweep (la convención del proyecto: el control va primero
/// en `--backends`). `None` si hay menos de dos brazos.
/// Celdas contables de un brazo: `(task_id, repetition) -> passed`. Alias
/// compartido por el pareo intra-sweep y el cross-invocación de DBV.
pub(crate) type OutcomeCells = std::collections::HashMap<(String, u32), bool>;

/// Celdas contables de un brazo: `(task_id, repetition) -> passed`,
/// excluyendo las filas `HarnessError` (mismo criterio N-37 que el resto
/// del reporte — un fallo de infra no es señal de capacidad). Compartido
/// por el pareo intra-sweep y el cross-invocación de DBV
/// (`crate::dbv`).
pub(crate) fn outcomes_map(results: &[TaskResult], backend: &str) -> OutcomeCells {
    results
        .iter()
        .filter(|r| r.backend == backend)
        .filter(|r| r.failure_cause != Some(FailureCause::HarnessError))
        .map(|r| ((r.task_id.clone(), r.repetition), r.passed))
        .collect()
}

/// El núcleo del pareo de McNemar: compara cada brazo (por sus celdas
/// contra `control_outcomes`) y llena el Holm sobre la familia. El origen
/// del control lo decide el caller — el primer brazo de ESTA corrida
/// (`paired_comparisons`) o el baseline de un JSON previo
/// (`crate::dbv`, cross-invocación).
pub(crate) fn compare_against_control(
    control_outcomes: &OutcomeCells,
    arm_outcomes: &[(String, OutcomeCells)],
) -> Vec<PairedComparison> {
    let mut comparisons: Vec<PairedComparison> = arm_outcomes
        .iter()
        .map(|(arm, arm_outcomes)| {
            let total_keys: std::collections::HashSet<&(String, u32)> =
                control_outcomes.keys().chain(arm_outcomes.keys()).collect();
            let mut n_pairs = 0u32;
            let mut dropped_pairs = 0u32;
            let mut control_only = 0u32;
            let mut arm_only = 0u32;
            for key in total_keys {
                match (control_outcomes.get(key), arm_outcomes.get(key)) {
                    (Some(&control_passed), Some(&arm_passed)) => {
                        n_pairs += 1;
                        match (control_passed, arm_passed) {
                            (true, false) => control_only += 1,
                            (false, true) => arm_only += 1,
                            _ => {}
                        }
                    }
                    _ => dropped_pairs += 1,
                }
            }
            PairedComparison {
                arm: arm.clone(),
                n_pairs,
                dropped_pairs,
                control_only,
                arm_only,
                p_exact: mcnemar_exact_p(control_only, arm_only),
                p_holm: 0.0,
            }
        })
        .collect();
    let p_values: Vec<f64> = comparisons.iter().map(|c| c.p_exact).collect();
    let adjusted = holm_adjust(&p_values);
    for (comparison, p_holm) in comparisons.iter_mut().zip(adjusted) {
        comparison.p_holm = p_holm;
    }
    comparisons
}

fn paired_comparisons(
    results: &[TaskResult],
    backend_order: &[&str],
) -> Option<Vec<PairedComparison>> {
    let (&control, arms) = backend_order.split_first()?;
    if arms.is_empty() {
        return None;
    }
    let control_outcomes = outcomes_map(results, control);
    let arm_outcomes: Vec<(String, OutcomeCells)> = arms
        .iter()
        .map(|&arm| (arm.to_string(), outcomes_map(results, arm)))
        .collect();
    Some(compare_against_control(&control_outcomes, &arm_outcomes))
}

/// Sums optional per-row USD costs into an optional total — `None` only
/// when every row was `None` (no pricing anywhere: stay silent rather
/// than claim "$0"); `Some(sum)` once at least one row reported.
/// f64 mirror of `metrics::sum_optional_u32`, same reasoning.
fn sum_optional_f64(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut sum = 0.0f64;
    let mut any_reported = false;
    for v in values.flatten() {
        sum += v;
        any_reported = true;
    }
    any_reported.then_some(sum)
}

/// `"$0.0123"` when priced, `"-"` when no row in the group had pricing —
/// visibly different from a genuine $0.0000 (all-Ollama rows).
fn format_cost_cell(total: Option<f64>) -> String {
    match total {
        Some(usd) => format!("${usd:.4}"),
        None => "-".to_string(),
    }
}

/// Prints a per-task detail line followed by a per-backend summary table,
/// grouped and ordered by first appearance of each backend name in
/// `results` (i.e. the order `--backends` listed them in).
pub fn print_table(results: &[TaskResult]) {
    println!("\n== Resultados por tarea ==");
    for result in results {
        let status = if result.passed { "PASS" } else { "FAIL" };
        let cause_suffix = result
            .failure_cause
            .map(|c| format!(" [{c:?}]"))
            .unwrap_or_default();
        // Fila funcional-pass con ruta no respetada (clase e4b/ornith) —
        // visible en el detalle para que la brecha pass/strict de la
        // tabla sea rastreable a filas concretas.
        let route_suffix = if result.passed && !result.passed_strict {
            " [RouteMiss]"
        } else {
            ""
        };
        let error_suffix = result
            .run_error
            .as_ref()
            .map(|e| format!(" (error: {e})"))
            .unwrap_or_default();
        let rep_suffix = if result.repetition > 0 {
            format!(" (rep {})", result.repetition + 1)
        } else {
            String::new()
        };
        println!(
            "[{status}] {:<24} {:<20}{rep_suffix} {:>6}ms  rounds={} tool_calls={}  schema_fail={}  exec_fail={}  denied={}  tokens_in={} tokens_out={}{route_suffix}{cause_suffix}{error_suffix}",
            result.task_id,
            result.backend,
            result.wall_time_ms,
            result.rounds,
            result.tool_calls_total,
            result.schema_validation_failures,
            result.tool_execution_failures,
            result.permission_denials,
            result.input_tokens,
            result.output_tokens,
        );
    }

    let mut backend_order: Vec<&str> = Vec::new();
    for result in results {
        if !backend_order.contains(&result.backend.as_str()) {
            backend_order.push(&result.backend);
        }
    }

    println!("\n== Comparación por backend ==");
    // N-37 (docs/AUDITORIA-2026-07-v2.md): `pass_rate`/`avg_*`/`median_ms`
    // exclude `harness_err` rows from their denominator — printed as its
    // own column instead of silently dropped, since a harness-level
    // failure isn't a model result at all (see
    // `BackendSummary::harness_errors`'s doc comment).
    // Reporte dual (decisión de banco 2026-08-12): `pass_rate` es la
    // métrica FUNCIONAL oficial; `strict` es la misma cuenta exigiendo
    // además la ruta de tool pedida — la brecha entre ambas es
    // exactamente la clase e4b/ornith (logro por tool no listada).
    println!(
        "{:<24} {:>16} {:>8} {:>8} {:>12} {:>10} {:>14} {:>16} {:>17} {:>14} {:>10} {:>12} {:>9} {:>9} {:>9} {:>9} {:>10}",
        "backend",
        "pass_rate[95%CI]",
        "strict",
        "avg_rounds",
        "avg_ms",
        "median_ms",
        "avg_tok_in",
        "avg_tok_out",
        "schema_fail",
        "exec_fail",
        "denied",
        "harness_err",
        "rescues",
        "escalat",
        "compact",
        "sumfall",
        "cost_usd"
    );
    let mut summaries = Vec::new();
    for backend in backend_order {
        let rows: Vec<&TaskResult> = results.iter().filter(|r| r.backend == backend).collect();
        let summary = summarize(backend, &rows);
        let pass_rate_cell = format!(
            "{}/{} [{:.0},{:.0}]%",
            summary.passed,
            summary.total,
            summary.pass_rate_ci_low_pct,
            summary.pass_rate_ci_high_pct
        );
        let strict_cell = format!("{}/{}", summary.passed_strict, summary.total);
        println!(
            "{:<24} {:>16} {:>8} {:>8.1} {:>12.0} {:>10.0} {:>14.0} {:>16.0} {:>17} {:>14} {:>10} {:>12} {:>9} {:>9} {:>9} {:>9} {:>10}",
            summary.backend,
            pass_rate_cell,
            strict_cell,
            summary.avg_rounds,
            summary.avg_wall_time_ms,
            summary.median_wall_time_ms,
            summary.avg_input_tokens,
            summary.avg_output_tokens,
            summary.schema_validation_failures,
            summary.tool_execution_failures,
            summary.permission_denials,
            summary.harness_errors,
            summary.rescued_tool_calls,
            summary.leader_escalations,
            summary.compaction_count,
            summary.summary_fallbacks,
            format_cost_cell(summary.total_cost_usd),
        );
        summaries.push(summary);
    }

    // pass^k (tau-bench) — sección propia en vez de columnas: la tabla
    // de arriba ya está al límite de ancho, y esta serie solo existe
    // con repeticiones > 1. Ver `pass_hat_k_series` para el estimador.
    if summaries.iter().any(|s| !s.pass_hat_k.is_empty()) {
        println!("\n== Confiabilidad pass^k (tau-bench: k intentos, TODOS pasan) ==");
        for summary in &summaries {
            if summary.pass_hat_k.is_empty() {
                continue;
            }
            let cells: Vec<String> = summary
                .pass_hat_k
                .iter()
                .map(|(k, value)| format!("k={k} {:.1}%", value * 100.0))
                .collect();
            println!("{:<24} {}", summary.backend, cells.join("  "));
        }
    }

    // v8 K-19 — el diseño pareado que los seeds compartidos habilitan:
    // McNemar exacto por brazo contra el PRIMER brazo (control), Holm
    // sobre la familia. Complementa (no reemplaza) el Wilson de arriba,
    // cuyo caveat i.i.d. sigue documentado en `BackendSummary`.
    let backend_order_refs: Vec<&str> = summaries.iter().map(|s| s.backend.as_str()).collect();
    if let Some(comparisons) = paired_comparisons(results, &backend_order_refs) {
        println!(
            "\n== Comparación pareada vs control '{}' (McNemar exacto + Holm) ==",
            backend_order_refs[0]
        );
        for c in &comparisons {
            print_comparison_row(c);
        }
        println!(
            "(* p_holm < 0.05; solo los pares discordantes llevan información — \
             'solo-brazo' > 'solo-control' favorece al brazo)"
        );
    }

    // Per-skill breakdown (F8): a flat pass-rate can't show *where* a
    // model's capability ends — grouping by `TaskDef::skill` (when tasks
    // set it) surfaces that a backend might ace single-tool tasks but
    // collapse on multi-step or error-recovery ones.
    let mut skills: Vec<&str> = Vec::new();
    for result in results {
        if let Some(skill) = result.skill.as_deref()
            && !skills.contains(&skill)
        {
            skills.push(skill);
        }
    }
    if !skills.is_empty() {
        // E5 (docs/AUDITORIA-2026-07-v3.md): pass_rate alone hides the
        // cost/quality tradeoff — two backends tied on pass_rate for a
        // skill can differ wildly in how many rounds/tokens/ms it took
        // them to get there, which is exactly the kind of thing that
        // decides whether a lever is "worth it" for small models.
        println!("\n== Comparación por skill ==");
        println!(
            "{:<24} {:<20} {:>9} {:>8} {:>10} {:>10} {:>11} {:>9} {:>9} {:>9} {:>9} {:>10}",
            "backend",
            "skill",
            "pass_rate",
            "avg_rounds",
            "avg_ms",
            "median_ms",
            "avg_tok_out",
            "rescues",
            "escalat",
            "compact",
            "sumfall",
            "cost_usd"
        );
        for backend in {
            let mut order: Vec<&str> = Vec::new();
            for result in results {
                if !order.contains(&result.backend.as_str()) {
                    order.push(&result.backend);
                }
            }
            order
        } {
            for skill in &skills {
                let rows: Vec<&TaskResult> = results
                    .iter()
                    .filter(|r| r.backend == backend && r.skill.as_deref() == Some(*skill))
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                let summary = summarize(backend, &rows);
                let pass_rate_cell = format!("{}/{}", summary.passed, summary.total);
                println!(
                    "{:<24} {:<20} {:>9} {:>8.1} {:>10.0} {:>10.0} {:>11.0} {:>9} {:>9} {:>9} {:>9} {:>10}",
                    backend,
                    skill,
                    pass_rate_cell,
                    summary.avg_rounds,
                    summary.avg_wall_time_ms,
                    summary.median_wall_time_ms,
                    summary.avg_output_tokens,
                    summary.rescued_tool_calls,
                    summary.leader_escalations,
                    summary.compaction_count,
                    summary.summary_fallbacks,
                    format_cost_cell(summary.total_cost_usd),
                );
            }
        }
    }
}

/// One sweep's full JSON output: run-level metadata (E6,
/// docs/AUDITORIA-2026-07-v3.md) alongside the raw per-task results — the
/// metadata is what makes a `results.json` file traceable back to exactly
/// what produced it (sampling, suite version, active ablation, model
/// digests, harness commit), instead of a bare, unreproducible array of
/// numbers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepReport<'a> {
    pub metadata: &'a crate::metadata::RunMetadata,
    pub results: &'a [TaskResult],
}

/// Writes the sweep's metadata plus raw per-task results as JSON, for
/// downstream analysis (e.g. tracking a backend's pass rate over time as
/// `braze` itself changes).
pub fn write_json(
    metadata: &crate::metadata::RunMetadata,
    results: &[TaskResult],
    path: &Path,
) -> Result<(), BenchError> {
    let file = std::fs::File::create(path)?;
    let report = SweepReport { metadata, results };
    serde_json::to_writer_pretty(file, &report)
        .map_err(|err| BenchError::Startup(format!("failed to write JSON report: {err}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_metadata() -> crate::metadata::RunMetadata {
        crate::metadata::RunMetadata {
            sampling: crate::backend_spec::SamplingSpec {
                temperature: 0.2,
                seed: Some(42),
                top_p: None,
                top_k: None,
                repeat_penalty: None,
            },
            repetitions: 3,
            task_timeout_secs: 180,
            turn_wall_clock_secs: None,
            round_wall_clock_secs: None,
            suite_path: "suites/default.toml".to_string(),
            suite_fingerprint: "deadbeef".to_string(),
            braze_git_commit: Some("abc123".to_string()),
            engine_version: None,
            ollama_model_digests: vec![],
            ollama_server_version: Some("0.30.7".to_string()),
            backend_specs: vec![
                "ollama:qwen2.5:3b".to_string(),
                "ollama:qwen2.5:3b+lead:ollama:x+ablate:no-lead".to_string(),
            ],
            local_env: std::collections::BTreeMap::new(),
            ollama_keep_alive: None,
            grading: Some(crate::metadata::GRADING_FUNCTIONAL_DUAL.to_string()),
        }
    }

    /// Regression test for E6 (docs/AUDITORIA-2026-07-v3.md): the JSON
    /// output must carry both the run metadata and the raw results under
    /// distinct top-level keys, not just a bare results array — otherwise
    /// a `results.json` file is unreproducible (no record of what
    /// sampling/suite/commit produced it).
    #[test]
    fn write_json_writes_metadata_alongside_results() {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-report-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.json");

        let metadata = sample_metadata();
        let results = vec![result(true, 100, 10, 2)];
        write_json(&metadata, &results, &path).expect("write must succeed");

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["metadata"]["suite_path"], "suites/default.toml");
        assert_eq!(parsed["metadata"]["repetitions"], 3);
        // H-17 (docs/AUDITORIA-2026-07-v5.md): the run-level record of
        // which backend rows (ablations included) produced this file.
        assert_eq!(
            parsed["metadata"]["backend_specs"][1],
            "ollama:qwen2.5:3b+lead:ollama:x+ablate:no-lead"
        );
        assert_eq!(parsed["results"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["results"][0]["task_id"], "t");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v8 § 6.11 — el estimador insesgado de tau-bench, fijado con los
    /// valores exactos: una tarea 3/5 tiene pass^2 = C(3,2)/C(5,2) =
    /// 3/10, y una tarea perfecta se queda en 1.0 para todo k.
    #[test]
    fn pass_hat_k_matches_the_unbiased_estimator_by_hand() {
        // Tarea "t": 3 pases de 5 repeticiones.
        let mut rows: Vec<TaskResult> = (0..5)
            .map(|i| {
                let mut r = result(i < 3, 10, 1, 1);
                r.repetition = i as u32;
                r
            })
            .collect();
        // Tarea "perfecta": 5/5.
        for i in 0..5 {
            let mut r = result(true, 10, 1, 1);
            r.task_id = "perfecta".to_string();
            r.repetition = i as u32;
            rows.push(r);
        }
        let refs: Vec<&TaskResult> = rows.iter().collect();
        let refs2: Vec<&&TaskResult> = refs.iter().collect();
        let series = pass_hat_k_series(&refs2);

        let k2 = series.iter().find(|(k, _)| *k == 2).unwrap().1;
        // Promedio de tareas: (3/10 + 1.0) / 2 = 0.65
        assert!((k2 - 0.65).abs() < 1e-9, "got {k2}");
        let k5 = series.iter().find(|(k, _)| *k == 5).unwrap().1;
        // C(3,5)=0 para la tarea flaky; (0 + 1.0) / 2 = 0.5
        assert!((k5 - 0.5).abs() < 1e-9, "got {k5}");
    }

    /// Con una sola repetición no hay serie (pass^1 ES el pass-rate), y
    /// una tarea con menos repeticiones que k queda fuera del promedio
    /// de ese k en vez de aportar un estimador imposible.
    #[test]
    fn pass_hat_k_is_empty_for_single_repetition_and_skips_short_tasks() {
        let single = [result(true, 10, 1, 1)];
        let refs: Vec<&TaskResult> = single.iter().collect();
        let refs2: Vec<&&TaskResult> = refs.iter().collect();
        assert!(pass_hat_k_series(&refs2).is_empty());

        // "larga" con 3 reps (2 pases), "corta" con 2 reps (2 pases):
        // en k=3 solo participa "larga" → C(2,3)/C(3,3) = 0.
        let mut rows = Vec::new();
        for i in 0..3 {
            let mut r = result(i < 2, 10, 1, 1);
            r.task_id = "larga".to_string();
            r.repetition = i as u32;
            rows.push(r);
        }
        for i in 0..2 {
            let mut r = result(true, 10, 1, 1);
            r.task_id = "corta".to_string();
            r.repetition = i as u32;
            rows.push(r);
        }
        let refs: Vec<&TaskResult> = rows.iter().collect();
        let refs2: Vec<&&TaskResult> = refs.iter().collect();
        let series = pass_hat_k_series(&refs2);
        let k3 = series.iter().find(|(k, _)| *k == 3).unwrap().1;
        assert!(
            (k3 - 0.0).abs() < 1e-9,
            "solo 'larga' participa en k=3: {k3}"
        );
        // En k=2 participan ambas: (C(2,2)/C(3,2) + 1.0)/2 = (1/3 + 1)/2
        let k2 = series.iter().find(|(k, _)| *k == 2).unwrap().1;
        assert!((k2 - (1.0 / 3.0 + 1.0) / 2.0).abs() < 1e-9, "got {k2}");
    }

    #[test]
    fn binomial_matches_pascal() {
        assert_eq!(binomial(5, 2), 10.0);
        assert_eq!(binomial(3, 3), 1.0);
        assert_eq!(binomial(2, 3), 0.0);
        assert_eq!(binomial(4, 0), 1.0);
    }

    /// v8 K-19 — McNemar exacto fijado con valores a mano:
    /// b=5,c=0 → p = 2·C(5,0)·2⁻⁵ = 2/32; b=1,c=8 → p =
    /// 2·(C(9,0)+C(9,1))·2⁻⁹ = 20/512; sin discordantes → p = 1.
    #[test]
    fn mcnemar_exact_p_matches_hand_computed_values() {
        assert!((mcnemar_exact_p(0, 0) - 1.0).abs() < 1e-12);
        assert!((mcnemar_exact_p(5, 0) - 0.0625).abs() < 1e-12);
        assert!((mcnemar_exact_p(0, 5) - 0.0625).abs() < 1e-12, "simétrico");
        assert!((mcnemar_exact_p(1, 8) - 0.0390625).abs() < 1e-12);
        // Reparto parejo: máxima compatibilidad con H0.
        assert!((mcnemar_exact_p(3, 3) - 1.0).abs() < 1e-9);
    }

    /// Holm step-down fijado a mano: [0.01, 0.04, 0.03] → ordenados
    /// [0.01, 0.03, 0.04] con multiplicadores 3,2,1 → [0.03, 0.06,
    /// max(0.06, 0.04)] = [0.03, 0.06, 0.06] mapeado al orden original.
    #[test]
    fn holm_adjust_matches_hand_computed_values() {
        let adjusted = holm_adjust(&[0.01, 0.04, 0.03]);
        assert!((adjusted[0] - 0.03).abs() < 1e-12);
        assert!((adjusted[1] - 0.06).abs() < 1e-12);
        assert!((adjusted[2] - 0.06).abs() < 1e-12);
        // Un solo p-value: Holm es identidad (capada a 1).
        let single = holm_adjust(&[0.2]);
        assert!((single[0] - 0.2).abs() < 1e-12);
        assert!(holm_adjust(&[0.9, 0.8]).iter().all(|p| *p <= 1.0));
    }

    /// El pareo usa (task_id, repetition), excluye pares donde falta la
    /// contraparte contable, y cuenta discordantes en la dirección
    /// correcta.
    #[test]
    fn paired_comparisons_pair_by_task_and_rep_and_count_discordants() {
        fn row(backend: &str, task: &str, rep: u32, passed: bool) -> TaskResult {
            let mut r = result(passed, 10, 1, 1);
            r.backend = backend.to_string();
            r.task_id = task.to_string();
            r.repetition = rep;
            r
        }
        let mut rows = vec![
            // Par concordante (ambos pasan).
            row("control", "a", 0, true),
            row("brazo", "a", 0, true),
            // Discordante: solo el brazo pasa.
            row("control", "b", 0, false),
            row("brazo", "b", 0, true),
            // Discordante: solo el control pasa.
            row("control", "a", 1, true),
            row("brazo", "a", 1, false),
            // El control tiene una fila cuyo par del brazo es
            // harness_error → el par se excluye y se cuenta.
            row("control", "c", 0, true),
        ];
        let mut orphan = row("brazo", "c", 0, false);
        orphan.failure_cause = Some(FailureCause::HarnessError);
        rows.push(orphan);

        let comparisons =
            paired_comparisons(&rows, &["control", "brazo"]).expect("dos brazos → Some");
        assert_eq!(comparisons.len(), 1);
        let c = &comparisons[0];
        assert_eq!(c.n_pairs, 3);
        assert_eq!(c.dropped_pairs, 1);
        assert_eq!(c.control_only, 1);
        assert_eq!(c.arm_only, 1);
        // b=1, c=1 → p = 2·C(2,0+..=1)... k=1, n=2: 2·(C(2,0)+C(2,1))·¼
        // capado a 1.
        assert!((c.p_exact - 1.0).abs() < 1e-9);

        assert!(
            paired_comparisons(&rows, &["control"]).is_none(),
            "con un solo brazo no hay comparación"
        );
    }

    fn result(
        passed: bool,
        wall_time_ms: u128,
        input_tokens: u32,
        output_tokens: u32,
    ) -> TaskResult {
        TaskResult {
            backend: "ollama:x".to_string(),
            task_id: "t".to_string(),
            skill: None,
            memory_condition: None,
            memory_file: None,
            memory_budget_tokens: None,
            memory_tokens: 0,
            repetition: 0,
            converged: passed,
            run_error: None,
            failure_cause: None,
            tool_calls_total: 0,
            tool_call_names: Vec::new(),
            planned: false,
            schema_validation_failures: 0,
            tool_execution_failures: 0,
            permission_denials: 0,
            rounds: 0,
            expected_tool_called: None,
            expected_text_found: None,
            expected_files_found: None,
            expected_cargo_check_passed: None,
            outcome_fingerprint: None,
            ttc_rollouts: None,
            // No budget asserted on this synthetic helper — same `None`
            // (not evaluated) semantics `TaskResult::expected_rounds_
            // within_budget`'s doc comment pins.
            expected_rounds_within_budget: None,
            expected_tokens_within_budget: None,
            input_tokens,
            output_tokens,
            // This test helper builds a synthetic result from
            // `report.rs`'s view — it doesn't run any rounds, so no
            // backend reported cache tokens. `None` (not reported),
            // same as `harness_error_result`.
            cache_read_tokens: None,
            cache_write_tokens: None,
            rescued_tool_calls: 0,
            fence_edits: 0,
            leader_escalations: 0,
            compaction_count: 0,
            summary_fallbacks: 0,
            harness_notes: 0,
            expected_cost_within_budget: None,
            estimated_cost_usd: None,
            wall_time_ms,
            passed,
            passed_strict: passed,
        }
    }

    /// H-3 (docs/AUDITORIA-2026-07-v5.md): the 4 SLM-lever fields sum
    /// (not average) across rows, same as `schema_validation_failures`
    /// already does — this is the aggregation `docs/sweep-si2-lead-ab-
    /// 2026-07-09.md`'s follow-up question needs (how many rescues vs.
    /// escalations per skill).
    #[test]
    fn summarize_sums_slm_levers_across_all_rows() {
        let mut a = result(true, 100, 10, 2);
        a.rescued_tool_calls = 2;
        a.leader_escalations = 1;
        a.compaction_count = 0;
        a.summary_fallbacks = 1;
        let mut b = result(true, 300, 20, 4);
        b.rescued_tool_calls = 1;
        b.leader_escalations = 0;
        b.compaction_count = 3;
        b.summary_fallbacks = 0;
        let rows = vec![&a, &b];

        let summary = summarize("ollama:x", &rows);

        assert_eq!(summary.rescued_tool_calls, 3);
        assert_eq!(summary.leader_escalations, 1);
        assert_eq!(summary.compaction_count, 3);
        assert_eq!(summary.summary_fallbacks, 1);
    }

    #[test]
    fn summarize_averages_across_all_rows() {
        let a = result(true, 100, 10, 2);
        let b = result(false, 300, 20, 4);
        let rows = vec![&a, &b];

        let summary = summarize("ollama:x", &rows);

        assert_eq!(summary.total, 2);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.avg_wall_time_ms, 200.0);
        assert_eq!(summary.avg_input_tokens, 15.0);
        assert_eq!(summary.avg_output_tokens, 3.0);
    }

    #[test]
    fn summarize_of_empty_results_does_not_divide_by_zero() {
        let rows: Vec<&TaskResult> = Vec::new();
        let summary = summarize("ollama:x", &rows);
        assert_eq!(summary.total, 0);
        assert_eq!(summary.avg_wall_time_ms, 0.0);
    }

    #[test]
    fn summarize_averages_rounds_across_all_rows() {
        let mut a = result(true, 100, 10, 2);
        a.rounds = 2;
        let mut b = result(false, 300, 20, 4);
        b.rounds = 8;
        let rows = vec![&a, &b];

        let summary = summarize("ollama:x", &rows);

        assert_eq!(summary.avg_rounds, 5.0);
    }

    #[test]
    fn wilson_interval_of_empty_sample_is_zero_width() {
        let (_center, half_width) = wilson_interval(0, 0);
        assert_eq!(half_width, 0.0);
    }

    #[test]
    fn wilson_interval_is_wide_for_a_single_repetition() {
        // With n=1, a single pass/fail tells you almost nothing — the
        // interval must be wide (this is the whole point of reporting it:
        // making that uncertainty visible instead of implying a 100% or
        // 0% pass rate is a real signal).
        let (_center, half_width) = wilson_interval(1, 1);
        assert!(
            half_width > 0.2,
            "expected a wide interval, got {half_width}"
        );
    }

    #[test]
    fn wilson_interval_narrows_with_more_repetitions() {
        // Same observed proportion (80%), more samples: the interval must
        // shrink — this is the statistical justification for
        // `--repetitions` actually making the comparison meaningful.
        let (_c1, narrow_at_5) = wilson_interval(4, 5);
        let (_c2, narrow_at_50) = wilson_interval(40, 50);
        assert!(
            narrow_at_50 < narrow_at_5,
            "expected the interval to narrow with more repetitions: n=5 -> {narrow_at_5}, n=50 -> {narrow_at_50}"
        );
    }

    #[test]
    fn median_of_an_odd_length_slice_is_the_middle_value() {
        assert_eq!(median(&[10, 20, 30]), 20.0);
    }

    #[test]
    fn median_of_an_even_length_slice_averages_the_two_middle_values() {
        assert_eq!(median(&[10, 20, 30, 40]), 25.0);
    }

    #[test]
    fn median_of_an_empty_slice_is_zero() {
        assert_eq!(median(&[]), 0.0);
    }

    /// Regression test for N-37 (docs/AUDITORIA-2026-07-v2.md): a
    /// `HarnessError` row (sandbox setup failed, session log unreadable,
    /// ...) isn't a model-capability result — it must not count toward
    /// the pass-rate denominator or dilute the wall-time/token averages
    /// with its always-zeroed fields.
    #[test]
    fn summarize_excludes_harness_errors_from_denominator_and_averages() {
        let a = result(true, 100, 10, 2);
        let b = result(true, 300, 30, 6);
        let mut harness_failure = result(false, 0, 0, 0);
        harness_failure.failure_cause = Some(crate::metrics::FailureCause::HarnessError);
        let rows = vec![&a, &b, &harness_failure];

        let summary = summarize("ollama:x", &rows);

        assert_eq!(
            summary.total, 2,
            "the harness-error row must not count toward total"
        );
        assert_eq!(summary.passed, 2);
        assert_eq!(summary.harness_errors, 1);
        assert_eq!(
            summary.avg_wall_time_ms, 200.0,
            "a harness-error row's wall_time_ms:0 must not drag the average down"
        );
    }

    #[test]
    fn summarize_computes_a_median_wall_time_alongside_the_average() {
        // One slow outlier (900ms) skews the average far more than the
        // median — this is the whole point of reporting both.
        let a = result(true, 100, 0, 0);
        let b = result(true, 100, 0, 0);
        let c = result(true, 900, 0, 0);
        let rows = vec![&a, &b, &c];

        let summary = summarize("ollama:x", &rows);

        assert_eq!(summary.median_wall_time_ms, 100.0);
        assert!(summary.avg_wall_time_ms > summary.median_wall_time_ms);
    }

    /// `TaskResult`'s cache-token fields are `#[serde(skip_serializing_if =
    /// "Option::is_none")]` (docs/AUDITORIA-2026-07-v5.md, H-1): a row from
    /// a backend that doesn't report caching (Ollama, Anthropic-native
    /// today, a harness-error row, ...) must NOT emit the fields at all in
    /// the JSON — keeping them apart from a backend that DID report
    /// `Some(0)` (genuinely zero cache hits, still serialized). This pins
    /// the skip behavior so a future refactor that drops the attribute
    /// (and starts emitting `"cache_read_tokens": null` for every
    /// non-caching row) breaks this test instead of silently bloating
    /// every JSON file.
    #[test]
    fn task_result_skips_cache_token_fields_in_json_when_none() {
        // `None` (not reported): the fields must be absent from the JSON.
        let none_row = result(true, 100, 0, 0);
        let json = serde_json::to_value(&none_row).expect("serialize");
        assert!(
            json.get("cache_read_tokens").is_none(),
            "None cache_read_tokens must be skipped, not serialized as null: {json}"
        );
        assert!(
            json.get("cache_write_tokens").is_none(),
            "None cache_write_tokens must be skipped, not serialized as null: {json}"
        );

        // `Some(N)` (genuinely reported): the fields must appear, with
        // their integer values, so a paper A/B reader can sum/compare
        // them across rows.
        let mut some_row = none_row;
        some_row.cache_read_tokens = Some(10_100);
        some_row.cache_write_tokens = Some(9_500);
        let json = serde_json::to_value(&some_row).expect("serialize");
        assert_eq!(
            json.get("cache_read_tokens"),
            Some(&serde_json::json!(10_100))
        );
        assert_eq!(
            json.get("cache_write_tokens"),
            Some(&serde_json::json!(9_500))
        );
    }

    #[test]
    fn task_result_serializes_tool_call_names_even_when_empty() {
        let mut row = result(true, 100, 0, 0);
        let json = serde_json::to_value(&row).expect("serialize empty tool names");
        assert_eq!(json.get("tool_call_names"), Some(&serde_json::json!([])));

        row.tool_call_names = vec!["read_file".to_string(), "write_file".to_string()];
        row.tool_calls_total = row.tool_call_names.len() as u32;
        let json = serde_json::to_value(&row).expect("serialize tool names");
        assert_eq!(
            json.get("tool_call_names"),
            Some(&serde_json::json!(["read_file", "write_file"]))
        );
    }
}
