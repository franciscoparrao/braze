//! Renders a `Vec<TaskResult>` as a per-backend comparison table on
//! stdout, and optionally as JSON for later analysis.

use std::path::Path;

use crate::error::BenchError;
use crate::metrics::TaskResult;

/// One backend's aggregated row in the printed table.
struct BackendSummary {
    backend: String,
    total: u32,
    passed: u32,
    avg_wall_time_ms: f64,
    avg_input_tokens: f64,
    avg_output_tokens: f64,
    schema_validation_failures: u32,
    tool_execution_failures: u32,
    permission_denials: u32,
}

fn summarize(backend: &str, results: &[&TaskResult]) -> BackendSummary {
    let total = results.len() as u32;
    let passed = results.iter().filter(|r| r.passed).count() as u32;
    let sum_wall_time: u128 = results.iter().map(|r| r.wall_time_ms).sum();
    let sum_input: u64 = results.iter().map(|r| r.input_tokens as u64).sum();
    let sum_output: u64 = results.iter().map(|r| r.output_tokens as u64).sum();
    let n = total.max(1) as f64;

    BackendSummary {
        backend: backend.to_string(),
        total,
        passed,
        avg_wall_time_ms: sum_wall_time as f64 / n,
        avg_input_tokens: sum_input as f64 / n,
        avg_output_tokens: sum_output as f64 / n,
        schema_validation_failures: results.iter().map(|r| r.schema_validation_failures).sum(),
        tool_execution_failures: results.iter().map(|r| r.tool_execution_failures).sum(),
        permission_denials: results.iter().map(|r| r.permission_denials).sum(),
    }
}

/// Prints a per-task detail line followed by a per-backend summary table,
/// grouped and ordered by first appearance of each backend name in
/// `results` (i.e. the order `--backends` listed them in).
pub fn print_table(results: &[TaskResult]) {
    println!("\n== Resultados por tarea ==");
    for result in results {
        let status = if result.passed { "PASS" } else { "FAIL" };
        let error_suffix = result
            .run_error
            .as_ref()
            .map(|e| format!(" (error: {e})"))
            .unwrap_or_default();
        println!(
            "[{status}] {:<24} {:<20} {:>6}ms  tool_calls={}  schema_fail={}  exec_fail={}  denied={}  tokens_in={} tokens_out={}{error_suffix}",
            result.task_id,
            result.backend,
            result.wall_time_ms,
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
    println!(
        "{:<24} {:>10} {:>12} {:>14} {:>16} {:>17} {:>14} {:>10}",
        "backend",
        "pass_rate",
        "avg_ms",
        "avg_tok_in",
        "avg_tok_out",
        "schema_fail",
        "exec_fail",
        "denied"
    );
    for backend in backend_order {
        let rows: Vec<&TaskResult> = results.iter().filter(|r| r.backend == backend).collect();
        let summary = summarize(backend, &rows);
        println!(
            "{:<24} {:>9}/{:<2} {:>12.0} {:>14.0} {:>16.0} {:>17} {:>14} {:>10}",
            summary.backend,
            summary.passed,
            summary.total,
            summary.avg_wall_time_ms,
            summary.avg_input_tokens,
            summary.avg_output_tokens,
            summary.schema_validation_failures,
            summary.tool_execution_failures,
            summary.permission_denials,
        );
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
            converged: passed,
            run_error: None,
            tool_calls_total: 0,
            schema_validation_failures: 0,
            tool_execution_failures: 0,
            permission_denials: 0,
            expected_tool_called: None,
            expected_text_found: None,
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
}
