//! Loading [`ConfigOverrides`] from the on-disk JSON config file.

use std::path::Path;

use crate::error::ConfigError;
use crate::overrides::ConfigOverrides;

/// Every top-level key [`ConfigOverrides`] actually recognizes — kept in
/// sync by hand with that struct's fields (used only for the
/// unrecognized-key warning below; a mismatch here means a missing/stale
/// warning, not an incorrectly-rejected config, so this is best-effort,
/// same as the rest of this codebase's non-critical diagnostics).
const KNOWN_OVERRIDE_KEYS: &[&str] = &[
    "default_backend",
    "anthropic_api_key",
    "anthropic_model",
    "ollama_base_url",
    "ollama_model",
    "ollama_num_ctx",
    // v5 H-9: these five sampling keys existed in ConfigOverrides and
    // applied correctly via env, but were missing here — a config FILE
    // that used them triggered `tracing::warn!("unrecognized config file
    // key; ignored")` even though the value silently went through anyway
    // (serde_json::from_value ignores our warning). Fix: add them so the
    // warning no longer misleads.
    "ollama_temperature",
    "ollama_seed",
    "ollama_top_p",
    "ollama_top_k",
    "ollama_repeat_penalty",
    "openrouter_api_key",
    "openrouter_model",
    "openrouter_base_url",
    "max_tokens",
    "system_prompt",
    "session_dir",
    "tactical_window",
    "tactical_compaction_threshold",
    "mcp_servers",
    "best_of_n",
    "tui_theme",
    "disable_textual_tool_call_rescue",
    "enable_prompt_caching",
    "disable_post_edit_check",
    "planner_backend",
    "planner_model",
    "lead_backend",
    "lead_model",
    // I-1 (docs/AUDITORIA-2026-07-v6.md) — the three escalation knobs
    // stop being unreachable EscalatingBackend builders. `lead_turns: 0`
    // is the purely-reactive mode.
    "lead_turns",
    "lead_failure_threshold",
    "lead_escalation_turns",
    // v4 P0.2 (mitad rondas) — `max_turn_iterations` and
    // `planner_max_tokens` stop being engine.rs hardcoded constants, now
    // configurable.
    "max_turn_iterations",
    "planner_max_tokens",
    // v4 P0.2 (Paquete 3) — circuit breaker por tokens acumulados por turno.
    "max_turn_total_tokens",
    // v4 P2.4 — `tool_output_max_bytes`/`tool_output_max_lines` configurable
    // truncation limits (previously `MAX_TOOL_OUTPUT_BYTES` hardcoded).
    "tool_output_max_bytes",
    "tool_output_max_lines",
    // v4 P1.6 — per-extension formatter map (generalizes the Rust-only
    // cargo check guardrail).
    "formatters",
    // Paquete 3 (docs/AUDITORIA-2026-07-v6.md) — pricing por
    // backend/modelo para estimated_cost_usd.
    "model_pricing",
    // opencode-10 (docs/opencode-a-braze.md § 10) — directorios de
    // referencia externos con descripción anunciada al modelo.
    "references",
    // C′.1 — umbral de deferral de tools por provider (search_tools).
    "tool_search_threshold",
    // C′.2 — lista de tareas tipada (task_add/task_update).
    "enable_task_list",
];

/// Read and parse the config file at `path` into [`ConfigOverrides`].
///
/// Returns `Ok(None)` if the file does not exist — that is not an error;
/// callers should fall back to defaults and/or other layers. Any other I/O
/// failure (permissions, etc.) or a file that exists but fails to parse as
/// JSON is returned as `Err`.
pub fn load_file(path: &Path) -> Result<Option<ConfigOverrides>, ConfigError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::ReadFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let value: serde_json::Value =
        serde_json::from_str(&contents).map_err(|source| ConfigError::InvalidJson {
            path: path.to_path_buf(),
            source,
        })?;

    // Bajo (docs/AUDITORIA-2026-07-v2.md, "claves de config desconocidas
    // se ignoran sin warning"): an unrecognized key (typo, or a field
    // from a different braze version) is still ignored — rejecting the
    // whole file over one bad key would be worse than the problem this
    // is trying to catch — but now at least logged, instead of vanishing
    // with zero trace.
    if let serde_json::Value::Object(map) = &value {
        for key in map.keys() {
            if !KNOWN_OVERRIDE_KEYS.contains(&key.as_str()) {
                tracing::warn!(key = %key, path = ?path, "unrecognized config file key; ignored");
            }
        }
    }

    let overrides: ConfigOverrides =
        serde_json::from_value(value).map_err(|source| ConfigError::InvalidJson {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(Some(overrides))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_file_returns_none_when_missing() {
        let path = Path::new("/nonexistent/path/that/braze-config/tests/never-create.json");
        let result = load_file(path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_file_parses_valid_json() {
        let dir = std::env::temp_dir().join(format!(
            "braze-config-test-{}-{}",
            std::process::id(),
            "load_file_parses_valid_json"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"default_backend": "anthropic", "max_tokens": 8192}"#,
        )
        .unwrap();

        let overrides = load_file(&path).unwrap().unwrap();
        assert_eq!(overrides.default_backend.as_deref(), Some("anthropic"));
        assert_eq!(overrides.max_tokens, Some(8192));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression test for the "claves de config desconocidas se
    /// ignoran sin warning" bajo (docs/AUDITORIA-2026-07-v2.md): an
    /// unrecognized top-level key must not fail the whole file — the
    /// rest of the known fields still load normally (the warning itself
    /// isn't asserted here, same as the rest of this codebase's
    /// non-critical `tracing` diagnostics).
    #[test]
    fn load_file_ignores_an_unrecognized_key_without_failing() {
        let dir = std::env::temp_dir().join(format!(
            "braze-config-test-{}-{}",
            std::process::id(),
            "load_file_ignores_an_unrecognized_key_without_failing"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"default_backend": "anthropic", "some_future_field": "value"}"#,
        )
        .unwrap();

        let overrides = load_file(&path).unwrap().unwrap();
        assert_eq!(overrides.default_backend.as_deref(), Some("anthropic"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_rejects_invalid_json() {
        let dir = std::env::temp_dir().join(format!(
            "braze-config-test-{}-{}",
            std::process::id(),
            "load_file_rejects_invalid_json"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ not valid json").unwrap();

        let err = load_file(&path).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidJson { .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
