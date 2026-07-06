//! Renders a `Vec<TaskResult>` as a per-backend comparison table on
//! stdout, and optionally as JSON for later analysis.

use std::path::Path;

use crate::error::BenchError;
use crate::metrics::{FailureCause, TaskResult};

/// One backend's aggregated row in the printed table.
struct BackendSummary {
    backend: String,
    total: u32,
    passed: u32,
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
    /// 95% Wilson score interval half-width around `passed/total`, in
    /// percentage points. With `--repetitions 1` (or few repetitions) a
    /// small local model's pass rate is mostly noise, not signal — this
    /// makes the uncertainty visible instead of implying false precision.
    /// See docs/AUDITORIA-2026-07.md hallazgo F3.
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
    pass_rate_interval_pp: f64,
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
    let sum_wall_time: u128 = counted.iter().map(|r| r.wall_time_ms).sum();
    let sum_input: u64 = counted.iter().map(|r| r.input_tokens as u64).sum();
    let sum_output: u64 = counted.iter().map(|r| r.output_tokens as u64).sum();
    let sum_rounds: u64 = counted.iter().map(|r| r.rounds as u64).sum();
    let n = total.max(1) as f64;
    let (_center, half_width) = wilson_interval(passed, total);

    let mut wall_times: Vec<u128> = counted.iter().map(|r| r.wall_time_ms).collect();
    wall_times.sort_unstable();

    BackendSummary {
        backend: backend.to_string(),
        total,
        passed,
        harness_errors,
        avg_wall_time_ms: sum_wall_time as f64 / n,
        median_wall_time_ms: median(&wall_times),
        avg_input_tokens: sum_input as f64 / n,
        avg_output_tokens: sum_output as f64 / n,
        avg_rounds: sum_rounds as f64 / n,
        schema_validation_failures: counted.iter().map(|r| r.schema_validation_failures).sum(),
        tool_execution_failures: counted.iter().map(|r| r.tool_execution_failures).sum(),
        permission_denials: counted.iter().map(|r| r.permission_denials).sum(),
        pass_rate_interval_pp: half_width * 100.0,
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
            "[{status}] {:<24} {:<20}{rep_suffix} {:>6}ms  rounds={} tool_calls={}  schema_fail={}  exec_fail={}  denied={}  tokens_in={} tokens_out={}{cause_suffix}{error_suffix}",
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
    println!(
        "{:<24} {:>16} {:>8} {:>12} {:>10} {:>14} {:>16} {:>17} {:>14} {:>10} {:>12}",
        "backend",
        "pass_rate(±95%)",
        "avg_rounds",
        "avg_ms",
        "median_ms",
        "avg_tok_in",
        "avg_tok_out",
        "schema_fail",
        "exec_fail",
        "denied",
        "harness_err"
    );
    for backend in backend_order {
        let rows: Vec<&TaskResult> = results.iter().filter(|r| r.backend == backend).collect();
        let summary = summarize(backend, &rows);
        let pass_rate_cell = format!(
            "{}/{} (±{:.0}pp)",
            summary.passed, summary.total, summary.pass_rate_interval_pp
        );
        println!(
            "{:<24} {:>16} {:>8.1} {:>12.0} {:>10.0} {:>14.0} {:>16.0} {:>17} {:>14} {:>10} {:>12}",
            summary.backend,
            pass_rate_cell,
            summary.avg_rounds,
            summary.avg_wall_time_ms,
            summary.median_wall_time_ms,
            summary.avg_input_tokens,
            summary.avg_output_tokens,
            summary.schema_validation_failures,
            summary.tool_execution_failures,
            summary.permission_denials,
            summary.harness_errors,
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
        println!("\n== Comparación por skill ==");
        println!("{:<24} {:<20} {:>12}", "backend", "skill", "pass_rate");
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
                println!(
                    "{:<24} {:<20} {:>9}/{:<2}",
                    backend, skill, summary.passed, summary.total
                );
            }
        }
    }
}

/// Writes the raw per-task results as JSON, for downstream analysis
/// (e.g. tracking a backend's pass rate over time as `braze` itself
/// changes).
pub fn write_json(results: &[TaskResult], path: &Path) -> Result<(), BenchError> {
    let file = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(file, results)
        .map_err(|err| BenchError::Startup(format!("failed to write JSON report: {err}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            repetition: 0,
            converged: passed,
            run_error: None,
            failure_cause: None,
            tool_calls_total: 0,
            planned: false,
            schema_validation_failures: 0,
            tool_execution_failures: 0,
            permission_denials: 0,
            rounds: 0,
            expected_tool_called: None,
            expected_text_found: None,
            expected_files_found: None,
            input_tokens,
            output_tokens,
            wall_time_ms,
            passed,
        }
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
}
