//! `braze-bench`: runs a TOML task suite through the real
//! `braze_engine::Engine` against several `ModelBackend`s and prints a
//! side-by-side comparison — see PLAN.md's "Harness comparativo de
//! backends" entry for why this exists and its safety posture.

mod backend_spec;
mod error;
mod metrics;
mod report;
mod runner;
mod sandbox;
mod task;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use backend_spec::BackendSpec;
use error::BenchError;
use metrics::{TaskResult, harness_error_result};

/// `braze-bench <suite.toml> --backends <spec,spec,...> [--output <path.json>]`
#[derive(Parser, Debug)]
#[command(
    name = "braze-bench",
    about = "Compara el comportamiento agéntico de varios ModelBackend sobre la misma suite de tareas"
)]
struct Cli {
    /// Ruta al archivo TOML con la suite de tareas.
    suite: PathBuf,
    /// Backends a comparar, separados por coma. Cada uno es
    /// "anthropic", "anthropic:<modelo>", "ollama", "ollama:<modelo>",
    /// "openrouter" o "openrouter:<modelo>".
    #[arg(long, value_delimiter = ',', required = true)]
    backends: Vec<String>,
    /// Si se pasa, además de la tabla en stdout escribe los resultados
    /// crudos (uno por tarea) como JSON en esta ruta.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Cuántas veces correr cada (tarea, backend). Con modelos locales
    /// chicos la varianza corrida-a-corrida es alta — un pass_rate con
    /// repetitions=1 es ruido, no señal. Recomendado >=5 para comparar
    /// backends Ollama en serio.
    #[arg(long, default_value_t = 1)]
    repetitions: u32,
    /// Presupuesto de tiempo por intento de tarea, en segundos. Un modelo
    /// que no converge puede tardar mucho más que uno que sí (se ha
    /// observado >20 minutos en CPU-only antes de agotar el cap de
    /// iteraciones) — sin este límite, una sola tarea puede colgar todo
    /// el sweep en vez de registrarse como el fallo (diagnósticamente
    /// útil) que es.
    #[arg(long, default_value_t = runner::DEFAULT_TASK_TIMEOUT.as_secs())]
    task_timeout_secs: u64,
    /// No ejecutar 'ollama stop <modelo>' al terminar con un backend Ollama.
    /// Por defecto el sweep sí lo hace: en esta máquina (38GB RAM, ~1.4GB
    /// libres bajo carga) un modelo grande que queda residente mientras
    /// carga el siguiente produce contención de memoria que se manifiesta
    /// como [Timeout] — no como fallo de razonamiento — inflando o
    /// desinflando pass rates sin relación con la capacidad real del
    /// modelo. Ver docs/AUDITORIA-2026-07.md.
    #[arg(long)]
    no_ollama_stop: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("braze-bench: error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), BenchError> {
    let cli = Cli::parse();
    let config = braze_config::Config::load()?;
    let tasks = task::load_suite(&cli.suite)?;

    if tasks.is_empty() {
        return Err(BenchError::Startup(format!(
            "la suite {} no tiene tareas",
            cli.suite.display()
        )));
    }

    let specs: Vec<(String, BackendSpec)> = cli
        .backends
        .iter()
        .map(|raw| BackendSpec::parse(raw).map(|spec| (raw.clone(), spec)))
        .collect::<Result<_, _>>()?;

    let mut results: Vec<TaskResult> = Vec::new();
    let task_timeout = Duration::from_secs(cli.task_timeout_secs);
    if cli.repetitions > 1 {
        println!(
            "Corriendo {} repetición(es) por (tarea, backend) — timeout {}s por intento.",
            cli.repetitions, cli.task_timeout_secs
        );
    }

    // Sequential on purpose: several large local Ollama models sharing
    // one GPU/CPU would just thrash each other under concurrency, and a
    // sequential run keeps stdout progress readable task by task.
    for (raw_spec, spec) in &specs {
        let display_name = spec.display_name(&config);
        // A backend that can't even build (Ollama down, no API key, ...)
        // is skipped, not fatal — the rest of the comparison still runs.
        if let Err(err) = spec.build(&config) {
            eprintln!("braze-bench: omitiendo backend '{raw_spec}' ({display_name}): {err}");
            continue;
        }

        for task in &tasks {
            for repetition in 0..cli.repetitions {
                if cli.repetitions > 1 {
                    println!(
                        "-> {display_name} :: {} (rep {}/{})",
                        task.id,
                        repetition + 1,
                        cli.repetitions
                    );
                } else {
                    println!("-> {display_name} :: {}", task.id);
                }
                match runner::run_task(spec, &config, task, repetition, task_timeout).await {
                    Ok(result) => results.push(result),
                    Err(err) => {
                        // A harness-level failure (sandbox setup, reading
                        // back the session log, ...) — not attributable to
                        // the model at all. Still recorded as a row (with
                        // its own failure cause) instead of silently
                        // vanishing from the totals, which previously let
                        // pass-rate denominators drift between backends
                        // with no visible explanation. See
                        // docs/AUDITORIA-2026-07.md hallazgo F5.
                        eprintln!(
                            "braze-bench: fallo irrecuperable corriendo '{}' contra '{display_name}': {err}",
                            task.id
                        );
                        results.push(harness_error_result(&display_name, task, repetition, &err));
                    }
                }
            }
        }

        // Release the model this backend just loaded before the next
        // backend spec builds its own — see the `no_ollama_stop` doc
        // comment above for why this isn't just tidiness.
        if !cli.no_ollama_stop
            && let Some(model) = spec.ollama_model(&config)
        {
            stop_ollama_model(&model).await;
        }
    }

    report::print_table(&results);

    if let Some(output_path) = &cli.output {
        report::write_json(&results, output_path)?;
        println!("\nResultados JSON escritos en {}", output_path.display());
    }

    Ok(())
}

/// Unloads `model` from the local Ollama daemon (`ollama stop <model>`) so
/// the next backend in the sweep starts from a clean memory baseline
/// instead of contending with whatever this one left resident. Best
/// effort: `ollama` missing or the daemon being down is logged, not fatal
/// — the sweep already treats a hung/slow backend as a timed-out task, not
/// a harness failure.
async fn stop_ollama_model(model: &str) {
    match tokio::process::Command::new("ollama")
        .args(["stop", model])
        .output()
        .await
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => eprintln!(
            "braze-bench: 'ollama stop {model}' salió con {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(err) => {
            eprintln!("braze-bench: no se pudo ejecutar 'ollama stop {model}': {err}");
        }
    }
}
