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

use clap::Parser;

use backend_spec::BackendSpec;
use error::BenchError;
use metrics::TaskResult;

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
    /// "anthropic", "anthropic:<modelo>", "ollama" o "ollama:<modelo>".
    #[arg(long, value_delimiter = ',', required = true)]
    backends: Vec<String>,
    /// Si se pasa, además de la tabla en stdout escribe los resultados
    /// crudos (uno por tarea) como JSON en esta ruta.
    #[arg(long)]
    output: Option<PathBuf>,
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
            println!("-> {display_name} :: {}", task.id);
            match runner::run_task(spec, &config, task).await {
                Ok(result) => results.push(result),
                Err(err) => {
                    eprintln!(
                        "braze-bench: fallo irrecuperable corriendo '{}' contra '{display_name}': {err}",
                        task.id
                    );
                }
            }
        }
    }

    report::print_table(&results);

    if let Some(output_path) = &cli.output {
        report::write_json(&results, output_path)?;
        println!("\nResultados JSON escritos en {}", output_path.display());
    }

    Ok(())
}
