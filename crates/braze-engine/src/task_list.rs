//! Lista de tareas tipada — C′.2 del estudio consolidado
//! (docs/harness-engineering-hooks-skills-2026-07-10.md § I.4).
//!
//! El plan como ESTADO, no como prosa: el fallo dominante de un executor
//! chico en `multi_step` es perder el hilo entre pasos, y la matriz de 4
//! brazos midió que el plan-en-prosa no solo no ayuda — daña
//! (`docs/sweep-matriz-4brazos-2026-07-10.md`). Esta es la alternativa
//! estructurada: tareas con id/estado que el harness re-inyecta
//! compactas en cada ronda ("1 [done] leer; 2 [in_progress] editar"),
//! más baratas en tokens que el plan completo y que no se diluyen con la
//! historia ni se pierden en una compactación.
//!
//! **Off por default** (`Config::enable_task_list`): agregar dos tools
//! al inventario es agregar distractores potenciales para un SLM — la
//! palanca entra al bench por su propia fila
//! (`+ablate:task-list`) y solo se promueve si el A/B pre-registrado del
//! planner (planner→tasks vs planner→prosa vs baseline) la valida.
//!
//! Estado en memoria del `Engine` (por sesión). Deliberadamente NO
//! persistido como eventos propios en v1: las tool calls
//! `task_add`/`task_update` del modelo ya quedan en el rollout log como
//! cualquier otra; un `--resume` pierde el estado vivo (documentado —
//! misma limitación que `activated_deferred_tools` de C′.1).

use braze_types::ToolStub;

/// Estado de una tarea — tres valores, no más: para un 3B cada estado
/// extra es una oportunidad de argumento inválido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskStatus {
    Pending,
    InProgress,
    Done,
}

impl TaskStatus {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "pending" => Some(TaskStatus::Pending),
            "in_progress" => Some(TaskStatus::InProgress),
            "done" | "completed" => Some(TaskStatus::Done),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Done => "done",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskEntry {
    pub(crate) id: usize,
    pub(crate) description: String,
    pub(crate) status: TaskStatus,
}

/// La lista misma — ids secuenciales desde 1 (un 3B maneja mejor "task
/// 2" que un uuid).
#[derive(Debug, Default)]
pub(crate) struct TaskList {
    entries: Vec<TaskEntry>,
    /// Cuántas tool calls REALES han terminado sin error en este turno.
    /// Solo cuenta el camino del registry: las dos tools de esta lista
    /// se manejan inline y nunca llegan al punto que alimenta este
    /// contador, así que un `task_add` no puede ser su propia evidencia.
    successful_tool_calls: usize,
    /// Cuánta de esa evidencia ya fue consumida por transiciones a
    /// `Done` aceptadas. Un `done` exige evidencia PROPIA: sin esto, una
    /// sola tool call exitosa avalaría cerrar la lista entera de un
    /// tirón, que es la forma que toma el modo de falla en un 3B.
    evidence_spent: usize,
    /// Gate de evidencia (`Config::enable_task_evidence`). Off = el
    /// comportamiento anterior a esta palanca: el estado es lo que el
    /// modelo dice que es.
    require_evidence: bool,
}

impl TaskList {
    /// Vacía la lista — J-4 (docs/AUDITORIA-2026-07-v7.md): el estado es
    /// del TURNO, no de la sesión. `Engine::run_turn` la resetea al
    /// entrar; sin esto, los planes de turnos distintos se mezclaban en
    /// el resumen y un pendiente abandonado lo re-inyectaba para
    /// siempre (con costo por ronda monótonamente creciente). Los ids
    /// vuelven a partir de 1 en el turno siguiente — coherente con que
    /// el resumen re-inyectado es el único lugar donde el modelo los ve.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        // Los contadores de evidencia son del TURNO, igual que las
        // entradas (J-4): sin este reset, el trabajo de un turno avalaría
        // los `done` del siguiente.
        self.successful_tool_calls = 0;
        self.evidence_spent = 0;
    }

    /// Enciende el gate de evidencia — ver [`TaskList::require_evidence`].
    pub(crate) fn set_require_evidence(&mut self, require: bool) {
        self.require_evidence = require;
    }

    /// Una tool call del registry terminó sin error. Es la ÚNICA fuente
    /// de evidencia que este gate reconoce.
    pub(crate) fn note_successful_tool_call(&mut self) {
        self.successful_tool_calls = self.successful_tool_calls.saturating_add(1);
    }

    pub(crate) fn add(&mut self, description: &str) -> usize {
        let id = self.entries.len() + 1;
        self.entries.push(TaskEntry {
            id,
            description: description.trim().to_string(),
            status: TaskStatus::Pending,
        });
        id
    }

    /// `Err` con mensaje accionable (vuelve al modelo como tool result
    /// de error) cuando el id no existe. `Ok(Some(description))` solo en
    /// la TRANSICIÓN a `Done` desde otro estado — la señal que el caller
    /// usa para emitir `AgentEvent::TaskCompleted` (braze-memory's
    /// `ProjectMemoryHook` la consume). Done→Done repetido devuelve
    /// `Ok(None)`: nada nuevo que reportar (v8 K-6 — un 3B re-marca
    /// "done" con frecuencia, y cada duplicado emitido contaminaba
    /// `completed_signals` expulsando señales legítimas del cap de 30).
    /// `Ok(None)` también para pending/in_progress.
    pub(crate) fn update(
        &mut self,
        id: usize,
        status: TaskStatus,
    ) -> Result<Option<String>, String> {
        // Gate de evidencia (checkers de Recuris, arXiv:2608.24876 § 2.2.3
        // traducido a lo que este harness puede verificar barato): un goal
        // no pasa a `done` porque el modelo lo afirme, sino porque algo
        // pasó en el entorno desde el último `done` aceptado. Se evalúa
        // ANTES de mutar, para que un rechazo deje la entrada intacta.
        let needs_evidence = self.require_evidence
            && status == TaskStatus::Done
            && self
                .entries
                .iter()
                .any(|entry| entry.id == id && entry.status != TaskStatus::Done);
        if needs_evidence && self.successful_tool_calls <= self.evidence_spent {
            return Err(format!(
                "task {id} stays open: no tool call has succeeded since the last completed \
                 task, so there is no evidence the work was done. Do the work with a tool \
                 first, then mark it done."
            ));
        }

        match self.entries.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => {
                let was_done = entry.status == TaskStatus::Done;
                entry.status = status;
                let completed = status == TaskStatus::Done && !was_done;
                if completed && self.require_evidence {
                    // Consumir la unidad recién avalada. Un Done→Done
                    // repetido no gasta nada (no entra acá), coherente con
                    // K-6.
                    self.evidence_spent = self.evidence_spent.saturating_add(1);
                }
                Ok(completed.then(|| entry.description.clone()))
            }
            None => Err(format!(
                "no task with id {id} — current ids: {}",
                self.entries
                    .iter()
                    .map(|e| e.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// `true` mientras quede algo que hacer — la re-inyección se apaga
    /// sola cuando todo está `done` (una lista completada ya no guía).
    pub(crate) fn has_open_tasks(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.status != TaskStatus::Done)
    }

    /// Siembra desde las líneas numeradas de un plan (`1. leer`,
    /// `2) editar`) — el puente planner→tasks del A/B pre-registrado.
    /// Devuelve cuántas sembró.
    pub(crate) fn seed_from_numbered_plan(&mut self, plan: &str) -> usize {
        let mut seeded = 0;
        for line in plan.lines() {
            let trimmed = line.trim_start();
            let digit_count = trimmed.chars().take_while(char::is_ascii_digit).count();
            if digit_count > 0 && trimmed[digit_count..].starts_with(['.', ')']) {
                let description = trimmed[digit_count + 1..].trim();
                if !description.is_empty() {
                    self.add(description);
                    seeded += 1;
                }
            }
        }
        seeded
    }

    /// El resumen de una línea que se re-inyecta por ronda — compacto a
    /// propósito: la gracia sobre el plan-prosa es costar pocos tokens y
    /// no repetir instrucciones.
    pub(crate) fn summary_line(&self) -> String {
        let rendered: Vec<String> = self
            .entries
            .iter()
            .map(|entry| {
                format!(
                    "{} [{}] {}",
                    entry.id,
                    entry.status.as_str(),
                    entry.description
                )
            })
            .collect();
        format!(
            "Task list: {}. Mark progress with task_update(id, status).",
            rendered.join("; ")
        )
    }
}

/// Los stubs de las dos tools, agregados al inventario solo con
/// `enable_task_list` — ver el module doc sobre por qué off-by-default.
/// Con la redacción del brazo U1 de Q0
/// (`docs/hypothesis-2026-08-28-task-evidence-gate.md`) cuando
/// `insistent`: las descripciones piden el uso explícitamente en vez de
/// ofrecerlo.
///
/// El texto se fija ACÁ y se commitea antes de correr, porque la
/// redacción **es** el tratamiento de U1: elegirla a la vista de los
/// resultados sería iterar sobre el tratamiento.
pub(crate) fn task_tool_stubs_with(insistent: bool) -> Vec<ToolStub> {
    let add_summary = if insistent {
        "REQUIRED FIRST STEP for any request needing more than one action: call \
         task_add once per step BEFORE you touch any other tool. Do this even if \
         the plan seems obvious."
    } else {
        "Add one step to your task list for this request. Use it to break a \
         multi-step request down before acting."
    };
    let update_summary = if insistent {
        "REQUIRED after finishing each step: call task_update(id, \"done\"). Also \
         call it with \"in_progress\" when you start a step. Do not skip this — \
         the task list is how progress is tracked."
    } else {
        "Update a task's status as you work: in_progress when you start it, \
         done when it's finished."
    };
    vec![
        ToolStub {
            name: "task_add".to_string(),
            summary: add_summary.to_string(),
            source: "harness".to_string(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "One short step, imperative form."
                    }
                },
                "required": ["description"],
                "additionalProperties": false
            })),
        },
        ToolStub {
            name: "task_update".to_string(),
            summary: update_summary.to_string(),
            source: "harness".to_string(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "integer",
                        "description": "The task id from the task list."
                    },
                    "status": {
                        "type": "string",
                        "enum": ["pending", "in_progress", "done"],
                        "description": "The task's new status."
                    }
                },
                "required": ["id", "status"],
                "additionalProperties": false
            })),
        },
    ]
}

pub(crate) const TASK_ADD_TOOL: &str = "task_add";
pub(crate) const TASK_UPDATE_TOOL: &str = "task_update";

#[cfg(test)]
mod tests {
    use super::*;

    fn gated() -> TaskList {
        let mut list = TaskList::default();
        list.set_require_evidence(true);
        list
    }

    /// El modo de falla que motiva el gate (v8 K-6): el modelo abre una
    /// tarea y la cierra sin haber ejecutado nada. Sin evidencia el
    /// `done` se rechaza y la entrada queda intacta, no a medias.
    #[test]
    fn closing_a_task_without_any_tool_call_is_rejected() {
        let mut list = gated();
        list.add("editar la función");

        let err = list
            .update(1, TaskStatus::Done)
            .expect_err("sin evidencia no se cierra");
        assert!(err.contains("no evidence"), "got: {err}");
        assert!(
            list.has_open_tasks(),
            "un done rechazado deja la tarea abierta"
        );
        assert!(list.summary_line().contains("1 [pending]"), "estado intacto");
    }

    #[test]
    fn a_successful_tool_call_is_the_evidence_that_closes_a_task() {
        let mut list = gated();
        list.add("editar la función");
        list.note_successful_tool_call();

        let completed = list.update(1, TaskStatus::Done).expect("hay evidencia");
        assert_eq!(completed.as_deref(), Some("editar la función"));
        assert!(!list.has_open_tasks());
    }

    /// El corazón del diseño: la evidencia se CONSUME. Una sola tool call
    /// exitosa no puede avalar el cierre de la lista entera — que es la
    /// forma concreta que toma el modo de falla en un 3B.
    #[test]
    fn evidence_is_spent_so_one_tool_call_closes_only_one_task() {
        let mut list = gated();
        list.add("tarea uno");
        list.add("tarea dos");
        list.note_successful_tool_call();

        list.update(1, TaskStatus::Done).expect("la primera se avala");
        let err = list
            .update(2, TaskStatus::Done)
            .expect_err("la segunda ya no tiene evidencia propia");
        assert!(err.contains("no evidence"), "got: {err}");

        // Y con trabajo nuevo, se cierra.
        list.note_successful_tool_call();
        list.update(2, TaskStatus::Done).expect("evidencia fresca");
        assert!(!list.has_open_tasks());
    }

    /// K-6 se preserva bajo el gate: re-marcar `done` algo ya cerrado no
    /// consume evidencia ni vuelve a emitir señal de completitud.
    #[test]
    fn re_marking_done_neither_spends_evidence_nor_re_signals() {
        let mut list = gated();
        list.add("tarea uno");
        list.add("tarea dos");
        list.note_successful_tool_call();
        list.update(1, TaskStatus::Done).expect("primera");

        assert_eq!(
            list.update(1, TaskStatus::Done).expect("done→done es no-op"),
            None,
            "no re-emite señal de completitud"
        );

        // Si el done repetido hubiera gastado evidencia, esta call no
        // alcanzaría para la segunda tarea.
        list.note_successful_tool_call();
        list.update(2, TaskStatus::Done).expect("segunda");
    }

    /// Los contadores son del TURNO (J-4): el trabajo de un turno no
    /// avala los `done` del siguiente.
    #[test]
    fn clear_resets_the_evidence_counters() {
        let mut list = gated();
        list.add("tarea vieja");
        list.note_successful_tool_call();
        list.note_successful_tool_call();

        list.clear();
        list.add("tarea nueva");
        let err = list
            .update(1, TaskStatus::Done)
            .expect_err("la evidencia del turno anterior no viaja");
        assert!(err.contains("no evidence"), "got: {err}");
    }

    /// Off por default: sin el gate, el comportamiento es exactamente el
    /// anterior a esta palanca.
    #[test]
    fn without_the_gate_a_claim_is_enough() {
        let mut list = TaskList::default();
        list.add("tarea uno");
        list.add("tarea dos");
        assert!(list.update(1, TaskStatus::Done).is_ok());
        assert!(list.update(2, TaskStatus::Done).is_ok());
        assert!(!list.has_open_tasks());
    }

    /// Un id inexistente sigue dando su error de siempre bajo el gate —
    /// el chequeo de evidencia no debe secuestrar ese diagnóstico, que es
    /// el que le dice al modelo qué ids hay.
    #[test]
    fn a_bad_id_still_reports_the_available_ids_under_the_gate() {
        let mut list = gated();
        list.add("tarea uno");
        let err = list.update(9, TaskStatus::Done).expect_err("no id 9");
        assert!(err.contains("current ids: 1"), "got: {err}");
    }

    /// Transiciones que no son a `done` no piden evidencia: marcar algo
    /// `in_progress` es justamente lo que se hace ANTES de trabajar.
    #[test]
    fn in_progress_needs_no_evidence() {
        let mut list = gated();
        list.add("tarea uno");
        list.update(1, TaskStatus::InProgress)
            .expect("in_progress no exige nada");
        assert!(list.summary_line().contains("1 [in_progress]"));
    }

    /// add → update → summary: the full life cycle, plus the actionable
    /// error for a bad id.
    #[test]
    fn add_update_and_summarize() {
        let mut list = TaskList::default();
        assert!(!list.has_open_tasks(), "empty list has nothing open");
        assert_eq!(list.add("leer el archivo"), 1);
        assert_eq!(list.add("editar la función"), 2);

        list.update(1, TaskStatus::Done).expect("id 1 exists");
        list.update(2, TaskStatus::InProgress).expect("id 2 exists");
        let err = list.update(9, TaskStatus::Done).expect_err("no id 9");
        assert!(err.contains("current ids: 1, 2"), "got: {err}");

        let summary = list.summary_line();
        assert!(
            summary.contains("1 [done] leer el archivo"),
            "got: {summary}"
        );
        assert!(summary.contains("2 [in_progress] editar la función"));
        assert!(list.has_open_tasks());

        list.update(2, TaskStatus::Done).unwrap();
        assert!(!list.has_open_tasks(), "all done → reinjection turns off");
    }

    /// `update` reports the completed task's description only on a
    /// transition INTO `Done` — the signal `braze-memory`'s
    /// `ProjectMemoryHook` consumes via `AgentEvent::TaskCompleted`. Any
    /// other transition, including Done→Done, is `Ok(None)`: nothing new
    /// to report.
    #[test]
    fn update_reports_the_description_only_on_transition_to_done() {
        let mut list = TaskList::default();
        list.add("leer el archivo");

        let to_in_progress = list.update(1, TaskStatus::InProgress).unwrap();
        assert_eq!(
            to_in_progress, None,
            "pending -> in_progress is not a completion"
        );

        let to_done = list.update(1, TaskStatus::Done).unwrap();
        assert_eq!(to_done, Some("leer el archivo".to_string()));

        let done_again = list.update(1, TaskStatus::Done).unwrap();
        assert_eq!(done_again, None, "Done→Done must not re-report (v8 K-6)");

        // Regresar a in_progress y completar de nuevo SÍ es una nueva
        // transición — el modelo reabrió la tarea y la volvió a cerrar.
        list.update(1, TaskStatus::InProgress).unwrap();
        let re_done = list.update(1, TaskStatus::Done).unwrap();
        assert_eq!(re_done, Some("leer el archivo".to_string()));
    }

    /// The planner bridge: numbered lines seed tasks; prose lines don't.
    #[test]
    fn seeding_parses_numbered_lines_and_skips_prose() {
        let mut list = TaskList::default();
        let seeded = list.seed_from_numbered_plan(
            "Voy a hacer lo siguiente:\n1. leer notas.txt\n2) sumar los valores\ntres. nada\n3. escribir el resultado",
        );
        assert_eq!(seeded, 3);
        let summary = list.summary_line();
        assert!(summary.contains("1 [pending] leer notas.txt"));
        assert!(summary.contains("2 [pending] sumar los valores"));
        assert!(summary.contains("3 [pending] escribir el resultado"));
        assert!(!summary.contains("nada"));
    }

    /// Status parsing tolerates the aliases a model plausibly emits.
    #[test]
    fn status_parse_accepts_the_documented_forms() {
        assert_eq!(TaskStatus::parse("pending"), Some(TaskStatus::Pending));
        assert_eq!(
            TaskStatus::parse("IN_PROGRESS"),
            Some(TaskStatus::InProgress)
        );
        assert_eq!(TaskStatus::parse("done"), Some(TaskStatus::Done));
        assert_eq!(TaskStatus::parse("completed"), Some(TaskStatus::Done));
        assert_eq!(TaskStatus::parse("wip"), None);
    }
}
