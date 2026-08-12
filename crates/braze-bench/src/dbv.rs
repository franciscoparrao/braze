//! Dynamic Baseline Verification (`--baseline-ref`) — cross-invocación
//! (docs/dynamic-baseline-verification-design-2026-08-11.md).
//!
//! El caso intra-sweep ya lo cubre McNemar (`report::paired_comparisons`
//! ignora pares concordantes → un fallo compartido no es regresión). El
//! gap es cross-invocación: la doctrina parte el sweep en invocaciones
//! secuenciales por brazo, pero nada carga el JSON de una invocación
//! previa como baseline, y la metadata que detectaría drift ambiental
//! entre ellas se graba pero nunca se compara.
//!
//! Este módulo carga un `results.json` previo y (1) verifica el drift del
//! entorno comparando fingerprints de `RunMetadata`, (2) compara los
//! brazos de la corrida actual contra el baseline (primer brazo) del ref
//! con la misma maquinaria de McNemar. braze usa fingerprint, no re-run
//! (gemini-cli re-ejecuta): braze ya captura el fingerprint
//! determinístico, más barato y exacto.

use serde::Deserialize;

use crate::error::BenchError;
use crate::metadata::RunMetadata;
use crate::metrics::TaskResult;
use crate::report;

/// Un `results.json` de un sweep previo, deserializado — el espejo
/// *owned* de `report::SweepReport` (que serializa por referencia).
#[derive(Debug, Deserialize)]
pub struct BaselineRef {
    pub metadata: RunMetadata,
    pub results: Vec<TaskResult>,
}

/// Carga el JSON de un sweep previo desde `path`. `Err` con mensaje claro
/// si no existe o no parsea como un `results.json` de braze-bench (p.ej.
/// un JSON de otra herramienta, o uno viejo sin `metadata`).
pub fn load_baseline_ref(path: &std::path::Path) -> Result<BaselineRef, BenchError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        BenchError::Startup(format!(
            "no se pudo leer el baseline-ref '{}': {e}",
            path.display()
        ))
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        BenchError::Startup(format!(
            "'{}' no es un results.json de braze-bench (¿de otra herramienta, o un formato \
             viejo sin metadata?): {e}",
            path.display()
        ))
    })
}

/// Una divergencia de entorno entre el ref y la corrida actual.
pub struct DriftEntry {
    pub field: &'static str,
    pub reference: String,
    pub current: String,
}

/// Compara el `RunMetadata` del ref contra el actual campo por campo.
/// Vacío = entorno reproducible (la comparación cross-invocación es
/// válida). Cualquier entrada = drift: comparar peras con peras dejó de
/// ser cierto y la comparación se marca inválida.
///
/// Los campos elegidos son los que CAMBIAN el resultado sin cambiar la
/// pregunta: la suite (¿mismas tareas?), el commit del harness (¿mismo
/// braze?), los digests de modelo (¿se re-pulleó?), la versión del server
/// (la clase OOM 0.30.7→0.32.1), el `local_env` (capas GPU, KV type), y
/// el sampling+repeticiones (sin el mismo seed, el pareo por
/// (task_id, repetition) no comparte semilla derivada).
pub fn drift_report(reference: &RunMetadata, current: &RunMetadata) -> Vec<DriftEntry> {
    let mut drift = Vec::new();
    let mut check = |field, r: String, c: String| {
        if r != c {
            drift.push(DriftEntry {
                field,
                reference: r,
                current: c,
            });
        }
    };
    check(
        "suite_fingerprint",
        reference.suite_fingerprint.clone(),
        current.suite_fingerprint.clone(),
    );
    check(
        "braze_git_commit",
        format!("{:?}", reference.braze_git_commit),
        format!("{:?}", current.braze_git_commit),
    );
    check(
        "ollama_server_version",
        format!("{:?}", reference.ollama_server_version),
        format!("{:?}", current.ollama_server_version),
    );
    // Digests de modelo: comparar el conjunto {model → digest}. Un
    // re-pull cambia el digest de un modelo con el mismo nombre.
    let fmt_digests = |m: &RunMetadata| {
        let mut v: Vec<String> = m
            .ollama_model_digests
            .iter()
            .map(|d| format!("{}={:?}", d.model, d.digest))
            .collect();
        v.sort();
        v.join(", ")
    };
    check(
        "ollama_model_digests",
        fmt_digests(reference),
        fmt_digests(current),
    );
    check(
        "sampling",
        format!("{:?}", reference.sampling),
        format!("{:?}", current.sampling),
    );
    check(
        "repetitions",
        reference.repetitions.to_string(),
        current.repetitions.to_string(),
    );
    let fmt_env = |m: &RunMetadata| {
        m.local_env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    check("local_env", fmt_env(reference), fmt_env(current));
    // keep_alive por-request (2026-08-12): bajo presión de RAM cambia qué
    // modelos conviven residentes — la clase [Timeout]/OOM del incidente
    // Nitro 2026-08-10 — sin tocar la generación. Distinta política de
    // residencia entre ref y actual = los timeouts no son comparables.
    check(
        "ollama_keep_alive",
        format!("{:?}", reference.ollama_keep_alive),
        format!("{:?}", current.ollama_keep_alive),
    );
    drift
}

/// Corre el reporte DBV completo: imprime el drift check, y la comparación
/// cross-invocación de cada brazo de `current_results` contra el baseline
/// (primer brazo) del `reference`. Si hay drift, la comparación se imprime
/// igual pero con un banner INVÁLIDA — el operador ve los números y sabe
/// que no son de fiar.
pub fn run_dbv_report(
    reference: &BaselineRef,
    current_results: &[TaskResult],
    current_metadata: &RunMetadata,
) {
    println!("\n== Dynamic Baseline Verification (--baseline-ref) ==");

    // El baseline es el PRIMER brazo del ref (mismo criterio que el pareo
    // intra-sweep). `backend_specs` preserva el orden de `--backends`.
    let Some(baseline_name) = reference.metadata.backend_specs.first() else {
        println!("  el baseline-ref no registró brazos (backend_specs vacío) — nada que comparar.");
        return;
    };
    println!(
        "  baseline ref: {} filas, brazo '{}'",
        reference.results.len(),
        baseline_name
    );

    // 1. Drift check.
    let drift = drift_report(&reference.metadata, current_metadata);
    let invalid = !drift.is_empty();
    if invalid {
        println!("  DRIFT DE ENTORNO detectado ({} campos):", drift.len());
        for d in &drift {
            println!("    {}: ref {} → actual {}", d.field, d.reference, d.current);
        }
        println!(
            "  => la comparación cross-invocación de abajo se marca INVÁLIDA: el entorno cambió \
             entre el baseline y esta corrida, así que la diferencia observada mezcla el efecto \
             del brazo con el drift. Re-correr el baseline en el entorno actual, o alinear el \
             entorno."
        );
    } else {
        println!("  entorno reproducible: fingerprints coinciden (comparación válida).");
    }

    // 2. Pareo cross-invocación: cada brazo actual vs el baseline del ref.
    let control_outcomes = report::outcomes_map(&reference.results, baseline_name);
    if control_outcomes.is_empty() {
        println!(
            "  el brazo baseline '{baseline_name}' no tiene filas contables en el ref — \
             sin celdas de control, no hay comparación."
        );
        return;
    }
    // Brazos de la corrida actual, en orden de aparición, sin el baseline
    // si por casualidad se re-corrió acá.
    let mut seen = std::collections::HashSet::new();
    let mut current_arms: Vec<String> = Vec::new();
    for r in current_results {
        if r.backend != *baseline_name && seen.insert(r.backend.clone()) {
            current_arms.push(r.backend.clone());
        }
    }
    if current_arms.is_empty() {
        println!("  la corrida actual no tiene brazos distintos del baseline — nada que comparar.");
        return;
    }
    let arm_outcomes: Vec<(String, report::OutcomeCells)> = current_arms
        .iter()
        .map(|arm| (arm.clone(), report::outcomes_map(current_results, arm)))
        .collect();
    let comparisons = report::compare_against_control(&control_outcomes, &arm_outcomes);

    let banner = if invalid { "  [INVÁLIDA — drift de entorno]" } else { "" };
    println!(
        "\n  Comparación cross-invocación vs baseline '{}' (McNemar exacto + Holm){banner}",
        baseline_name
    );
    for c in &comparisons {
        report::print_comparison_row(c);
    }
    println!(
        "  (* p_holm < 0.05; 'solo-brazo' > 'solo-control' favorece al brazo. El baseline viene \
         de un sweep previo — su validez depende del drift check de arriba.)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend_spec::SamplingSpec;
    use crate::metadata::OllamaModelDigest;

    fn meta() -> RunMetadata {
        RunMetadata {
            sampling: SamplingSpec {
                temperature: 0.2,
                seed: Some(42),
                top_p: None,
                top_k: None,
                repeat_penalty: None,
            },
            repetitions: 5,
            task_timeout_secs: 180,
            turn_wall_clock_secs: None,
            round_wall_clock_secs: None,
            suite_path: "suites/default.toml".to_string(),
            suite_fingerprint: "abc123".to_string(),
            braze_git_commit: Some("deadbeef".to_string()),
            ollama_model_digests: vec![OllamaModelDigest {
                model: "qwen2.5:3b".to_string(),
                digest: Some("d1".to_string()),
            }],
            ollama_server_version: Some("0.32.1".to_string()),
            backend_specs: vec!["ollama:qwen2.5:3b".to_string()],
            local_env: std::collections::BTreeMap::new(),
            ollama_keep_alive: Some("2m".to_string()),
        }
    }

    #[test]
    fn a_different_keep_alive_policy_is_drift() {
        // Distinta residencia = distinta presión de RAM = timeouts no
        // comparables (incidente Nitro 2026-08-10).
        let reference = meta();
        let mut current = meta();
        current.ollama_keep_alive = None;
        let drift = drift_report(&reference, &current);
        assert!(drift.iter().any(|d| d.field == "ollama_keep_alive"));
    }

    #[test]
    fn identical_metadata_reports_no_drift() {
        assert!(drift_report(&meta(), &meta()).is_empty());
    }

    #[test]
    fn a_repulled_model_is_drift() {
        let reference = meta();
        let mut current = meta();
        current.ollama_model_digests[0].digest = Some("d2".to_string());
        let drift = drift_report(&reference, &current);
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].field, "ollama_model_digests");
    }

    #[test]
    fn a_server_upgrade_is_drift() {
        // La clase OOM 0.30.7→0.32.1.
        let reference = meta();
        let mut current = meta();
        current.ollama_server_version = Some("0.30.7".to_string());
        let drift = drift_report(&reference, &current);
        assert!(drift.iter().any(|d| d.field == "ollama_server_version"));
    }

    #[test]
    fn a_different_seed_is_drift() {
        // Sin el mismo seed el pareo (task_id, repetition) no comparte
        // semilla derivada — la comparación no sería válida.
        let reference = meta();
        let mut current = meta();
        current.sampling.seed = Some(7);
        let drift = drift_report(&reference, &current);
        assert!(drift.iter().any(|d| d.field == "sampling"));
    }

    #[test]
    fn a_changed_suite_and_commit_are_both_drift() {
        let reference = meta();
        let mut current = meta();
        current.suite_fingerprint = "xyz789".to_string();
        current.braze_git_commit = Some("cafe".to_string());
        let drift = drift_report(&reference, &current);
        assert_eq!(drift.len(), 2);
    }

    /// Round-trip: un `results.json` REAL escrito por un sweep previo
    /// deserializa a `BaselineRef` sin pérdida (el Deserialize agregado a
    /// RunMetadata/TaskResult/etc. matchea lo que write_json serializa).
    #[test]
    fn a_real_sweep_json_deserializes() {
        // El sweep de edit-fence del 10-ago, versionado en el repo.
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/sweep-json-tax-edit-fence-2026-08-10.json"
        ));
        if !path.exists() {
            // El JSON es un artefacto versionado; si no está (checkout
            // parcial), el test no aplica en vez de fallar.
            return;
        }
        let reference = load_baseline_ref(path).expect("un results.json real debe cargar");
        assert!(!reference.results.is_empty());
        assert!(!reference.metadata.backend_specs.is_empty());
        // El baseline (primer brazo) tiene celdas contables.
        let baseline = &reference.metadata.backend_specs[0];
        let cells = report::outcomes_map(&reference.results, baseline);
        assert!(!cells.is_empty(), "el primer brazo debe tener filas");
    }

    /// La maquinaria de comparación que DBV reusa cuenta los discordantes
    /// como McNemar espera: el control es el baseline del ref, el brazo el
    /// de la corrida actual — el pareo cross-invocación es el mismo cálculo
    /// que el intra-sweep, solo cambia de dónde viene el control.
    #[test]
    fn cross_invocation_pairing_counts_discordants() {
        use std::collections::HashMap;
        // Baseline (del ref): t1 pasa, t2 pasa.
        let control: HashMap<(String, u32), bool> =
            [(("t1".to_string(), 0), true), (("t2".to_string(), 0), true)].into();
        // Brazo actual: t1 pasa, t2 falla → una regresión solo-control.
        let arm: HashMap<(String, u32), bool> =
            [(("t1".to_string(), 0), true), (("t2".to_string(), 0), false)].into();
        let cmp = report::compare_against_control(&control, &[("B".to_string(), arm)]);
        assert_eq!(cmp.len(), 1);
        assert_eq!(cmp[0].control_only, 1, "regresión: control pasó t2, brazo no");
        assert_eq!(cmp[0].arm_only, 0);
        assert_eq!(cmp[0].n_pairs, 2);
    }
}
