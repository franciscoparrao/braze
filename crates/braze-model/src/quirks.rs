//! Rarezas por modelo para el backend OpenAI-compatible — recomendación 2
//! del estudio de código fuente "Harness Engineering" (arXiv 2609.00006):
//! una capa declarativa de acondicionamiento por modelo en vez de
//! condicionales desperdigados. Algunos modelos alcanzables por este
//! backend rechazan o restringen parámetros de sampling estándar; esta
//! tabla dice cuáles, así un modelo conocido se acondiciona ANTES del
//! primer request en vez de aprenderse recién de un 400 en vivo.
//!
//! Observado en OpenCode Go/Zen (2026-09-28/30):
//! - `glm-5.3*` / `glm-5.2*`: rechazan `seed` (`[unsupported_parameter]`).
//! - familia `kimi-k2*` (p.ej. `kimi-k2.7-code`): solo aceptan
//!   `temperature = 1`; con 0,2 responden 400 y el brazo del bench quedaba
//!   "no medible".
//!
//! Las rarezas son específicas de cómo OpenCode sirve estos modelos, así
//! que SOLO se aplican cuando el `provider_label` es `"zen"` (Zen/Go). Para
//! OpenRouter u otros gateways, el default permisivo + el fallback de
//! `seed` aprendido en vivo (ver `openrouter.rs`) siguen siendo la red.

/// Qué permite un modelo en el campo `temperature`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TemperaturePolicy {
    /// Cualquier valor (o ninguno) — el caso normal.
    Free,
    /// El proveedor solo acepta este valor exacto; otros dan 400. La
    /// temperatura configurada se sobreescribe a él.
    Locked(f32),
}

/// Restricciones de un modelo sobre los parámetros de sampling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelQuirks {
    /// `false` = omitir `seed` del request (el proveedor lo rechaza).
    pub seed_supported: bool,
    /// Política de `temperature`.
    pub temperature: TemperaturePolicy,
}

impl Default for ModelQuirks {
    fn default() -> Self {
        Self {
            seed_supported: true,
            temperature: TemperaturePolicy::Free,
        }
    }
}

/// Rarezas conocidas de un modelo servido por OpenCode (Zen/Go), por id
/// (match case-insensitive por substring; los ids llegan planos, p.ej.
/// `"glm-5.3-flash"`, `"kimi-k2.7-code"`). Un modelo desconocido recibe el
/// default permisivo y se apoya en el fallback en vivo.
pub fn quirks_for(model: &str) -> ModelQuirks {
    let m = model.to_ascii_lowercase();
    // Familia kimi-k2: temperatura fija en 1,0 (Go rechaza 0,2).
    if m.contains("kimi-k2") {
        return ModelQuirks {
            seed_supported: true,
            temperature: TemperaturePolicy::Locked(1.0),
        };
    }
    // GLM en OpenCode Go/Zen rechaza `seed`.
    if m.contains("glm-5.3") || m.contains("glm-5.2") {
        return ModelQuirks {
            seed_supported: false,
            temperature: TemperaturePolicy::Free,
        };
    }
    ModelQuirks::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_get_their_quirks_and_unknowns_stay_permissive() {
        // kimi-k2*: temperatura bloqueada, seed ok.
        let k = quirks_for("kimi-k2.7-code");
        assert_eq!(k.temperature, TemperaturePolicy::Locked(1.0));
        assert!(k.seed_supported);
        assert_eq!(quirks_for("KIMI-K2.5").temperature, TemperaturePolicy::Locked(1.0));
        // glm-5.x: seed omitido, temperatura libre.
        let g = quirks_for("glm-5.3-flash");
        assert!(!g.seed_supported);
        assert_eq!(g.temperature, TemperaturePolicy::Free);
        assert!(!quirks_for("glm-5.2").seed_supported);
        // Desconocido / deepseek: default permisivo.
        let d = quirks_for("deepseek-v4.1-flash");
        assert!(d.seed_supported);
        assert_eq!(d.temperature, TemperaturePolicy::Free);
        assert_eq!(quirks_for("deepseek-v4.1-flash"), ModelQuirks::default());
    }
}
