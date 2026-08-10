//! Errors surfaced by [`crate::Engine`].
//!
//! Kept small deliberately: `#[from]` covers propagation from each
//! composed crate's own error type, so there is no need for a bespoke
//! variant per possible failure inside the loop itself.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    #[error("session store error: {0}")]
    Session(#[from] braze_session::SessionError),

    #[error("model backend error: {0}")]
    Model(#[from] braze_model::ModelError),

    #[error("tool registry error: {0}")]
    Tool(#[from] braze_tools_core::ToolError),

    /// The main loop hit its safety iteration cap (see
    /// `Engine::MAX_TURN_ITERATIONS`) without the model converging on a
    /// final text-only response. Not necessarily a bug in the model or the
    /// engine — a legitimate long tool-use chain could also hit this —
    /// but the MVP has no better answer than surfacing it and stopping
    /// rather than looping forever.
    #[error("turn exceeded the maximum of {0} model/tool-call round trips without converging")]
    TurnDidNotConverge(usize),

    /// The turn's cumulative token consumption (input + output summed
    /// across every round) blew past `Engine::max_turn_total_tokens` (v4
    /// P0.2, docs/AUDITORIA-2026-07-v6.md § roadmap Paquete 3) and the
    /// graceful tools-free summary attempt didn't produce usable text
    /// either. The circuit breaker `max_turn_iterations` can't provide
    /// this: a turn of few rounds can still accumulate hundreds of
    /// thousands of input tokens re-sending a growing history (caso
    /// real: 481K tokens de input en un turno de 40 rondas, sesión
    /// ccd4621b — CLAUDE.md § "Próximos pasos").
    #[error(
        "turn spent {spent_tokens} tokens, past its budget of {budget_tokens}, without converging"
    )]
    TurnBudgetExhausted {
        budget_tokens: u64,
        spent_tokens: u64,
    },

    /// The turn ran out of its wall-clock budget
    /// (`Engine::max_turn_wall_clock`) at a round boundary — la condición
    /// de corte de primera clase que pide la línea round-economics
    /// (`docs/hypothesis-2026-07-28-round-economics.md` § "Factibilidad
    /// hoy"). Distinta de las otras dos en lo que mide: `max_turn_iterations`
    /// cuenta rondas y `max_turn_total_tokens` cuenta tokens, ambos
    /// invariantes al precio de una ronda; este cuenta el recurso que SÍ
    /// cambia cuando la ronda se abarata, y es lo que permite comparar
    /// configuraciones a presupuesto de tiempo fijo en vez de a rondas
    /// fijas.
    ///
    /// A diferencia de `TurnBudgetExhausted`, este corte NO intenta la
    /// ronda de resumen sin tools: esa ronda cuesta tiempo, y su costo
    /// escala con el precio de la ronda — es decir, con el factor
    /// experimental. Concederla le regalaría una ronda extra al brazo
    /// caro medida en el mismo eje que el experimento manipula, que es
    /// exactamente el confundido que este corte existe para evitar.
    #[error(
        "turn spent {elapsed_ms} ms, past its wall-clock budget of {budget_ms} ms, \
         without converging (stopped at a round boundary after {rounds_completed} round(s))"
    )]
    TurnWallClockExhausted {
        budget_ms: u128,
        elapsed_ms: u128,
        rounds_completed: usize,
    },

    /// UNA ronda superó `Engine::max_round_wall_clock` y el stream se
    /// abandonó a mitad de generación. Es el deadline a nivel de
    /// streaming que el piloto de round-economics anotó como defecto del
    /// instrumento (`docs/round-economics-pilot-costo-2026-08-08.md`
    /// § 4.4): el corte de `TurnWallClockExhausted` evalúa en el borde de
    /// la ronda, así que no puede acotar una ronda que no termina — con
    /// generación en CPU a ~6 tok/s y un presupuesto de tokens amplio,
    /// una sola ronda pasaba de los 600 s del backstop con `rounds` 0-1 y
    /// contabilidad censurada.
    ///
    /// A diferencia de aquel corte, este NO deja la contabilidad de la
    /// ronda en vuelo intacta — no puede: el `Usage` llega al final del
    /// stream y el stream se abandonó. Lo que sí preserva, y el backstop
    /// de infraestructura no, es todo lo anterior: las rondas completadas
    /// del turno ya persistieron sus eventos y su usage, y el error sale
    /// por el camino normal de `run_turn` en vez de matar el future desde
    /// afuera. El texto/tool-calls parciales de la ronda cortada se
    /// descartan por la misma razón que el brazo `Err` del stream: un
    /// intento inconcluso no debe persistirse como respuesta convergida.
    #[error(
        "a single round spent {elapsed_ms} ms, past the per-round deadline of {deadline_ms} ms — \
         the stream was abandoned mid-generation"
    )]
    RoundWallClockExhausted { deadline_ms: u128, elapsed_ms: u128 },

    /// A `ModelBackend`'s completion stream ended without ever yielding
    /// `CompletionEvent::Done` and without reporting an `Err` first — an
    /// invariant every implementation must uphold (see
    /// `braze_model::ModelBackend::complete`'s doc comment). Surfacing
    /// this instead of silently treating whatever partial text/tool-calls
    /// arrived as a complete, converged response.
    #[error("model backend's completion stream ended without a terminal event")]
    IncompleteStream,

    /// A round with no tool calls stopped because of
    /// `stop_reason: "max_tokens"`/`"length"` — N-24
    /// (docs/AUDITORIA-2026-07-v2.md). The text gathered so far may be cut
    /// off mid-sentence (or mid-tool-call-JSON that then failed to
    /// parse); persisting it as a normal, converged final answer would
    /// look identical downstream to a response the model actually
    /// finished on its own.
    #[error("model's final response was truncated by the token budget before it could finish")]
    TruncatedFinalResponse,

    /// A round produced no text and no tool calls at all — not a
    /// legitimate final answer, just a wasted round. Surfaced as an error
    /// (docs/AUDITORIA-2026-07-v2.md, "una completion vacía termina el
    /// turno como éxito silencioso") rather than treated as silent
    /// convergence, since under best-of-n several empty candidates can
    /// share the same signature and win the vote outright.
    #[error(
        "model's response had no text and requested no tool calls \
         (the round generated {generated_tokens} output tokens — if that is \
         not zero, the model emitted something the harness could not map: \
         a reasoning/commentary channel this backend does not surface, or a \
         tool call that failed to parse and was dropped)"
    )]
    EmptyModelResponse {
        /// Tokens the provider reported for the empty round (`eval_count`
        /// en Ollama). Incidente roam #8 (2026-07-20): un turno murió
        /// con rondas de 11 y 31 tokens que no eran contenido, ni
        /// `thinking`, ni tool call — y el error de entonces ("no text
        /// and no tool calls") no permitía distinguir "el modelo no dijo
        /// NADA" de "el modelo dijo algo que el harness no supo leer".
        /// Sin este número, esa diferencia es invisible en el log.
        generated_tokens: u32,
    },

    /// A second `run_turn` call was attempted on this `Engine` while a
    /// first one was still in flight — N-17
    /// (docs/AUDITORIA-2026-07-v2.md). Not reachable via any current
    /// caller (every caller serializes turns), but two concurrent turns
    /// would share one `TaskNotifier`'s single completion channel and
    /// silently steal each other's tool-call results. Reject the misuse
    /// explicitly instead of leaving it undocumented and unguarded.
    #[error("a run_turn call is already in progress on this Engine")]
    ConcurrentTurn,
}
