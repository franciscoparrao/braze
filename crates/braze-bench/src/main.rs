//! `braze-bench`: runs a TOML task suite through the real
//! `braze_engine::Engine` against several `ModelBackend`s and prints a
//! side-by-side comparison — see PLAN.md's "Harness comparativo de
//! backends" entry for why this exists and its safety posture.

mod backend_spec;
mod bare_lead_baseline;
mod error;
mod external;
mod memory;
mod metadata;
mod metrics;
mod noise;
mod preserve;
mod report;
mod synthetic;
mod runner;
mod sandbox;
mod task;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use backend_spec::BackendSpec;
use error::BenchError;
use external::ExternalHarness;
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
    /// "openrouter" o "openrouter:<modelo>". Sufijos componibles antes
    /// de ablations: "+plan:<spec>" adjunta un planner (PLAN.md §
    /// "Split planificador/ejecutor"); "+lead:<spec>" adjunta un modelo
    /// líder reactivo estilo `EscalatingBackend`;
    /// "+ablate:<clave>[=<valor>];..." (nota: separador ';', no ',' —
    /// la coma ya delimita entradas de --backends) override de palancas
    /// del harness para esta fila del sweep — claves: no-rescue,
    /// no-post-edit-check, strict-edit, best-of-n=N, tactical-window=N,
    /// tactical-threshold=N, full-observations=N (E1,
    /// docs/AUDITORIA-2026-07-v3.md). No requerido si `--external` se pasa
    /// solo (se valida en runtime que al menos uno de los dos esté
    /// presente, ver `run()`).
    #[arg(long, value_delimiter = ',')]
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
    /// Corre un baseline de harness externo (bypassa `braze_engine::Engine`
    /// por completo, EMSE review Issue 1 —
    /// `docs/external-harness-baseline-design.md`) además de los
    /// `--backends` de arriba, sobre la misma suite. Único adapter hoy:
    /// `bare-lead:<spec>`, donde `<spec>` usa la misma gramática que
    /// `--backends` pero DEBE llevar un sufijo `+lead:` (un loop
    /// lead+executor desde cero, ni rescate textual, ni compactación, ni
    /// tool deferral, ni post-edit check — ver
    /// `bare_lead_baseline.rs`). Ej.:
    /// `--external "bare-lead:ollama:llama3.2:1b+lead:ollama:gemma4:e4b"`.
    #[arg(long, value_delimiter = ',')]
    external: Vec<String>,
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
    if cli.backends.is_empty() && cli.external.is_empty() {
        return Err(BenchError::Startup(
            "se requiere al menos uno de --backends o --external".to_string(),
        ));
    }
    let config = braze_config::Config::load()?;
    let tasks = task::load_suite(&cli.suite)?;

    // Opt-in transcript preservation (Issue 4,
    // docs/emse-review-2026-07-13-checklist.md) — was an uncommitted local
    // patch (docs/sweep-search-tools-ab-n15-2026-07-12.md:116), now a real
    // env var. Off by default: `None` here means every `run_task` call
    // below behaves exactly as it did before this existed.
    let preserve_root = preserve::keep_sessions_enabled().then(|| {
        let root = PathBuf::from(preserve::DEFAULT_PRESERVE_ROOT);
        eprintln!(
            "braze-bench: BRAZE_BENCH_KEEP_SESSIONS activo — sandbox y transcripciones de \
             cada run se preservan en {}/ (no se borran por defecto — limpiar a mano cuando \
             ya no se necesiten)",
            root.display()
        );
        root
    });

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

    // H-13 (docs/AUDITORIA-2026-07-v5.md): `--top-p`/`--top-k`/
    // `--repeat-penalty` only reach Ollama builders — a mixed sweep
    // that sets them compares different sampling regimes in up to 3
    // unflagged dimensions. Warn once per affected spec up front (not
    // per task — that would repeat it `tasks × repetitions` times), so
    // the imbalance is visible in the sweep log next to the results it
    // taints.
    let ollama_only_knobs: Vec<&str> = [
        cli.top_p.map(|_| "--top-p"),
        cli.top_k.map(|_| "--top-k"),
        cli.repeat_penalty.map(|_| "--repeat-penalty"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !ollama_only_knobs.is_empty() {
        for (_, spec) in &specs {
            let ignoring = spec.non_ollama_halves();
            if !ignoring.is_empty() {
                eprintln!(
                    "braze-bench: advertencia: {} solo aplican a backends Ollama — '{}' los \
                     ignora en: {}. Ese brazo corre con el sampling default de su proveedor \
                     en esas dimensiones.",
                    ollama_only_knobs.join("/"),
                    spec.display_name(&config),
                    ignoring.join(", ")
                );
            }
        }
    }

    // Mismo patrón H-13 para los brazos del A/B de constrained decoding
    // (docs/constrained-decoding-ab-design.md): `prompt-tools`/
    // `constrained-tools` solo llegan al builder de Ollama — en un
    // executor Anthropic/OpenRouter la fila correría en modo nativo
    // mientras su display name promete el envelope, y el engine además
    // parsearía envelopes que el modelo nunca fue instruido a emitir.
    for (_, spec) in &specs {
        if spec.ablation().prompt_tools_active() && !spec.executor_is_ollama() {
            eprintln!(
                "braze-bench: advertencia: '+ablate:prompt-tools'/'constrained-tools' solo \
                 aplican a executors Ollama — '{}' corre con tool-calling nativo y su fila \
                 NO mide la modalidad prompt que su nombre declara.",
                spec.display_name(&config)
            );
        }
    }

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
            .build_agent_model(&config, probe_sampling)
            .and_then(|_| spec.build_planner(&config, probe_sampling).map(|_| ()))
        {
            eprintln!("braze-bench: omitiendo backend '{raw_spec}' ({display_name}): {err}");
            continue;
        }

        // J-6 (docs/AUDITORIA-2026-07-v7.md): load every local model this
        // arm uses BEFORE its first task, so no arm's first task pays the
        // cold-load time. Without this the first arm of a sweep started
        // cold (inflated wall-time / spurious timeouts, always on the
        // suite's first task) while later arms under `--no-ollama-stop`
        // inherited the previous arm's resident model — a systematic
        // between-arm bias. Best effort: on failure the first real
        // request pays the load, which is exactly the old behavior.
        for model in spec.ollama_models(&config) {
            if let Err(err) =
                braze_model::warm_up_ollama_model(&config.ollama_base_url, &model).await
            {
                eprintln!("braze-bench: warm-up de '{model}' falló (se continúa igual): {err}");
            }
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
                // v8 § 6.15: con `+ablate:ttc=N` (N>1) la unidad de
                // medición pasa a ser "N rollouts + selección" — una
                // fila por repetición igual que siempre, con el costo
                // agregado de los N adentro.
                let ttc_rollouts = spec.ablation().ttc_rollouts.unwrap_or(1);
                let run = if ttc_rollouts > 1 {
                    runner::run_task_ttc(
                        spec,
                        &config,
                        task,
                        repetition,
                        task_timeout,
                        sampling,
                        preserve_root.as_deref(),
                        ttc_rollouts,
                    )
                    .await
                } else {
                    runner::run_task(
                        spec,
                        &config,
                        task,
                        repetition,
                        task_timeout,
                        sampling,
                        preserve_root.as_deref(),
                    )
                    .await
                };
                match run {
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
                stop_ollama_model(&config.ollama_base_url, &model).await;
            }
        }
    }

    // External harness baseline(s) — EMSE review Issue 1
    // (docs/external-harness-baseline-design.md). Same sequential-on-purpose
    // reasoning as the `--backends` loop above; runs after it so a sweep
    // combining both never contends over the same Ollama models at once.
    for raw_external in &cli.external {
        let Some(rest) = raw_external.strip_prefix("bare-lead:") else {
            eprintln!(
                "braze-bench: omitiendo --external '{raw_external}': solo se soporta el prefijo \
                 'bare-lead:' hoy (docs/external-harness-baseline-design.md)"
            );
            continue;
        };
        let spec = match backend_spec::BackendSpec::parse(rest) {
            Ok(spec) => spec,
            Err(err) => {
                eprintln!("braze-bench: omitiendo --external '{raw_external}': {err}");
                continue;
            }
        };
        let sampling = backend_spec::SamplingSpec {
            temperature: cli.temperature,
            seed: cli.seed,
            top_p: cli.top_p,
            top_k: cli.top_k,
            repeat_penalty: cli.repeat_penalty,
        };
        let executor = match spec.build(&config, sampling) {
            Ok(executor) => executor,
            Err(err) => {
                eprintln!("braze-bench: omitiendo --external '{raw_external}': {err}");
                continue;
            }
        };
        let lead = match spec.build_lead(&config, sampling) {
            Ok(Some(lead)) => lead,
            Ok(None) => {
                eprintln!(
                    "braze-bench: omitiendo --external '{raw_external}': 'bare-lead:' requiere \
                     un sufijo '+lead:' en el spec"
                );
                continue;
            }
            Err(err) => {
                eprintln!("braze-bench: omitiendo --external '{raw_external}': {err}");
                continue;
            }
        };
        let harness =
            bare_lead_baseline::BareLeadExecutor::new(lead, executor, spec.display_name(&config));

        for model in spec.ollama_models(&config) {
            if let Err(err) =
                braze_model::warm_up_ollama_model(&config.ollama_base_url, &model).await
            {
                eprintln!("braze-bench: warm-up de '{model}' falló (se continúa igual): {err}");
            }
        }

        for task in &tasks {
            for repetition in 0..cli.repetitions {
                if cli.repetitions > 1 {
                    println!(
                        "-> {} :: {} (rep {}/{})",
                        harness.name(),
                        task.id,
                        repetition + 1,
                        cli.repetitions
                    );
                } else {
                    println!("-> {} :: {}", harness.name(), task.id);
                }
                match external::run_external_task(&harness, task, repetition, task_timeout).await {
                    Ok(result) => results.push(result),
                    Err(err) => {
                        eprintln!(
                            "braze-bench: fallo irrecuperable corriendo '{}' contra '{}': {err}",
                            task.id,
                            harness.name()
                        );
                        results.push(harness_error_result(
                            &harness.name(),
                            task,
                            repetition,
                            &err,
                        ));
                    }
                }
            }
        }

        if !cli.no_ollama_stop {
            for model in spec.ollama_models(&config) {
                stop_ollama_model(&config.ollama_base_url, &model).await;
            }
        }
    }

    report::print_table(&results);

    if let Some(output_path) = &cli.output {
        // E6 (docs/AUDITORIA-2026-07-v3.md): only worth computing when
        // actually writing a JSON file — the suite re-read, the git
        // subprocess, and (if any Ollama backend is in the sweep) the
        // digest lookups are all wasted work for a stdout-only run.
        let suite_bytes = std::fs::read(&cli.suite)?;
        let ollama_models: Vec<String> = {
            let mut models: Vec<String> = specs
                .iter()
                .flat_map(|(_, spec)| spec.ollama_models(&config))
                .collect();
            models.sort();
            models.dedup();
            models
        };
        let metadata = metadata::RunMetadata {
            sampling: backend_spec::SamplingSpec {
                temperature: cli.temperature,
                seed: cli.seed,
                top_p: cli.top_p,
                top_k: cli.top_k,
                repeat_penalty: cli.repeat_penalty,
            },
            repetitions: cli.repetitions,
            task_timeout_secs: cli.task_timeout_secs,
            suite_path: cli.suite.display().to_string(),
            suite_fingerprint: metadata::fingerprint_bytes(&suite_bytes),
            braze_git_commit: metadata::current_git_commit().await,
            ollama_model_digests: metadata::collect_ollama_model_digests(
                &config.ollama_base_url,
                &ollama_models,
            )
            .await,
            // Serving-layer identity (EMSE blind b2, Issue 3): only
            // looked up when some backend actually talks to Ollama —
            // best-effort, same posture as the digests above.
            ollama_server_version: if ollama_models.is_empty() {
                None
            } else {
                braze_model::ollama_server_version(&config.ollama_base_url)
                    .await
                    .ok()
                    .flatten()
            },
            // H-17: the resolved display name carries the full spec —
            // executor, +plan:/+lead: halves, and the +ablate: suffix —
            // so the run itself records which ablations were active.
            backend_specs: specs
                .iter()
                .map(|(_, spec)| spec.display_name(&config))
                .collect(),
        };

        report::write_json(&metadata, &results, output_path)?;
        println!("\nResultados JSON escritos en {}", output_path.display());
    }

    Ok(())
}

/// Unloads `model` from the Ollama daemon at `base_url` (`ollama stop
/// <model>`) so the next backend in the sweep starts from a clean memory
/// baseline instead of contending with whatever this one left resident.
/// `OLLAMA_HOST` must be set explicitly (AUDITORIA-2026-07-v8 K-11): the
/// `ollama` CLI defaults to localhost, so without it a sweep against a
/// remote node (Nitro via `BRAZE_OLLAMA_BASE_URL`) silently no-ops the
/// stop and the memory hygiene this function promises never happens on
/// the machine that actually needs it. Best effort: `ollama` missing or
/// the daemon being down is logged, not fatal — the sweep already treats
/// a hung/slow backend as a timed-out task, not a harness failure.
async fn stop_ollama_model(base_url: &str, model: &str) {
    match tokio::process::Command::new("ollama")
        .args(["stop", model])
        .env("OLLAMA_HOST", base_url)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for a real bug caught while wiring E1's ablation
    /// suffix (docs/AUDITORIA-2026-07-v3.md): `--backends` uses
    /// `value_delimiter = ','` to split multiple entries, so an
    /// `+ablate:` suffix using `,` as ITS OWN internal separator would
    /// get silently split apart by clap before `BackendSpec::parse` ever
    /// saw it whole. The suffix grammar uses `;` specifically to avoid
    /// this collision — this test parses through the real `Cli` (not
    /// just `BackendSpec::parse` directly) so a future change to either
    /// the delimiter or the suffix grammar that reintroduces the
    /// collision fails here.
    #[test]
    fn an_ablate_suffix_with_multiple_keys_survives_the_comma_backends_delimiter() {
        let cli = Cli::parse_from([
            "braze-bench",
            "suite.toml",
            "--backends",
            "ollama:qwen2.5:3b+ablate:no-rescue;strict-edit,anthropic:claude-x",
        ]);
        assert_eq!(
            cli.backends,
            vec![
                "ollama:qwen2.5:3b+ablate:no-rescue;strict-edit".to_string(),
                "anthropic:claude-x".to_string(),
            ]
        );

        let spec = BackendSpec::parse(&cli.backends[0]).expect("must parse");
        let ablation = spec.ablation();
        assert!(ablation.disable_textual_rescue);
        assert!(ablation.edit_strict_mode);
    }
}
