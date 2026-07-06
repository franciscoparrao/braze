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
    /// backends Ollama en serio. `0` se rechaza explícitamente (bajo,
    /// docs/AUDITORIA-2026-07-v2.md, "--repetitions 0 acepta
    /// silenciosamente") — antes producía un sweep vacío con exit 0 y
    /// ningún aviso de que no se probó nada.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    repetitions: u32,
    /// Presupuesto de tiempo por intento de tarea, en segundos. Un modelo
    /// que no converge puede tardar mucho más que uno que sí (se ha
    /// observado >20 minutos en CPU-only antes de agotar el cap de
    /// iteraciones) — sin este límite, una sola tarea puede colgar todo
    /// el sweep en vez de registrarse como el fallo (diagnósticamente
    /// útil) que es.
    #[arg(long, default_value_t = runner::DEFAULT_TASK_TIMEOUT.as_secs())]
    task_timeout_secs: u64,
    /// Temperatura de sampling aplicada por igual a los tres backends
    /// (N-34, docs/AUDITORIA-2026-07-v2.md) — sin esto, comparar Ollama
    /// fijado a una temperatura baja contra Anthropic/OpenRouter en su
    /// default de proveedor (~1.0) compara regímenes de sampling
    /// distintos, no modelos distintos.
    #[arg(long, default_value_t = 0.2)]
    temperature: f32,
    /// Seed base para sampling reproducible en Ollama/OpenRouter (la API
    /// de Anthropic no tiene parámetro de seed). Cada repetición usa
    /// `seed + repetición` para no colapsar `--repetitions` en copias
    /// idénticas. Sin este flag (default), cada corrida usa el sampling
    /// no determinístico normal del proveedor.
    #[arg(long)]
    seed: Option<u64>,
    /// `options.top_p` para backends Ollama (ítem 7 del backlog: la
    /// familia Qwen recomienda temp 0.7 / top_p 0.8 / top_k 20 /
    /// repeat_penalty 1.05 — el default del bench es 0.2 sin estos
    /// knobs). Sin el flag, Ollama usa el valor del Modelfile del
    /// modelo. Ignorado por backends anthropic/openrouter.
    #[arg(long)]
    top_p: Option<f32>,
    /// `options.top_k` para backends Ollama — ver `--top-p`.
    #[arg(long)]
    top_k: Option<u32>,
    /// `options.repeat_penalty` para backends Ollama — ver `--top-p`.
    #[arg(long)]
    repeat_penalty: Option<f32>,
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
    // Same posture as `braze-cli`: binaries (and only binaries) install
    // the tracing subscriber, respecting `RUST_LOG`, writing to stderr
    // so it never interleaves with the report on stdout. Without this, a
    // sweep run with `RUST_LOG=info` silently showed nothing — e.g. the
    // engine's textual-rescue events, the signal that tells whether a
    // reliability lever actually fired during the bench (found while
    // validating the Qwen `<tool_call>` rescue, 2026-07-06).
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

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

    // Bajo (docs/AUDITORIA-2026-07-v2.md, "--output se valida recién tras
    // el sweep entero"): validated *before* the (possibly very slow)
    // sweep runs, not only when `report::write_json` finally tries to
    // create the file — a typo'd output directory used to lose the
    // whole sweep's results (still visible on stdout, but never written
    // out) instead of failing fast.
    if let Some(output_path) = &cli.output
        && let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        return Err(BenchError::Startup(format!(
            "el directorio de --output {parent:?} no existe"
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
        let probe_sampling = backend_spec::SamplingSpec {
            temperature: cli.temperature,
            seed: cli.seed,
            top_p: cli.top_p,
            top_k: cli.top_k,
            repeat_penalty: cli.repeat_penalty,
        };
        if let Err(err) = spec
            .build(&config, probe_sampling)
            .and(spec.build_planner(&config, probe_sampling))
        {
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
                // A fixed base seed offset by `repetition` keeps sweeps
                // reproducible across separate invocations while still
                // giving `--repetitions` genuine variance to measure
                // within one sweep — the same base seed on every
                // repetition would collapse them into identical copies.
                let sampling = backend_spec::SamplingSpec {
                    temperature: cli.temperature,
                    seed: cli.seed.map(|s| s.wrapping_add(u64::from(repetition))),
                    top_p: cli.top_p,
                    top_k: cli.top_k,
                    repeat_penalty: cli.repeat_penalty,
                };
                match runner::run_task(spec, &config, task, repetition, task_timeout, sampling)
                    .await
                {
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

        // Release every local model this backend row loaded (executor
        // and/or a local planner) before the next backend spec builds its
        // own — see the `no_ollama_stop` doc comment above for why this
        // isn't just tidiness.
        if !cli.no_ollama_stop {
            for model in spec.ollama_models(&config) {
                stop_ollama_model(&model).await;
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
