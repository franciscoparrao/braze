//! Exportador rollout→JSONL para SFT — línea experto-por-motor
//! (piloto pizzeria 2026-08-13/14; formalizado 2026-08-14).
//!
//! Convierte las trayectorias PRESERVADAS de un sweep
//! (`BRAZE_BENCH_KEEP_SESSIONS=1`, ver `preserve.rs`) en un JSONL de
//! fine-tuning: una línea por corrida, con la conversación en el formato
//! messages estilo OpenAI function-calling (`user` / `assistant` con
//! `tool_calls` / `tool`) que aceptan los pipelines de SFT habituales
//! (LLaMA-Factory, axolotl, unsloth), más metadata de procedencia
//! trazable a la fila exacta del `results.json` que la produjo.
//!
//! El filtro por defecto es la métrica OFICIAL del banco (`passed`,
//! funcional — decisión 2026-08-12): el set de entrenamiento son
//! demostraciones del experto que LOGRARON la tarea; `--include-failed`
//! existe para análisis, no para entrenar.
//!
//! Qué NO exporta (deliberado, v1): el system prompt y los schemas de
//! tools no viven en el rollout log (son request-scoped, reconstruidos
//! por el engine en cada request) — el pipeline de entrenamiento los
//! agrega como constantes de la versión del harness (mismo criterio que
//! el chat template: es del lado del entrenamiento, no del dato). Los
//! eventos que el modelo vio como mensajes pero cuyo framing vive en
//! `braze-engine::history` (`PlanCreated`, `VerificationFailed`,
//! `HarnessNote`) NO se reconstruyen aquí — duplicar esos strings sería
//! drift esperando ocurrir; se cuentan en `dropped_events` y la línea
//! queda marcada `lossy: true` para que el pipeline decida (las
//! trayectorias pizzeria no contienen ninguno: sin planner, sin gate,
//! sin notas de presupuesto).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use braze_events::AgentEvent;
use serde::Serialize;

use crate::error::BenchError;
use crate::metrics::TaskResult;
use crate::preserve;

/// `braze-bench export-sft ...` — despachado a mano desde `main` cuando
/// el primer argumento es exactamente `export-sft` (los repro commands
/// documentados de sweeps usan la forma `braze-bench <suite.toml> ...`;
/// un subcomando clap real cambiaría esa gramática para todos).
#[derive(Debug, clap::Parser)]
#[command(
    name = "braze-bench export-sft",
    about = "Exporta trayectorias preservadas de un sweep como JSONL de SFT (línea experto-por-motor)"
)]
pub struct ExportCli {
    /// Ruta al `results.json` del sweep (el mismo que escribió `--output`).
    #[arg(long)]
    pub results: PathBuf,
    /// Raíz de sesiones preservadas del MISMO sweep
    /// (`BRAZE_BENCH_KEEP_SESSIONS=1` durante la corrida).
    #[arg(long, default_value = preserve::DEFAULT_PRESERVE_ROOT)]
    pub sessions: PathBuf,
    /// Archivo JSONL de salida (una línea por trayectoria exportada).
    #[arg(long)]
    pub output: PathBuf,
    /// Exporta también las corridas que NO pasaron (métrica funcional).
    /// Para análisis — el set de SFT por defecto son solo demostraciones
    /// exitosas.
    #[arg(long)]
    pub include_failed: bool,
    /// Exporta solo las filas de estos backends (display name EXACTO,
    /// e.g. "ollama:gpt-oss:20b" — repetible). Sin el flag se exportan
    /// todos, lo que en un sweep multi-modelo mezcla expertos y débiles
    /// en el mismo set: casi siempre quieres filtrar al experto.
    #[arg(long = "backend")]
    pub backends: Vec<String>,
}

/// Un mensaje del JSONL exportado — formato messages estilo OpenAI
/// function-calling. `content` se serializa siempre (null en un turno
/// assistant que solo llama tools, como espera el formato); los campos
/// de tools solo cuando aplican.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SftMessage {
    pub role: &'static str,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<SftToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SftToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: SftFunction,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SftFunction {
    pub name: String,
    /// JSON codificado como STRING (convención del formato — los
    /// pipelines lo re-parsean al aplicar el chat template).
    pub arguments: String,
}

/// Resultado de convertir un event log en messages: la conversación más
/// la contabilidad de lo que NO se pudo representar — el exportador
/// nunca descarta en silencio (misma cultura no-silent-loss del
/// compactador).
#[derive(Debug, Default)]
pub struct Conversion {
    pub messages: Vec<SftMessage>,
    /// Eventos que el modelo VIO como mensajes pero cuyo framing vive en
    /// `braze-engine::history` y no se reconstruye aquí (ver module
    /// doc): su presencia marca la trayectoria como `lossy`.
    pub lossy_events: BTreeMap<&'static str, u32>,
    /// Eventos audit-only que nunca fueron mensajes (Usage,
    /// ToolCallStarted, permisos, compactación, palancas H-3, Unknown…)
    /// — contados para el summary, sin efecto en la conversación.
    pub audit_events: BTreeMap<&'static str, u32>,
}

impl Conversion {
    pub fn is_lossy(&self) -> bool {
        !self.lossy_events.is_empty()
    }
}

/// Convierte el event log lineal de una sesión en la secuencia de
/// messages. Agrupación por ronda: una racha de eventos del lado
/// assistant (`AssistantText` + `AssistantToolCall`s, con eventos
/// audit-only invisibles en el medio) forma UN mensaje assistant —
/// texto unido + tool_calls — exactamente como el turno que el backend
/// emitió; cada `ToolCallCompleted` cierra la racha y sale como mensaje
/// `tool`.
pub fn events_to_messages(events: &[AgentEvent]) -> Conversion {
    let mut out = Conversion::default();
    let mut pending_texts: Vec<String> = Vec::new();
    let mut pending_calls: Vec<SftToolCall> = Vec::new();

    fn flush(
        messages: &mut Vec<SftMessage>,
        texts: &mut Vec<String>,
        calls: &mut Vec<SftToolCall>,
    ) {
        if texts.is_empty() && calls.is_empty() {
            return;
        }
        let content = if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n"))
        };
        let tool_calls = if calls.is_empty() {
            None
        } else {
            Some(std::mem::take(calls))
        };
        texts.clear();
        messages.push(SftMessage {
            role: "assistant",
            content,
            tool_calls,
            tool_call_id: None,
        });
    }

    for event in events {
        match event {
            AgentEvent::UserMessage { text } => {
                flush(&mut out.messages, &mut pending_texts, &mut pending_calls);
                out.messages.push(SftMessage {
                    role: "user",
                    content: Some(text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            AgentEvent::AssistantText { text } => {
                pending_texts.push(text.clone());
            }
            AgentEvent::AssistantToolCall {
                id,
                name,
                arguments,
            } => {
                pending_calls.push(SftToolCall {
                    id: id.clone(),
                    kind: "function",
                    function: SftFunction {
                        name: name.clone(),
                        arguments: arguments.to_string(),
                    },
                });
            }
            AgentEvent::ToolCallCompleted { id, result } => {
                flush(&mut out.messages, &mut pending_texts, &mut pending_calls);
                out.messages.push(SftMessage {
                    role: "tool",
                    content: Some(result.content.clone()),
                    tool_calls: None,
                    tool_call_id: Some(id.clone()),
                });
            }
            // Mensajes reales para el modelo cuyo framing no se
            // reconstruye aquí — ver module doc. Marcan la línea lossy.
            AgentEvent::PlanCreated { .. } => {
                *out.lossy_events.entry("plan_created").or_insert(0) += 1;
            }
            AgentEvent::VerificationFailed { .. } => {
                *out.lossy_events.entry("verification_failed").or_insert(0) += 1;
            }
            AgentEvent::HarnessNote { .. } => {
                *out.lossy_events.entry("harness_note").or_insert(0) += 1;
            }
            // Audit-only: nunca fueron mensajes; contados y nada más.
            other => {
                let name = match other {
                    AgentEvent::ToolCallStarted { .. } => "tool_call_started",
                    AgentEvent::CompactionOccurred { .. } => "compaction_occurred",
                    AgentEvent::PermissionRequested { .. } => "permission_requested",
                    AgentEvent::PermissionDecided { .. } => "permission_decided",
                    AgentEvent::Usage { .. } => "usage",
                    AgentEvent::TextualRescueApplied { .. } => "textual_rescue_applied",
                    AgentEvent::EditFenceApplied { .. } => "edit_fence_applied",
                    AgentEvent::EscalationToLead { .. } => "escalation_to_lead",
                    AgentEvent::SummaryFallbackAttempted => "summary_fallback_attempted",
                    AgentEvent::HookErrored { .. } => "hook_errored",
                    AgentEvent::SkillLoaded { .. } => "skill_loaded",
                    AgentEvent::SkillLoadSkipped { .. } => "skill_load_skipped",
                    AgentEvent::AgentsMdLoaded { .. } => "agents_md_loaded",
                    AgentEvent::TaskCompleted { .. } => "task_completed",
                    AgentEvent::ExplorationDelegated { .. } => "exploration_delegated",
                    AgentEvent::EditorDelegated { .. } => "editor_delegated",
                    AgentEvent::SessionConstraintDeclared { .. } => "session_constraint_declared",
                    AgentEvent::Unknown => "unknown",
                    // Las variantes conversacionales ya matchearon arriba.
                    _ => "other",
                };
                *out.audit_events.entry(name).or_insert(0) += 1;
            }
        }
    }
    flush(&mut out.messages, &mut pending_texts, &mut pending_calls);
    out
}

/// Metadata de procedencia por línea del JSONL — suficiente para volver
/// de un ejemplo de entrenamiento a la fila exacta del sweep que lo
/// produjo (y para filtrar río abajo sin re-abrir el results.json).
#[derive(Debug, Serialize)]
struct LineMetadata<'a> {
    task_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill: Option<&'a str>,
    backend: &'a str,
    repetition: u32,
    passed: bool,
    passed_strict: bool,
    rounds: u32,
    input_tokens: u32,
    output_tokens: u32,
    compaction_count: u32,
    /// La trayectoria contiene eventos que el modelo vio y este export
    /// no reconstruye (ver module doc) — el pipeline decide si la usa.
    lossy: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    lossy_events: BTreeMap<&'static str, u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suite_fingerprint: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    braze_git_commit: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct ExportLine<'a> {
    messages: Vec<SftMessage>,
    metadata: LineMetadata<'a>,
}

/// Contabilidad del export completo, impresa al final — sin caps
/// silenciosos: cada fila del results que NO terminó en el JSONL está en
/// alguno de estos contadores.
#[derive(Debug, Default, PartialEq)]
pub struct ExportSummary {
    pub exported: u32,
    pub skipped_not_passed: u32,
    pub skipped_backend_filter: u32,
    pub missing_sessions: u32,
    pub lossy_trajectories: u32,
}

/// Núcleo testeable del export: filas ya cargadas → JSONL en `writer`.
/// `sessions_root` es la raíz preservada (`preserve::preserved_run_dir`
/// resuelve el subdirectorio de cada fila).
pub fn export_rows(
    rows: &[TaskResult],
    sessions_root: &std::path::Path,
    include_failed: bool,
    backend_filter: &[String],
    suite_fingerprint: Option<&str>,
    braze_git_commit: Option<&str>,
    writer: &mut dyn std::io::Write,
) -> Result<ExportSummary, BenchError> {
    let mut summary = ExportSummary::default();

    for row in rows {
        if !backend_filter.is_empty() && !backend_filter.iter().any(|b| b == &row.backend) {
            summary.skipped_backend_filter += 1;
            continue;
        }
        if !row.passed && !include_failed {
            summary.skipped_not_passed += 1;
            continue;
        }

        let session_dir =
            preserve::preserved_run_dir(sessions_root, &row.backend, &row.task_id, row.repetition)
                .join("session");
        let candidates = load_session_candidates(&session_dir)?;
        let Some(events) = select_matching_session(candidates, row)? else {
            summary.missing_sessions += 1;
            eprintln!(
                "braze-bench export-sft: sin sesión preservada CONSISTENTE con la fila \
                 '{}' :: '{}' rep {} (¿el sweep corrió con BRAZE_BENCH_KEEP_SESSIONS=1? \
                 ¿o las sesiones en el directorio son de una corrida anterior de la misma \
                 suite?): {}",
                row.task_id,
                row.backend,
                row.repetition,
                session_dir.display()
            );
            continue;
        };

        let conversion = events_to_messages(&events);
        if conversion.is_lossy() {
            summary.lossy_trajectories += 1;
        }
        let line = ExportLine {
            messages: conversion.messages,
            metadata: LineMetadata {
                task_id: &row.task_id,
                skill: row.skill.as_deref(),
                backend: &row.backend,
                repetition: row.repetition,
                passed: row.passed,
                passed_strict: row.passed_strict,
                rounds: row.rounds,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                compaction_count: row.compaction_count,
                lossy: !conversion.lossy_events.is_empty(),
                lossy_events: conversion.lossy_events,
                suite_fingerprint,
                braze_git_commit,
            },
        };
        let json = serde_json::to_string(&line)
            .map_err(|e| BenchError::Startup(format!("serializando línea de export: {e}")))?;
        writeln!(writer, "{json}")
            .map_err(|e| BenchError::Startup(format!("escribiendo JSONL: {e}")))?;
        summary.exported += 1;
    }
    Ok(summary)
}

/// Lee TODOS los event logs `<session-id>.jsonl` presentes en el
/// `session/` preservado de una corrida. Más de uno es un estado real,
/// no un error de layout: `preserve::copy_dir_recursive` copia DENTRO
/// del mismo `rep<N>/` en cada sweep, así que re-correr la misma suite
/// con preservación activa ACUMULA sesiones de corridas distintas bajo
/// el mismo rep (encontrado en vivo con las 3 iteraciones del piloto
/// pizzeria, 2026-08-14). La selección de cuál corresponde a la fila
/// del results es de [`select_matching_session`], por contenido — nunca
/// por mtime ni por orden de directorio.
fn load_session_candidates(
    session_dir: &std::path::Path,
) -> Result<Vec<(PathBuf, Vec<AgentEvent>)>, BenchError> {
    if !session_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut logs: Vec<PathBuf> = std::fs::read_dir(session_dir)
        .map_err(|e| BenchError::Startup(format!("leyendo '{}': {e}", session_dir.display())))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    logs.sort(); // orden determinista, independiente del filesystem
    let mut out = Vec::with_capacity(logs.len());
    for path in logs {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| BenchError::Startup(format!("leyendo '{}': {e}", path.display())))?;
        let mut events = Vec::new();
        for (i, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let event: AgentEvent = serde_json::from_str(line).map_err(|e| {
                BenchError::Startup(format!(
                    "'{}' línea {}: no parsea como AgentEvent: {e}",
                    path.display(),
                    i + 1
                ))
            })?;
            events.push(event);
        }
        out.push((path, events));
    }
    Ok(out)
}

/// La clave de correlación contenido↔fila: la secuencia exacta de tool
/// names emitidos (de los `AssistantToolCall`, en orden de log — el
/// espejo de cómo el runner llena `TaskResult::tool_call_names`), el
/// número de rondas (conteo de `Usage` = `TaskResult::rounds`), y las
/// sumas de tokens de esos mismos `Usage`
/// (= `TaskResult::{input,output}_tokens`). Los tokens son el
/// discriminador fino: dos corridas de la misma tarea pueden emitir la
/// misma secuencia de calls en las mismas rondas (visto en vivo entre
/// las iteraciones v2/v3 del piloto pizzeria) pero prácticamente nunca
/// con sumas de tokens idénticas — y si TODO coincide y el contenido
/// además es idéntico, cualquiera de las dos sesiones sirve (ver
/// [`select_matching_session`]).
fn correlation_key(events: &[AgentEvent]) -> (Vec<&str>, u32, u32, u32) {
    let names: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AssistantToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let mut rounds = 0u32;
    let mut input = 0u32;
    let mut output = 0u32;
    for event in events {
        if let AgentEvent::Usage {
            input_tokens,
            output_tokens,
            ..
        } = event
        {
            rounds += 1;
            input += input_tokens;
            output += output_tokens;
        }
    }
    (names, rounds, input, output)
}

/// Elige, entre los logs acumulados de un `rep<N>/session/`, el que
/// corresponde a ESTA fila del results — por contenido: debe calzar la
/// secuencia de tool names y el conteo de rondas de la fila. También
/// valida el caso de UN solo log (podría ser de una corrida anterior de
/// la misma suite, con el results de la nueva — una sesión stale
/// exportada en silencio sería exactamente la clase de mezcla que la
/// procedencia por línea existe para impedir).
///
/// `Ok(None)` = ningún candidato consistente (contado como faltante por
/// el caller). Varios candidatos consistentes: si sus event logs son
/// IDÉNTICOS (re-corridas greedy con el mismo seed producen lo mismo),
/// cualquiera sirve y se usa el primero; si difieren en contenido,
/// error explícito — elegir uno sería inventar el dato.
fn select_matching_session(
    candidates: Vec<(PathBuf, Vec<AgentEvent>)>,
    row: &TaskResult,
) -> Result<Option<Vec<AgentEvent>>, BenchError> {
    let expected_names: Vec<&str> = row.tool_call_names.iter().map(String::as_str).collect();
    let expected = (
        expected_names,
        row.rounds,
        row.input_tokens,
        row.output_tokens,
    );
    let mut matching: Vec<(PathBuf, Vec<AgentEvent>)> = candidates
        .into_iter()
        .filter(|(_, events)| correlation_key(events) == expected)
        .collect();
    match matching.len() {
        0 => Ok(None),
        1 => Ok(Some(matching.remove(0).1)),
        _ => {
            let all_identical = {
                let first_canonical = canonical_log(&matching[0].1);
                matching
                    .iter()
                    .skip(1)
                    .all(|(_, events)| canonical_log(events) == first_canonical)
            };
            if all_identical {
                Ok(Some(matching.remove(0).1))
            } else {
                let paths: Vec<String> = matching
                    .iter()
                    .map(|(p, _)| p.display().to_string())
                    .collect();
                Err(BenchError::Startup(format!(
                    "{} sesiones preservadas distintas calzan con la fila '{}' :: '{}' \
                     rep {} (tool names + rondas idénticos, contenido diferente) — no se \
                     puede saber cuál produjo la fila; limpiar el directorio preservado y \
                     re-correr el sweep con preservación: {}",
                    paths.len(),
                    row.task_id,
                    row.backend,
                    row.repetition,
                    paths.join(", ")
                )))
            }
        }
    }
}

/// Forma canónica de un event log para comparar identidad de
/// trayectoria entre sesiones acumuladas: los ids sintéticos de tool
/// calls llevan un timestamp de la corrida
/// (`ollama-tool-call-<nanos>-N`), así que dos re-corridas
/// deterministas byte-idénticas en TODO lo demás difieren igual en los
/// ids (encontrado en vivo, piloto pizzeria v2 vs v3). Se reemplaza
/// cada id por un placeholder estable por orden de aparición y se
/// serializa — dos logs con la misma forma canónica son la misma
/// trayectoria.
fn canonical_log(events: &[AgentEvent]) -> Vec<String> {
    use std::collections::HashMap;

    fn rewrite(value: &mut serde_json::Value, ids: &mut HashMap<String, String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    if (key == "id" || key == "tool_call_id")
                        && let serde_json::Value::String(s) = val
                    {
                        let next = format!("canonical-call-{}", ids.len());
                        let placeholder = ids.entry(s.clone()).or_insert(next);
                        *s = placeholder.clone();
                    } else {
                        rewrite(val, ids);
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    rewrite(item, ids);
                }
            }
            _ => {}
        }
    }

    let mut ids = HashMap::new();
    events
        .iter()
        .map(|event| {
            let mut value = serde_json::to_value(event).unwrap_or_default();
            rewrite(&mut value, &mut ids);
            value.to_string()
        })
        .collect()
}

/// Punto de entrada del subcomando: carga el results.json (mismo formato
/// que consume DBV), exporta y reporta el summary por stderr/stdout.
pub fn run(cli: ExportCli) -> Result<(), BenchError> {
    let file = crate::dbv::load_baseline_ref(&cli.results)?;
    let output = std::fs::File::create(&cli.output).map_err(|e| {
        BenchError::Startup(format!("creando '{}': {e}", cli.output.display()))
    })?;
    let mut writer = std::io::BufWriter::new(output);
    let summary = export_rows(
        &file.results,
        &cli.sessions,
        cli.include_failed,
        &cli.backends,
        Some(file.metadata.suite_fingerprint.as_str()),
        file.metadata.braze_git_commit.as_deref(),
        &mut writer,
    )?;
    writer
        .flush()
        .map_err(|e| BenchError::Startup(format!("cerrando '{}': {e}", cli.output.display())))?;

    println!(
        "export-sft: {} trayectorias exportadas a {} \
         (saltadas: {} no-passed, {} por filtro de backend; {} sin sesión preservada; \
         {} lossy)",
        summary.exported,
        cli.output.display(),
        summary.skipped_not_passed,
        summary.skipped_backend_filter,
        summary.missing_sessions,
        summary.lossy_trajectories,
    );
    if summary.exported == 0 {
        eprintln!(
            "export-sft: ADVERTENCIA — 0 trayectorias exportadas. Causas típicas: el sweep \
             corrió sin BRAZE_BENCH_KEEP_SESSIONS=1, la raíz --sessions no es la del sweep, \
             o el filtro --backend no coincide con ningún display name del results.json."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use braze_types::ToolResult;

    fn user(text: &str) -> AgentEvent {
        AgentEvent::UserMessage {
            text: text.to_string(),
        }
    }

    fn assistant_text(text: &str) -> AgentEvent {
        AgentEvent::AssistantText {
            text: text.to_string(),
        }
    }

    fn tool_call(id: &str, name: &str, args: serde_json::Value) -> AgentEvent {
        AgentEvent::AssistantToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args,
        }
    }

    fn tool_started(id: &str, name: &str) -> AgentEvent {
        AgentEvent::ToolCallStarted {
            id: id.to_string(),
            name: name.to_string(),
            background: false,
        }
    }

    fn tool_done(id: &str, content: &str) -> AgentEvent {
        AgentEvent::ToolCallCompleted {
            id: id.to_string(),
            result: ToolResult {
                tool_call_id: id.to_string(),
                content: content.to_string(),
                is_error: false,
            },
        }
    }

    fn usage() -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: 10,
            output_tokens: 5,
            stop_reason: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
        }
    }

    /// La forma real de una ronda con texto + tool call, con los eventos
    /// audit (`ToolCallStarted`, `Usage`) intercalados como los persiste
    /// `dispatch_tool_calls`: un solo mensaje assistant (texto +
    /// tool_calls), luego el mensaje tool, luego el cierre.
    #[test]
    fn a_full_round_groups_into_assistant_tool_and_final_messages() {
        let events = vec![
            user("¿cuánto cuesta la napolitana familiar?"),
            assistant_text("Voy a consultar el menú."),
            tool_call("call-1", "shell_exec", serde_json::json!({"command": ["python3", "pizzeria.py", "menu"]})),
            tool_started("call-1", "shell_exec"),
            usage(),
            tool_done("call-1", "napolitana familiar: $11900"),
            assistant_text("11900"),
            usage(),
        ];

        let conversion = events_to_messages(&events);

        assert!(!conversion.is_lossy());
        assert_eq!(conversion.audit_events.get("usage"), Some(&2));
        assert_eq!(conversion.audit_events.get("tool_call_started"), Some(&1));

        let roles: Vec<&str> = conversion.messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "assistant"]);

        let assistant = &conversion.messages[1];
        assert_eq!(assistant.content.as_deref(), Some("Voy a consultar el menú."));
        let calls = assistant.tool_calls.as_ref().expect("tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "shell_exec");
        // Convención del formato: arguments es JSON codificado como string.
        let parsed: serde_json::Value = serde_json::from_str(&calls[0].function.arguments)
            .expect("arguments debe ser JSON re-parseable");
        assert_eq!(parsed["command"][0], "python3");

        let tool = &conversion.messages[2];
        assert_eq!(tool.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(tool.content.as_deref(), Some("napolitana familiar: $11900"));

        assert_eq!(conversion.messages[3].content.as_deref(), Some("11900"));
        assert!(conversion.messages[3].tool_calls.is_none());
    }

    /// Una ronda con 2 tool calls concurrentes y sin texto: un solo
    /// mensaje assistant con content null y ambas calls, luego los dos
    /// mensajes tool en orden de finalización.
    #[test]
    fn concurrent_calls_without_text_share_one_assistant_message_with_null_content() {
        let events = vec![
            user("lee ambos"),
            tool_call("a", "read_file", serde_json::json!({"path": "x.txt"})),
            tool_call("b", "read_file", serde_json::json!({"path": "y.txt"})),
            tool_done("b", "contenido y"),
            tool_done("a", "contenido x"),
        ];

        let conversion = events_to_messages(&events);
        let roles: Vec<&str> = conversion.messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "tool"]);

        let assistant = &conversion.messages[1];
        assert_eq!(assistant.content, None);
        assert_eq!(assistant.tool_calls.as_ref().unwrap().len(), 2);

        // Serialización: content null presente, tool_calls omitido en user.
        let json = serde_json::to_string(&conversion.messages[0]).unwrap();
        assert!(json.contains("\"content\":\"lee ambos\""));
        assert!(!json.contains("tool_calls"));
        let json = serde_json::to_string(assistant).unwrap();
        assert!(json.contains("\"content\":null"));
        assert!(json.contains("\"type\":\"function\""));
    }

    /// Los eventos que el modelo vio pero cuyo framing vive en
    /// braze-engine::history marcan la trayectoria lossy — nunca se
    /// descartan en silencio.
    #[test]
    fn plan_and_verification_events_mark_the_trajectory_lossy() {
        let events = vec![
            user("haz la tarea"),
            AgentEvent::PlanCreated {
                plan: "1. leer 2. editar".to_string(),
            },
            assistant_text("listo"),
            AgentEvent::VerificationFailed {
                output: "boom".to_string(),
            },
        ];

        let conversion = events_to_messages(&events);
        assert!(conversion.is_lossy());
        assert_eq!(conversion.lossy_events.get("plan_created"), Some(&1));
        assert_eq!(conversion.lossy_events.get("verification_failed"), Some(&1));
        // La conversación exportable sigue presente.
        let roles: Vec<&str> = conversion.messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec!["user", "assistant"]);
    }

    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "braze-bench-sft-test-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn preserved_session(root: &std::path::Path, backend: &str, task: &str, rep: u32, events: &[AgentEvent]) {
        let dir = preserve::preserved_run_dir(root, backend, task, rep).join("session");
        std::fs::create_dir_all(&dir).unwrap();
        let lines: Vec<String> = events
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        std::fs::write(dir.join("some-session-id.jsonl"), lines.join("\n")).unwrap();
    }

    fn row(backend: &str, task: &str, rep: u32, passed: bool) -> TaskResult {
        let task_def = crate::task::TaskDef {
            session_constraint: None,
            id: task.to_string(),
            prompt: "p".to_string(),
            setup_files: Default::default(),
            expect_tool_call: None,
            accept_tool_calls: Vec::new(),
            expect_no_tool_call: false,
            expect_text_contains: None,
            expect_file_contains: Default::default(),
            expect_cargo_check: false,
            sandbox_commands: Vec::new(),
            skill: Some("single_tool".to_string()),
            expect_max_rounds: None,
            expect_max_tokens: None,
            expect_max_cost_usd: None,
            noise_tools: 0,
            synthetic_tools: Vec::new(),
            memory_condition: None,
            memory_file: None,
            memory_budget_tokens: None,
        };
        let mut r = crate::metrics::harness_error_result(
            backend,
            &task_def,
            rep,
            &BenchError::Startup("seed".to_string()),
        );
        r.passed = passed;
        r.run_error = None;
        r
    }

    /// Como [`row`], pero con la clave de correlación completa (tool
    /// names + rondas + sumas de tokens) fijada para calzar con una
    /// sesión preservada concreta. El helper `usage()` de estos tests
    /// aporta 10 in / 5 out por ronda.
    fn row_with_key(
        backend: &str,
        task: &str,
        rep: u32,
        passed: bool,
        tool_call_names: &[&str],
        rounds: u32,
    ) -> TaskResult {
        let mut r = row(backend, task, rep, passed);
        r.tool_call_names = tool_call_names.iter().map(|s| s.to_string()).collect();
        r.rounds = rounds;
        r.input_tokens = rounds * 10;
        r.output_tokens = rounds * 5;
        r
    }

    /// End-to-end del núcleo: filtra por passed y por backend, exporta
    /// las trayectorias preservadas, cuenta la sesión ausente — y cada
    /// fila del results queda contabilizada en exactamente un contador.
    #[test]
    fn export_rows_filters_and_accounts_for_every_row() {
        let root = temp_root("export");
        let expert = "ollama:gpt-oss:20b";
        let weak = "ollama:qwen2.5:3b";

        let events = vec![
            user("pregunta"),
            tool_call("c1", "shell_exec", serde_json::json!({"command": ["python3", "pizzeria.py", "menu"]})),
            tool_done("c1", "napolitana familiar: $11900"),
            assistant_text("11900"),
        ];
        preserved_session(&root, expert, "pizzeria_precio_menu", 0, &events);
        // La rep 1 del experto pasó pero NO fue preservada (missing).
        // El débil pasó también, pero el filtro de backend lo excluye.
        preserved_session(&root, weak, "pizzeria_precio_menu", 0, &events);

        let rows = vec![
            row_with_key(expert, "pizzeria_precio_menu", 0, true, &["shell_exec"], 0),
            row_with_key(expert, "pizzeria_precio_menu", 1, true, &["shell_exec"], 0),
            row(expert, "pizzeria_total", 0, false),
            row_with_key(weak, "pizzeria_precio_menu", 0, true, &["shell_exec"], 0),
        ];

        let mut out = Vec::new();
        let summary = export_rows(
            &rows,
            &root,
            false,
            &[expert.to_string()],
            Some("fp123"),
            Some("commitabc"),
            &mut out,
        )
        .expect("export");

        assert_eq!(
            summary,
            ExportSummary {
                exported: 1,
                skipped_not_passed: 1,
                skipped_backend_filter: 1,
                missing_sessions: 1,
                lossy_trajectories: 0,
            }
        );
        // Cada fila en exactamente un contador.
        assert_eq!(
            (summary.exported
                + summary.skipped_not_passed
                + summary.skipped_backend_filter
                + summary.missing_sessions) as usize,
            rows.len()
        );

        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["metadata"]["task_id"], "pizzeria_precio_menu");
        assert_eq!(parsed["metadata"]["backend"], expert);
        assert_eq!(parsed["metadata"]["passed"], true);
        assert_eq!(parsed["metadata"]["lossy"], false);
        assert_eq!(parsed["metadata"]["suite_fingerprint"], "fp123");
        assert_eq!(parsed["metadata"]["braze_git_commit"], "commitabc");
        assert_eq!(parsed["messages"][0]["role"], "user");
        assert_eq!(parsed["messages"][1]["role"], "assistant");
        assert_eq!(
            parsed["messages"][1]["tool_calls"][0]["function"]["name"],
            "shell_exec"
        );
        assert_eq!(parsed["messages"][2]["role"], "tool");
        assert_eq!(parsed["messages"][3]["content"], "11900");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `--include-failed` exporta también las corridas no-passed (para
    /// análisis) — el default de arriba las salta.
    #[test]
    fn include_failed_exports_non_passed_rows_too() {
        let root = temp_root("include-failed");
        let backend = "ollama:qwen2.5:3b";
        preserved_session(&root, backend, "t", 0, &[user("hola"), assistant_text("chao")]);

        let rows = vec![row(backend, "t", 0, false)];
        let mut out = Vec::new();
        let summary =
            export_rows(&rows, &root, true, &[], None, None, &mut out).expect("export");

        assert_eq!(summary.exported, 1);
        assert_eq!(summary.skipped_not_passed, 0);
        let parsed: serde_json::Value =
            serde_json::from_str(String::from_utf8(out).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(parsed["metadata"]["passed"], false);
        // Sin fingerprint/commit: los campos se omiten, no salen null.
        assert!(parsed["metadata"].get("suite_fingerprint").is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    fn write_session_file(dir: &std::path::Path, name: &str, events: &[AgentEvent]) {
        std::fs::create_dir_all(dir).unwrap();
        let lines: Vec<String> = events
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        std::fs::write(dir.join(name), lines.join("\n")).unwrap();
    }

    /// El estado real que encontró la verificación en vivo (piloto
    /// pizzeria, 3 iteraciones): `preserve` ACUMULA sesiones de sweeps
    /// distintos bajo el mismo `rep<N>/session/`. La selección es por
    /// contenido — calza tool names + rondas contra la fila — nunca por
    /// mtime: aquí la sesión vieja (distinta clave) convive con la de la
    /// corrida del results, y se exporta la correcta.
    #[test]
    fn accumulated_sessions_from_older_sweeps_are_disambiguated_by_content() {
        let root = temp_root("accumulated");
        let backend = "ollama:x";
        let dir = preserve::preserved_run_dir(&root, backend, "t", 0).join("session");
        // Sesión vieja: el modelo de entonces usó grep (clave distinta).
        write_session_file(
            &dir,
            "old-sweep.jsonl",
            &[
                user("pregunta"),
                tool_call("c1", "grep", serde_json::json!({"pattern": "x"})),
                tool_done("c1", "viejo"),
                usage(),
            ],
        );
        // Sesión de la corrida del results: shell_exec, 1 ronda.
        write_session_file(
            &dir,
            "current-sweep.jsonl",
            &[
                user("pregunta"),
                tool_call("c1", "shell_exec", serde_json::json!({"command": ["ls"]})),
                tool_done("c1", "nuevo"),
                assistant_text("listo"),
                usage(),
            ],
        );

        let rows = vec![row_with_key(backend, "t", 0, true, &["shell_exec"], 1)];
        let mut out = Vec::new();
        let summary = export_rows(&rows, &root, false, &[], None, None, &mut out).expect("export");

        assert_eq!(summary.exported, 1);
        assert_eq!(summary.missing_sessions, 0);
        let parsed: serde_json::Value =
            serde_json::from_str(String::from_utf8(out).unwrap().lines().next().unwrap()).unwrap();
        // El contenido exportado es el de la sesión correcta, no la vieja.
        assert_eq!(parsed["messages"][2]["content"], "nuevo");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Un ÚNICO log preservado que no calza con la fila (sesión stale de
    /// una corrida anterior, results de la nueva) se cuenta como
    /// faltante — nunca se exporta una trayectoria que no produjo la
    /// fila.
    #[test]
    fn a_single_stale_session_counts_as_missing_not_exported() {
        let root = temp_root("stale");
        let backend = "ollama:x";
        let dir = preserve::preserved_run_dir(&root, backend, "t", 0).join("session");
        write_session_file(
            &dir,
            "stale.jsonl",
            &[
                user("pregunta"),
                tool_call("c1", "grep", serde_json::json!({"pattern": "x"})),
                tool_done("c1", "viejo"),
                usage(),
            ],
        );

        let rows = vec![row_with_key(backend, "t", 0, true, &["shell_exec"], 1)];
        let mut out = Vec::new();
        let summary = export_rows(&rows, &root, false, &[], None, None, &mut out).expect("export");

        assert_eq!(summary.exported, 0);
        assert_eq!(summary.missing_sessions, 1);
        assert!(out.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Dos sesiones que calzan con la MISMA clave pero difieren en
    /// contenido: elegir una sería inventar el dato — error explícito.
    /// (Si fueran idénticas — re-corridas greedy con el mismo seed —
    /// cualquiera sirve y no es error.)
    #[test]
    fn two_differing_sessions_matching_the_same_row_is_an_explicit_error() {
        let root = temp_root("ambiguous");
        let backend = "ollama:x";
        let dir = preserve::preserved_run_dir(&root, backend, "t", 0).join("session");
        write_session_file(
            &dir,
            "a.jsonl",
            &[
                user("pregunta"),
                tool_call("c1", "shell_exec", serde_json::json!({"command": ["ls"]})),
                tool_done("c1", "salida A"),
                usage(),
            ],
        );
        write_session_file(
            &dir,
            "b.jsonl",
            &[
                user("pregunta"),
                tool_call("c1", "shell_exec", serde_json::json!({"command": ["ls"]})),
                tool_done("c1", "salida B distinta"),
                usage(),
            ],
        );

        let rows = vec![row_with_key(backend, "t", 0, true, &["shell_exec"], 1)];
        let mut out = Vec::new();
        let result = export_rows(&rows, &root, false, &[], None, None, &mut out);
        assert!(result.is_err(), "contenido distinto con la misma clave debe ser error");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// El caso encontrado en vivo (piloto pizzeria v2 vs v3): dos
    /// sesiones acumuladas idénticas en TODO salvo los ids sintéticos
    /// de tool calls (que llevan timestamp de la corrida). Son la misma
    /// trayectoria — se exporta una, sin error.
    #[test]
    fn sessions_identical_modulo_synthetic_ids_are_the_same_trajectory() {
        let root = temp_root("modulo-ids");
        let backend = "ollama:x";
        let dir = preserve::preserved_run_dir(&root, backend, "t", 0).join("session");
        let build = |id: &str| {
            vec![
                user("pregunta"),
                tool_call(id, "shell_exec", serde_json::json!({"command": ["ls"]})),
                tool_done(id, "misma salida"),
                usage(),
            ]
        };
        write_session_file(&dir, "a.jsonl", &build("ollama-tool-call-111-0"));
        write_session_file(&dir, "b.jsonl", &build("ollama-tool-call-999-0"));

        let rows = vec![row_with_key(backend, "t", 0, true, &["shell_exec"], 1)];
        let mut out = Vec::new();
        let summary = export_rows(&rows, &root, false, &[], None, None, &mut out).expect("export");

        assert_eq!(summary.exported, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Dos sesiones idénticas byte a byte (re-corrida determinista con
    /// el mismo seed) calzando la misma fila: cualquiera sirve — se
    /// exporta una sola, sin error.
    #[test]
    fn two_identical_matching_sessions_export_cleanly() {
        let root = temp_root("identical");
        let backend = "ollama:x";
        let dir = preserve::preserved_run_dir(&root, backend, "t", 0).join("session");
        let events = vec![
            user("pregunta"),
            tool_call("c1", "shell_exec", serde_json::json!({"command": ["ls"]})),
            tool_done("c1", "misma salida"),
            usage(),
        ];
        write_session_file(&dir, "a.jsonl", &events);
        write_session_file(&dir, "b.jsonl", &events);

        let rows = vec![row_with_key(backend, "t", 0, true, &["shell_exec"], 1)];
        let mut out = Vec::new();
        let summary = export_rows(&rows, &root, false, &[], None, None, &mut out).expect("export");

        assert_eq!(summary.exported, 1);
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
