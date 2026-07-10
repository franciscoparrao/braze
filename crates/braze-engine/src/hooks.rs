//! Hooks audit-only del engine — Paquete B′ del estudio consolidado
//! (docs/harness-engineering-hooks-skills-2026-07-10.md § Parte II).
//!
//! Primera versión deliberadamente H0/H1: un hook puede OBSERVAR
//! (eventos persistidos, requests a punto de enviarse) pero no mutar
//! nada — la superficie transformadora (H2) y de autoridad (H3) quedan
//! para después de que el bench demuestre que valen su riesgo. Esto
//! habilita observabilidad externa (OTel, auditoría de presupuesto de
//! prompt, clasificación de fallos sobre eventos crudos) sin abrir una
//! superficie que pueda cambiar el comportamiento del turno.
//!
//! Distinto de [`TurnObserver`](braze_events::TurnObserver) a propósito:
//! el observer es el espejo pasivo para UI/headless callers y no puede
//! fallar; un hook es código potencialmente ajeno al binario que puede
//! colgarse o errar, y por eso cada llamada corre bajo timeout
//! ([`HOOK_TIMEOUT`]) y un hook que acumula
//! [`MAX_CONSECUTIVE_HOOK_ERRORS`] se desactiva por el resto de la
//! sesión (failure policy `warn_and_continue` + auto-disable — la única
//! policy de la v1; las demás llegan con H2/H3 si llegan).
//!
//! Sin plugins dinámicos en v1: los hooks se compilan y los registran
//! los composition roots (`braze-cli`/`braze-bench`) vía
//! [`Engine::with_hook`](crate::Engine::with_hook) — la API pública se
//! estabiliza antes de cargar código externo.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use braze_events::AgentEvent;
use braze_model::CompletionRequest;

/// Per-call ceiling for any single hook method — a hook that hangs must
/// never stall the turn it's only supposed to watch.
pub(crate) const HOOK_TIMEOUT: Duration = Duration::from_millis(250);

/// Consecutive failures (error or timeout) at one attach point after
/// which a hook is disabled for the rest of the session — bounded
/// noise, no repeated stalls from a persistently-broken hook. A success
/// resets only its own point's streak (see [`RegisteredHook`]).
pub(crate) const MAX_CONSECUTIVE_HOOK_ERRORS: u32 = 3;

/// An audit-only observer of the engine's operation (H0/H1 — see the
/// module doc comment). Both methods are `&self` with no mutable access
/// to anything of the engine's: a v1 hook cannot influence the turn,
/// only watch it and (via its `Err`) report its own failure.
///
/// Default implementations are no-ops so a hook only implements the
/// points it cares about.
#[async_trait]
pub trait EngineHook: Send + Sync {
    /// Stable identifier — appears in tracing lines and in the
    /// [`AgentEvent::HookErrored`] events this hook's failures persist.
    fn id(&self) -> &str;

    /// Called after every [`AgentEvent`] is persisted and mirrored to
    /// the observer (the "after `append_and_notify`" attach point).
    /// NOT called for [`AgentEvent::HookErrored`] itself — a failing
    /// hook must not feed back into hook dispatch.
    async fn on_event(&self, _event: &AgentEvent) -> Result<(), String> {
        Ok(())
    }

    /// Called with every [`CompletionRequest`] the executor is about to
    /// send (the "before `complete_once`/`complete_with_best_of_n`"
    /// attach point). Read-only by construction: the engine passes a
    /// shared reference and sends the request regardless of what this
    /// returns.
    async fn before_model_request(&self, _request: &CompletionRequest) -> Result<(), String> {
        Ok(())
    }
}

/// A hook plus its per-session failure bookkeeping — what
/// `Engine::hooks` actually stores. The failure streak is tracked PER
/// ATTACH POINT: a hook broken only in `on_event` but healthy in
/// `before_model_request` would otherwise interleave successes into its
/// streak and never get disabled — unbounded warn noise from a
/// permanently-broken point.
pub(crate) struct RegisteredHook {
    pub(crate) hook: Arc<dyn EngineHook>,
    consecutive_errors: [AtomicU32; 2],
    disabled: AtomicBool,
}

/// Where a hook failure happened — the `point` field of
/// [`AgentEvent::HookErrored`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum HookPoint {
    OnEvent,
    BeforeModelRequest,
}

impl HookPoint {
    fn index(self) -> usize {
        match self {
            HookPoint::OnEvent => 0,
            HookPoint::BeforeModelRequest => 1,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            HookPoint::OnEvent => "on_event",
            HookPoint::BeforeModelRequest => "before_model_request",
        }
    }
}

impl RegisteredHook {
    pub(crate) fn new(hook: Arc<dyn EngineHook>) -> Self {
        Self {
            hook,
            consecutive_errors: [AtomicU32::new(0), AtomicU32::new(0)],
            disabled: AtomicBool::new(false),
        }
    }

    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::SeqCst)
    }

    /// Records one call's outcome at `point`. Returns `true` when THIS
    /// failure is the one that crossed [`MAX_CONSECUTIVE_HOOK_ERRORS`]
    /// and disabled the hook — the caller persists the corresponding
    /// [`AgentEvent::HookErrored`] exactly then, bounding the persisted
    /// noise to one event per disable instead of one per failure. A
    /// success only resets the streak of ITS OWN point (see the struct
    /// doc comment); the disable, however, is hook-wide — an audit hook
    /// with one permanently-broken point isn't trustworthy at the other.
    pub(crate) fn record_outcome(&self, point: HookPoint, failed: bool) -> bool {
        let streak = &self.consecutive_errors[point.index()];
        if !failed {
            streak.store(0, Ordering::SeqCst);
            return false;
        }
        let errors = streak.fetch_add(1, Ordering::SeqCst) + 1;
        if errors >= MAX_CONSECUTIVE_HOOK_ERRORS && !self.disabled.swap(true, Ordering::SeqCst) {
            return true;
        }
        false
    }
}

/// H0 concreto del Paquete B′: reporta (vía `tracing::info!`) el
/// desglose aproximado del presupuesto de prompt de cada request —
/// system prompt, schemas de tools, historia — en tokens estimados
/// (~4 chars/token, la misma heurística del resto del proyecto). Ataca
/// el lado observabilidad de I-2: "¿en qué se está yendo el `num_ctx`?"
/// deja de requerir un debugger. Registrado por `braze-bench` en cada
/// corrida (visible con `RUST_LOG=braze_engine=info`); opt-in en
/// producción vía `Engine::with_hook`.
pub struct PromptBudgetAuditHook;

#[async_trait]
impl EngineHook for PromptBudgetAuditHook {
    fn id(&self) -> &str {
        "prompt-budget-audit"
    }

    async fn before_model_request(&self, request: &CompletionRequest) -> Result<(), String> {
        let system_chars = request.system_prompt.len();
        let tools_chars: usize = request
            .tool_stubs
            .iter()
            .map(|stub| {
                stub.name.len()
                    + stub.summary.len()
                    + stub
                        .input_schema
                        .as_ref()
                        .map(|schema| schema.to_string().len())
                        .unwrap_or(0)
            })
            .sum();
        let history_chars: usize = request
            .messages
            .iter()
            .map(|message| {
                message
                    .content
                    .iter()
                    .map(|block| match block {
                        braze_types::ContentBlock::Text { text } => text.len(),
                        braze_types::ContentBlock::ToolUse { name, input, .. } => {
                            name.len() + input.to_string().len()
                        }
                        braze_types::ContentBlock::ToolResult { content, .. } => content.len(),
                    })
                    .sum::<usize>()
            })
            .sum();
        tracing::info!(
            system_tokens_est = system_chars / 4,
            tools_tokens_est = tools_chars / 4,
            history_tokens_est = history_chars / 4,
            total_tokens_est = (system_chars + tools_chars + history_chars) / 4,
            message_count = request.messages.len(),
            "prompt budget breakdown (PromptBudgetAuditHook)"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopHook;
    #[async_trait]
    impl EngineHook for NoopHook {
        fn id(&self) -> &str {
            "noop"
        }
    }

    /// The disable threshold fires exactly once, on the crossing
    /// failure; a success resets only ITS OWN point's streak — the
    /// properties the bounded-noise contract depends on.
    #[test]
    fn record_outcome_disables_once_at_the_threshold_and_success_resets_per_point() {
        let registered = RegisteredHook::new(Arc::new(NoopHook));
        assert!(!registered.record_outcome(HookPoint::OnEvent, true));
        assert!(!registered.record_outcome(HookPoint::OnEvent, true));
        // A success at the OTHER point must NOT reset on_event's streak.
        assert!(!registered.record_outcome(HookPoint::BeforeModelRequest, false));
        // A success at the SAME point does.
        assert!(!registered.record_outcome(HookPoint::OnEvent, false));
        assert!(!registered.record_outcome(HookPoint::OnEvent, true));
        assert!(!registered.record_outcome(HookPoint::OnEvent, true));
        // Third consecutive failure at the point: crosses, disables,
        // reports `true` exactly once.
        assert!(registered.record_outcome(HookPoint::OnEvent, true));
        assert!(registered.is_disabled());
        assert!(
            !registered.record_outcome(HookPoint::OnEvent, true),
            "already disabled — never reports the crossing twice"
        );
    }

    /// The cross-point independence that motivated per-point streaks: a
    /// hook failing only in on_event still gets disabled even though its
    /// before_model_request keeps succeeding in between.
    #[test]
    fn a_success_at_another_point_does_not_rescue_a_broken_point() {
        let registered = RegisteredHook::new(Arc::new(NoopHook));
        for _ in 0..2 {
            assert!(!registered.record_outcome(HookPoint::OnEvent, true));
            assert!(!registered.record_outcome(HookPoint::BeforeModelRequest, false));
        }
        assert!(registered.record_outcome(HookPoint::OnEvent, true));
        assert!(registered.is_disabled());
    }
}
