//! Lectura de la ventana de contexto de un `llama-server` (llama.cpp)
//! servido como endpoint OpenAI-compatible detrás del backend `zen`.
//!
//! El wire de chat completions no informa el límite de contexto, y sin
//! él `braze` solo compacta por número de eventos: en la sesión Juego de
//! la Vida (2026-10-22) el prompt creció de 4,5k a 34k tokens en 39
//! rondas sin una compactación y el turno murió con HTTP 400 "exceeds the
//! available context size (32768 tokens)". `llama-server` expone el valor
//! en `GET /props` → `default_generation_settings.n_ctx` (el contexto por
//! slot, que es el que limita cada request).

use std::time::Duration;

/// Tope del sondeo: corre una vez al construir el engine, y un endpoint
/// que no es llama-server (o no responde) no debe demorar el arranque.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// `<base>/props` a partir de la base OpenAI-compatible: `/props` vive en
/// la raíz del servidor, no bajo `/v1`.
fn props_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    format!("{root}/props")
}

/// `n_ctx` por slot desde la respuesta de `/props`. Acepta también un
/// `n_ctx` de primer nivel por si una versión lo mueve; `0` o ausente
/// cuentan como "no informado".
fn parse_props_n_ctx(props: &serde_json::Value) -> Option<u32> {
    props
        .get("default_generation_settings")
        .and_then(|s| s.get("n_ctx"))
        .or_else(|| props.get("n_ctx"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

/// Pregunta a `base_url` su ventana de contexto si es un `llama-server`.
/// `None` ante cualquier fallo (red, status, JSON sin el campo): el
/// llamador se queda con la conducta de siempre, sin budget.
pub async fn llama_server_context_window(base_url: &str) -> Option<u32> {
    let client = crate::http_client::build_client_with_timeouts(PROBE_TIMEOUT, PROBE_TIMEOUT);
    let response = client
        .get(props_url(base_url))
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let props: serde_json::Value = response.json().await.ok()?;
    parse_props_n_ctx(&props)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_lives_at_the_server_root_not_under_v1() {
        assert_eq!(props_url("http://127.0.0.1:18080/v1"), "http://127.0.0.1:18080/props");
        assert_eq!(props_url("http://127.0.0.1:18080/v1/"), "http://127.0.0.1:18080/props");
        assert_eq!(props_url("http://host:8080"), "http://host:8080/props");
    }

    /// Forma real de `/props` (llama-server del fork en Nitro, 2026-10-23):
    /// el `n_ctx` está bajo `default_generation_settings`.
    #[test]
    fn reads_the_per_slot_n_ctx() {
        let props = serde_json::json!({
            "default_generation_settings": { "n_ctx": 32768, "params": {} },
            "total_slots": 1
        });
        assert_eq!(parse_props_n_ctx(&props), Some(32768));
        assert_eq!(parse_props_n_ctx(&serde_json::json!({ "n_ctx": 8192 })), Some(8192));
    }

    #[test]
    fn missing_or_zero_n_ctx_is_not_reported() {
        assert_eq!(parse_props_n_ctx(&serde_json::json!({ "total_slots": 1 })), None);
        assert_eq!(
            parse_props_n_ctx(&serde_json::json!({ "default_generation_settings": { "n_ctx": 0 } })),
            None
        );
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_yields_none_quickly() {
        // Puerto 9 (discard) en loopback: conexión rechazada al instante.
        assert_eq!(llama_server_context_window("http://127.0.0.1:9/v1").await, None);
    }
}
