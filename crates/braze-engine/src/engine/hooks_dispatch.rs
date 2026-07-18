//! Persistencia + espejo a observer + dispatch de hooks — P1.1 paso 4
//! (v8 § 3). Extraído VERBATIM de `engine/mod.rs` (2026-07-18):
//! `append_and_notify` (la única puerta al rollout log durante un
//! turno: el observer se notifica solo DESPUÉS de un append exitoso) y
//! el dispatch de `EngineHook`s con timeout, streak por attach point y
//! auto-disable (Paquete B′).

use super::*;

impl Engine {
    /// Persists `event` to the session store and mirrors it into the
    /// turn's [`TurnObserver`] — the live seam frontends consume (see
    /// PLAN.md § "Fase TUI — diseño"). Persistence stays the source of
    /// truth: the observer is only notified *after* a successful append,
    /// so a frontend can never display an event the rollout log doesn't
    /// have.
    pub(super) async fn append_and_notify(
        &self,
        session: &SessionId,
        event: &AgentEvent,
        observer: &mut dyn TurnObserver,
    ) -> Result<(), EngineError> {
        self.store.append(session, event).await?;
        observer.on_event(event);
        // B′ (docs/harness-engineering-hooks-skills-2026-07-10.md):
        // audit-only hooks see every persisted event — EXCEPT
        // `HookErrored` itself, so a failing hook can't feed back into
        // hook dispatch. A hook whose failure streak crosses the
        // threshold gets its disable recorded as a persisted event
        // (appended directly: the guard above makes re-dispatch moot,
        // but appending without dispatching keeps this non-recursive by
        // construction).
        if !self.hooks.is_empty() && !matches!(event, AgentEvent::HookErrored { .. }) {
            for (id, reason) in self.dispatch_hooks_on_event(event).await {
                let hook_event = AgentEvent::HookErrored {
                    id,
                    point: crate::hooks::HookPoint::OnEvent.as_str().to_string(),
                    reason,
                };
                self.store.append(session, &hook_event).await?;
                observer.on_event(&hook_event);
            }
        }
        Ok(())
    }

    /// Runs every enabled hook's `on_event` under the per-call timeout,
    /// warn-and-continue on failure. Returns the `(id, reason)` of each
    /// hook whose failure streak crossed the disable threshold on THIS
    /// call — the caller persists those as [`AgentEvent::HookErrored`].
    pub(super) async fn dispatch_hooks_on_event(&self, event: &AgentEvent) -> Vec<(String, String)> {
        let mut disabled_now = Vec::new();
        for registered in &self.hooks {
            if registered.is_disabled() {
                continue;
            }
            let outcome = tokio::time::timeout(
                crate::hooks::HOOK_TIMEOUT,
                registered.hook.on_event(event),
            )
            .await;
            if let Some((id, reason)) =
                Self::hook_failure(registered, crate::hooks::HookPoint::OnEvent, outcome)
            {
                disabled_now.push((id, reason));
            }
        }
        disabled_now
    }

    /// `before_model_request` twin of [`Engine::dispatch_hooks_on_event`].
    pub(super) async fn dispatch_hooks_before_model_request(
        &self,
        request: &CompletionRequest,
    ) -> Vec<(String, String)> {
        let mut disabled_now = Vec::new();
        for registered in &self.hooks {
            if registered.is_disabled() {
                continue;
            }
            let outcome = tokio::time::timeout(
                crate::hooks::HOOK_TIMEOUT,
                registered.hook.before_model_request(request),
            )
            .await;
            if let Some((id, reason)) = Self::hook_failure(
                registered,
                crate::hooks::HookPoint::BeforeModelRequest,
                outcome,
            ) {
                disabled_now.push((id, reason));
            }
        }
        disabled_now
    }

    /// Shared failure bookkeeping for both dispatchers: logs the
    /// warn-and-continue line, records the outcome on the hook's streak,
    /// and returns `Some((id, reason))` only on the call that crossed
    /// the disable threshold.
    pub(super) fn hook_failure(
        registered: &crate::hooks::RegisteredHook,
        point: crate::hooks::HookPoint,
        outcome: Result<Result<(), String>, tokio::time::error::Elapsed>,
    ) -> Option<(String, String)> {
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(reason)) => Some(reason),
            Err(_) => Some(format!(
                "timed out after {}ms",
                crate::hooks::HOOK_TIMEOUT.as_millis()
            )),
        };
        if let Some(reason) = &failure {
            tracing::warn!(
                hook = registered.hook.id(),
                point = point.as_str(),
                reason = %reason,
                "engine hook failed (warn_and_continue)"
            );
        }
        let crossed = registered.record_outcome(point, failure.is_some());
        if crossed {
            tracing::warn!(
                hook = registered.hook.id(),
                point = point.as_str(),
                "engine hook disabled after repeated failures"
            );
            return Some((registered.hook.id().to_string(), failure.unwrap_or_default()));
        }
        None
    }
}
