//! Shared helper for synthesizing a tool-call id when a provider omits
//! one (`ollama_wire.rs`, `openrouter_wire.rs`).

use std::sync::OnceLock;

static PROCESS_NONCE: OnceLock<u64> = OnceLock::new();

/// A value that's stable for the lifetime of this process but differs
/// across process restarts — the missing piece that made
/// `TOOL_CALL_COUNTER` (an `AtomicU64` starting at 0 every run) unsafe to
/// use alone. N-23-adjacent finding (docs/AUDITORIA-2026-07-v2.md, "ids
/// de tool call con contador global de proceso, colisión tras resume"):
/// a synthesized id like `ollama-tool-call-3` from a fresh process after
/// `--resume` can collide with the very same id already persisted by an
/// earlier run of the same session, since both processes' counters start
/// at 0. Mixing this nonce into the id makes a collision astronomically
/// unlikely without needing any session-aware state at this layer (wire
/// modules have no visibility into `SessionId`).
pub(crate) fn process_nonce() -> u64 {
    *PROCESS_NONCE.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    })
}
