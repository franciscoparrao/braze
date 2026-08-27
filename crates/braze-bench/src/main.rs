//! `braze-bench`: runs a TOML task suite through the real
//! `braze_engine::Engine` against several `ModelBackend`s and prints a
//! side-by-side comparison — see PLAN.md's "Harness comparativo de
//! backends" entry for why this exists and its safety posture.

mod backend_spec;
mod bare_lead_baseline;
mod dbv;
mod error;
mod external;
mod memory;
mod metadata;
mod metrics;
mod noise;
mod preserve;
mod report;
mod runner;
mod sandbox;
mod sequential;
mod sft;
mod synthetic;
mod task;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use backend_spec::BackendSpec;
use error::BenchError;
use external::ExternalHarness;
use metrics::{TaskResult, harness_error_result};

/// L-11 (v9): cuántos fallos CONSECUTIVOS de nivel harness/backend, cada
/// uno por debajo de [`ARM_FAIL_FAST_INSTANT_MS`], abortan el brazo. Tres:
/// uno puede ser un blip transitorio, dos una mala racha; tres seguidos e
/// instantáneos es la firma medida del brazo estructuralmente roto (57
/// fallos de carga a ~185-200ms, casos gemma3:1b 2026-07-04 y binarios
/// desincronizados de Nitro 2026-07-21).
const ARM_FAIL_FAST_THRESHOLD: u32 = 3;

/// Umbral de instantaneidad del fail-fast: un fallo REAL del modelo
/// (razonamiento, timeout, presupuesto) gasta segundos de generación
/// antes de fallar; un fallo de infraestructura (modelo ausente, breaker
/// abierto) muere en milisegundos. 2s separa las dos poblaciones con
/// margen en ambas direcciones.
const ARM_FAIL_FAST_INSTANT_MS: u32 = 2_000;

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
    /// Default: 180 (`runner::DEFAULT_TASK_TIMEOUT`), o 3× el
    /// `--turn-wall-clock-secs` si ese se pasa y este no.
    #[arg(long)]
    task_timeout_secs: Option<u64>,
    /// Presupuesto de wall-clock POR TURNO, en segundos — el corte
    /// experimental de la línea round-economics
    /// (`docs/hypothesis-2026-07-28-round-economics.md`). El engine para en
    /// el borde de la ronda cuando el turno ya gastó este tiempo, y la
    /// fila queda como [WallClock], con sus rondas y tokens COMPLETOS.
    ///
    /// No confundir con `--task-timeout-secs`, que es el backstop de
    /// infraestructura: aquel mata la ronda en vuelo y censura su `Usage`
    /// (J-21/J-10), así que una fila [Timeout] no es comparable entre
    /// brazos. Por eso `run()` exige que el backstop sea estrictamente
    /// mayor que este presupuesto — si muerde primero, el experimento
    /// midió el backstop.
    ///
    /// Sin el flag (default) no hay presupuesto de tiempo y el turno corta
    /// por rondas/tokens como siempre.
    #[arg(long)]
    turn_wall_clock_secs: Option<u64>,
    /// Deadline de wall-clock POR RONDA, en segundos, aplicado a nivel de
    /// streaming (`Engine::with_max_round_wall_clock`). Acota la ronda
    /// desbocada que el corte en borde de ronda de
    /// `--turn-wall-clock-secs` no puede: el piloto de round-economics
    /// midió filas de 600 s con `rounds` 0-1 — una sola ronda de
    /// generación CPU sin cota — que solo el backstop paraba, censurando
    /// la contabilidad (`docs/round-economics-pilot-costo-2026-08-08.md`
    /// § 4.4). Al vencerse, la fila queda como [RoundWallClock]: las
    /// rondas COMPLETADAS conservan rondas/tokens, la desbocada se
    /// descarta. `run()` exige que el backstop quede por encima, igual
    /// que con el presupuesto de turno.
    #[arg(long)]
    round_wall_clock_secs: Option<u64>,
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
    /// `keep_alive` por-request para backends Ollama (e.g. "2m", "0",
    /// "-1"): cuánto queda residente el modelo tras cada request,
    /// ganándole a la env `OLLAMA_KEEP_ALIVE` del server — la política de
    /// residencia del sweep viaja con el request en vez de depender de
    /// cómo quedó configurado el servicio remoto (con el servicio en
    /// `-1`, apilar dos modelos grandes OOM-kileó Ollama en Nitro a mitad
    /// de sweep, 2026-08-10). Gana sobre `ollama_keep_alive` de
    /// config/`BRAZE_OLLAMA_KEEP_ALIVE`. Sin flag ni config, manda la del
    /// server. Ignorado por backends anthropic/openrouter y por `local`
    /// (in-process, sin server que mantenga residencia).
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    keep_alive: Option<String>,
    /// No ejecutar 'ollama stop <modelo>' al terminar con un backend Ollama.
    /// Por defecto el sweep sí lo hace: en esta máquina (38GB RAM, ~1.4GB
    /// libres bajo carga) un modelo grande que queda residente mientras
    /// carga el siguiente produce contención de memoria que se manifiesta
    /// como [Timeout] — no como fallo de razonamiento — inflando o
    /// desinflando pass rates sin relación con la capacidad real del
    /// modelo. Ver docs/AUDITORIA-2026-07.md.
    #[arg(long)]
    no_ollama_stop: bool,
    /// Corte secuencial anytime-valid (`sequential.rs`): monitorea los pares
    /// discordantes de McNemar entre el PRIMER brazo y cada brazo siguiente,
    /// y corta el sweep cuando el criterio ya está decidido — sin inflar α,
    /// por la desigualdad de Ville. El valor es el umbral PRE-REGISTRADO del
    /// experimento, en celdas (p.ej. `--sequential-stop 3` para un criterio
    /// de "±3 tareas"); ese umbral define `p1`, y no hay default a propósito:
    /// un `p1` genérico confunde "sub-umbral" con "efecto cero" (medido en
    /// docs/retrodiccion-evalues-2026-08-06.md). Ahorro mediano histórico:
    /// 62%, concentrado en los A/B que SÍ tienen efecto — un nulo corre
    /// completo, que es lo correcto.
    #[arg(long)]
    sequential_stop: Option<usize>,
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
    /// Dynamic Baseline Verification
    /// (docs/dynamic-baseline-verification-design-2026-08-11.md): ruta al
    /// `results.json` de un sweep PREVIO cuyo primer brazo es el baseline.
    /// Al terminar, se compara la metadata del ref contra la de esta
    /// corrida (drift de entorno: suite, commit del harness, digests de
    /// modelo, versión del server Ollama, `local_env`, sampling) y se
    /// parean los brazos de ESTA corrida contra el baseline del ref con
    /// McNemar exacto + Holm — el flujo "3 invocaciones por brazo" hecho
    /// first-class. Si el entorno derivó, la comparación se imprime igual
    /// pero marcada INVÁLIDA. No aborta (over-inform, no bloquear).
    #[arg(long)]
    baseline_ref: Option<PathBuf>,
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

    // Subcomando `export-sft` (línea experto-por-motor, `sft.rs`),
    // despachado a mano ANTES de `Cli::parse()`: el CLI principal usa la
    // suite como posicional (`braze-bench <suite.toml> ...`) en todos los
    // repro commands documentados, y un subcomando clap real cambiaría
    // esa gramática. El token `export-sft` no colisiona con ningún path
    // de suite razonable, y `--help` del subcomando funciona igual.
    if std::env::args().nth(1).as_deref() == Some("export-sft") {
        let cli = sft::ExportCli::parse_from(std::env::args().skip(1));
        return match sft::run(cli) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("braze-bench: error: {err}");
                ExitCode::FAILURE
            }
        };
    }

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
    let mut config = braze_config::Config::load()?;
    // `--keep-alive` gana sobre `ollama_keep_alive` de archivo/env — la
    // misma precedencia flag-sobre-config de todo el bench. Se aplica
    // sobre `config` (no como parámetro aparte) para que TODO camino que
    // construya un backend Ollama —executor, mitades planner/lead— lo
    // herede sin enhebrado extra.
    if let Some(keep_alive) = &cli.keep_alive {
        config.ollama_keep_alive = Some(keep_alive.clone());
    }
    let tasks = task::load_suite(&cli.suite)?;

    // La identidad del sweep se captura al INICIO, no al escribir el
    // JSON (lección del re-run Bloque 2, 2026-07-19): en un sweep de
    // horas, HEAD y el archivo de suite pueden cambiar MIENTRAS corre
    // (commits de integración aterrizando en paralelo en el mismo
    // árbol) — la metadata debe describir lo que corrió, no el estado
    // del repo al momento de terminar. Aquel sweep quedó registrado
    // con un commit varios pasos posterior al del binario. Aquel caveat
    // —"el binario pudo compilarse en un commit anterior al HEAD del
    // arranque"— ya está cerrado: el commit se embebe en tiempo de build
    // (`build.rs`) y `resolve_git_commit` lo prefiere sobre el HEAD del
    // cwd, así que la metadata describe el ejecutable y no el directorio
    // desde el que se lo lanzó. Eso arregla además el caso Nitro, donde
    // `~/braze` es una copia sin `.git` y el campo salía `null`.
    let suite_fingerprint = metadata::fingerprint_bytes(&std::fs::read(&cli.suite)?);
    let braze_git_commit = metadata::resolve_git_commit().await;

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

    // Mismo patrón H-13 para `--keep-alive`, con un corte distinto al de
    // arriba: `keep_alive` es residencia en un *server* Ollama, así que
    // `local` (in-process) también lo ignora aunque honre el sampling.
    if cli.keep_alive.is_some() {
        for (_, spec) in &specs {
            let ignoring = spec.keep_alive_ignoring_halves();
            if !ignoring.is_empty() {
                eprintln!(
                    "braze-bench: advertencia: --keep-alive solo aplica a backends Ollama — \
                     '{}' lo ignora en: {}. Esa mitad corre con la política de residencia \
                     de su proveedor/proceso.",
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
    // round-economics: el backstop de infraestructura tiene que quedar
    // POR ENCIMA del presupuesto experimental, si no muerde primero y
    // todas las filas del brazo salen censuradas ([Timeout] pierde el
    // `Usage` de la ronda en vuelo). Cuando el usuario no fija el
    // backstop, se deriva de 3× el presupuesto — el corte del engine es
    // en el borde de la ronda, así que el turno puede sobrepasar el
    // presupuesto por hasta una ronda entera y el backstop tiene que
    // dejar lugar para eso.
    let task_timeout_secs = match (cli.task_timeout_secs, cli.turn_wall_clock_secs) {
        (Some(explicit), Some(budget)) if explicit <= budget => {
            return Err(BenchError::Startup(format!(
                "--task-timeout-secs ({explicit}) debe ser MAYOR que --turn-wall-clock-secs \
                 ({budget}): el backstop de infraestructura mataría el turno antes de que el \
                 presupuesto experimental pueda cortar, y toda fila cortada así queda con \
                 rondas/tokens censurados y no comparable entre brazos"
            )));
        }
        (Some(explicit), _) => explicit,
        (None, Some(budget)) => (budget * 3).max(runner::DEFAULT_TASK_TIMEOUT.as_secs()),
        (None, None) => runner::DEFAULT_TASK_TIMEOUT.as_secs(),
    };
    let task_timeout = Duration::from_secs(task_timeout_secs);
    // El presupuesto viaja por `Config` (es donde `runner::run_task` lee
    // los knobs del engine); el flag gana sobre lo que traiga el archivo
    // de config o el entorno, como el resto de los flags del bench.
    if let Some(budget) = cli.turn_wall_clock_secs {
        config.max_turn_wall_clock_secs = Some(budget);
        println!(
            "Presupuesto de wall-clock por turno: {budget}s (corte en borde de ronda). \
             Backstop de infraestructura: {task_timeout_secs}s."
        );
    }
    // El deadline por ronda tiene la misma relación con el backstop que
    // el presupuesto de turno: si el backstop muerde primero, la fila
    // sale [Timeout] con la contabilidad censurada y el deadline nunca
    // llega a actuar.
    if let Some(deadline) = cli.round_wall_clock_secs {
        if task_timeout_secs <= deadline {
            return Err(BenchError::Startup(format!(
                "--task-timeout-secs ({task_timeout_secs}) debe ser MAYOR que \
                 --round-wall-clock-secs ({deadline}): el backstop de infraestructura mataría \
                 la ronda antes de que el deadline de streaming pueda cortarla con la \
                 contabilidad de las rondas previas intacta"
            )));
        }
        config.max_round_wall_clock_secs = Some(deadline);
        println!(
            "Deadline de wall-clock por ronda: {deadline}s (corte a nivel de streaming, \
             acota la ronda desbocada)."
        );
    }
    if cli.repetitions > 1 {
        println!(
            "Corriendo {} repetición(es) por (tarea, backend) — timeout {}s por intento.",
            cli.repetitions, task_timeout_secs
        );
    }

    // Sequential on purpose: several large local Ollama models sharing
    // one GPU/CPU would just thrash each other under concurrency, and a
    // sequential run keeps stdout progress readable task by task.
    // `--sequential-stop`: un monitor por brazo-vs-primer-brazo. El primer
    // brazo es la referencia (baseline), así que no tiene monitor propio.
    // Las celdas del primer brazo se guardan para poder parear.
    let mut baseline_cells: std::collections::HashMap<(String, u32), bool> =
        std::collections::HashMap::new();
    let mut sequential_notes: Vec<String> = Vec::new();

    for (arm_index, (raw_spec, spec)) in specs.iter().enumerate() {
        let display_name = spec.display_name(&config);
        let mut monitor = cli.sequential_stop.map(|delta| {
            sequential::SequentialStop::for_threshold(delta, tasks.len() * cli.repetitions as usize)
        });
        let mut arm_cut_short = false;
        // L-11 (v9): fail-fast de brazo. El caso real (dos veces, por
        // binarios desincronizados en Nitro, 21-jul): la carga del modelo
        // falla INSTANTÁNEO en cada tarea y el brazo entero se quema en 57
        // fallos silenciosos de ~200ms que el reporte después descuenta
        // como HarnessError. Tres filas consecutivas de nivel
        // harness/backend por debajo del umbral de instantaneidad no son
        // flakiness: son un brazo estructuralmente roto (modelo ausente,
        // binario viejo, breaker abierto), y seguir corriéndolo no
        // produce ni una celda interpretable. Las filas ya corridas se
        // conservan; las restantes NO se inventan — misma doctrina que el
        // corte de --sequential-stop.
        let mut consecutive_instant_failures = 0u32;
        let mut arm_failed_fast = false;
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
                // round-economics: el presupuesto es de la TAREA. Con TTC
                // se reparte entre los rollouts (`run_task_ttc`), no se
                // le da entero a cada uno.
                let wall_clock_budget = config
                    .max_turn_wall_clock_secs
                    .map(std::time::Duration::from_secs);
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
                        wall_clock_budget,
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
                        wall_clock_budget,
                    )
                    .await
                };
                match run {
                    Ok(result) => {
                        let key = (result.task_id.clone(), result.repetition);
                        if arm_index == 0 {
                            baseline_cells.insert(key, result.passed);
                        } else if let (Some(m), Some(&base_passed)) =
                            (monitor.as_mut(), baseline_cells.get(&key))
                            && m.observe(base_passed, result.passed).is_some()
                        {
                            let note = format!("[{display_name}] {}", m.summary());
                            println!("\n{note}");
                            sequential_notes.push(note);
                            arm_cut_short = true;
                        }
                        results.push(result);
                    }
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
                // L-11: contabilidad del fail-fast sobre la fila recién
                // registrada (ambas ramas del match empujan una).
                let instant_infra_failure = results.last().is_some_and(|r| {
                    matches!(
                        r.failure_cause,
                        Some(
                            metrics::FailureCause::HarnessError
                                | metrics::FailureCause::ModelBackendError
                        )
                    ) && r.wall_time_ms < u128::from(ARM_FAIL_FAST_INSTANT_MS)
                });
                if instant_infra_failure {
                    consecutive_instant_failures += 1;
                    if consecutive_instant_failures >= ARM_FAIL_FAST_THRESHOLD {
                        eprintln!(
                            "\nbraze-bench: BRAZO '{display_name}' ABORTADO: \
                             {consecutive_instant_failures} fallos consecutivos de nivel \
                             harness/backend en <{ARM_FAIL_FAST_INSTANT_MS}ms cada uno — el \
                             brazo está estructuralmente roto (¿modelo ausente, binario \
                             desincronizado, breaker abierto?), no flaky. Las filas corridas \
                             se conservan; las restantes no se corren."
                        );
                        arm_failed_fast = true;
                        break;
                    }
                } else {
                    consecutive_instant_failures = 0;
                }
            }
            if arm_failed_fast {
                break;
            }
            if arm_cut_short {
                // El criterio pre-registrado ya está decidido para este
                // brazo: seguir corriendo no cambia el veredicto y sí
                // cuesta horas. Las celdas faltantes NO se inventan —
                // simplemente no existen, y el JSON lo refleja.
                println!(
                    "-> {display_name}: brazo cortado por --sequential-stop tras {} filas",
                    results.iter().filter(|r| r.backend == display_name).count()
                );
                break;
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

    // round-economics: en un sweep con presupuesto experimental, una fila
    // [Timeout] significa que el backstop de infraestructura mordió antes
    // que el corte del engine — o sea que esa fila perdió las rondas y
    // tokens de su ronda en vuelo (J-21/J-10) y no es comparable entre
    // brazos. La guardia de `--task-timeout-secs` de arriba hace esto
    // improbable, no imposible: una sola ronda puede durar más que todo
    // el margen si el modelo es lento y el presupuesto chico. El aviso
    // aplica igual si el corte del engine es el presupuesto de turno o
    // el deadline de ronda — cualquiera de los dos debería morder antes
    // que el backstop.
    if config.max_turn_wall_clock_secs.is_some() || config.max_round_wall_clock_secs.is_some() {
        let censored = results
            .iter()
            .filter(|r| r.failure_cause == Some(metrics::FailureCause::Timeout))
            .count();
        if censored > 0 {
            println!(
                "\n[round-economics] ATENCIÓN: {censored} fila(s) cortadas por el backstop de \
                 infraestructura ({task_timeout_secs}s) y no por los cortes del engine \
                 (turno: {}, ronda: {}). Esas filas tienen rondas/tokens censurados — subir \
                 --task-timeout-secs o bajar el corte del engine y re-correr antes de \
                 interpretar el contraste.",
                config
                    .max_turn_wall_clock_secs
                    .map_or("—".to_string(), |s| format!("{s}s")),
                config
                    .max_round_wall_clock_secs
                    .map_or("—".to_string(), |s| format!("{s}s")),
            );
        }
    }

    // Chequeo de salud de banco (técnica #2, docs/irt-suites-2026-08-07.md):
    // un ítem cuyo acierto no correlaciona con el puntaje total del
    // respondente no está midiendo capacidad. Solo informa — nunca falla el
    // sweep — pero se imprime DESPUÉS de la tabla para que nadie interprete
    // los números sin verlo.
    {
        let cells: Vec<(String, String, bool)> = results
            .iter()
            .map(|r| {
                (
                    format!("{}#{}", r.backend, r.repetition),
                    r.task_id.clone(),
                    r.passed,
                )
            })
            .collect();
        match sequential::low_discrimination_items(&cells) {
            Some(flagged) if !flagged.is_empty() => {
                println!(
                    "\n[salud del banco] {} ítem(s) con discriminación bajo {:.2} — \
                     REVISAR antes de interpretar este sweep:",
                    flagged.len(),
                    sequential::LOW_DISCRIMINATION
                );
                for (item, r) in &flagged {
                    println!("   {item:38} r_pbis={r:+.3}");
                }
                println!(
                    "   Un ítem así puede estar midiendo infraestructura y no capacidad \
                     (caso read_file_basic / transporte Ollama 0.30.7, julio 2026)."
                );
            }
            Some(_) => println!("\n[salud del banco] sin ítems de discriminación anómala."),
            None => {}
        }
    }

    for note in &sequential_notes {
        println!("[secuencial] {note}");
    }

    // La metadata se construye si vamos a escribir JSON (`--output`) O si
    // DBV la necesita para el drift check (`--baseline-ref`). Antes solo
    // el primer caso la armaba; el segundo la comparte.
    let metadata = if cli.output.is_some() || cli.baseline_ref.is_some() {
        // E6 (docs/AUDITORIA-2026-07-v3.md): the digest lookups only
        // run when actually writing a JSON file. The suite fingerprint
        // and git commit, in contrast, were captured at sweep START —
        // see the comment at the top of `run()`.
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
            task_timeout_secs,
            turn_wall_clock_secs: config.max_turn_wall_clock_secs,
            round_wall_clock_secs: config.max_round_wall_clock_secs,
            suite_path: cli.suite.display().to_string(),
            suite_fingerprint,
            braze_git_commit,
            // Contraparte de `ollama_server_version` para el camino
            // in-process: con llama.cpp linkeado en el binario no hay
            // servidor al que preguntarle la versión, así que viene
            // embebida del build. Misma postura condicional que el
            // `ollama_server_version` de abajo — solo cuando ALGÚN
            // backend del sweep corre de verdad por el LocalBackend, no
            // por el mero hecho de que el binario traiga el feature: lo
            // segundo describe el binario y no la corrida, y haría
            // driftear dos sweeps servidos idénticos.
            engine_version: specs
                .iter()
                .any(|(_, spec)| spec.uses_local_backend())
                .then(braze_model::local_engine_version)
                .flatten(),
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
            // v9 L-1: the env-only deployment tier travels with the
            // sweep — see `RunMetadata::local_env`.
            local_env: metadata::collect_local_env(std::env::vars()),
            // H-17: the resolved display name carries the full spec —
            // executor, +plan:/+lead: halves, and the +ablate: suffix —
            // so the run itself records which ablations were active.
            backend_specs: specs
                .iter()
                .map(|(_, spec)| spec.display_name(&config))
                .collect(),
            // El valor EFECTIVO (flag ya aplicado sobre config arriba),
            // no el flag crudo — es lo que viajó en cada request.
            ollama_keep_alive: config.ollama_keep_alive.clone(),
            grading: Some(metadata::GRADING_FUNCTIONAL_DUAL.to_string()),
        };
        Some(metadata)
    } else {
        None
    };

    if let (Some(output_path), Some(metadata)) = (&cli.output, &metadata) {
        report::write_json(metadata, &results, output_path)?;
        println!("\nResultados JSON escritos en {}", output_path.display());
    }

    // Dynamic Baseline Verification: carga el sweep previo, verifica el
    // drift del entorno y parea los brazos actuales contra su baseline
    // (docs/dynamic-baseline-verification-design-2026-08-11.md).
    if let Some(ref_path) = &cli.baseline_ref {
        let reference = dbv::load_baseline_ref(ref_path)?;
        // `metadata` es Some por construcción cuando baseline_ref está.
        if let Some(metadata) = &metadata {
            dbv::run_dbv_report(&reference, &results, metadata);
        }
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
