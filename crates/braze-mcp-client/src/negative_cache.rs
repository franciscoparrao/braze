//! Negative-cache de servidor MCP muerto (K-16, docs/AUDITORIA-2026-07-v8.md).
//!
//! El costo que elimina: un server MCP que se cuelga hace que CADA
//! request pague `REQUEST_TIMEOUT` (60 s) completo — un turno de 20
//! rondas contra un server muerto son ~20 minutos de walltime perdido en
//! timeouts idénticos. Tras el primer timeout, esta caché hace que las
//! llamadas siguientes fallen INSTANTÁNEO durante una ventana de
//! cooldown; al vencerse, la próxima llamada pasa como probe (y re-paga
//! el timeout si el server sigue muerto, re-armando la ventana). Un
//! round-trip exitoso la limpia.
//!
//! Solo los TIMEOUTS arman la caché, a propósito: un error de request
//! (transporte roto, subprocess muerto) ya falla rápido por sí solo — el
//! único modo de fallo que re-paga 60 s por ronda es el server colgado.
//!
//! Kill-switch: `BRAZE_MCP_NEGATIVE_CACHE=off` (espejo del
//! `BRAZE_CIRCUIT_BREAKER=off` del breaker de braze-model, y brazo de
//! ablación del bench por la misma vía).

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Estado del cooldown: el instante del último timeout, o `None` si el
/// server está (hasta donde sabemos) sano.
pub(crate) struct NegativeCache {
    cooldown: Duration,
    last_timeout: Mutex<Option<Instant>>,
}

/// Lo que `check` informa cuando la caché está armada — cuánto hace del
/// timeout que la armó y cuánto falta para el próximo probe, para que el
/// error hacia el modelo sea accionable y no un "unavailable" seco.
pub(crate) struct CooldownActive {
    pub(crate) since: Duration,
    pub(crate) retry_in: Duration,
}

impl NegativeCache {
    pub(crate) fn new(cooldown: Duration) -> Self {
        Self {
            cooldown,
            last_timeout: Mutex::new(None),
        }
    }

    /// `Ok(())` = adelante (server presuntamente sano, o ventana vencida —
    /// la llamada actual actúa de probe). `Err` = en cooldown: fallar
    /// instantáneo en vez de re-pagar el timeout.
    pub(crate) async fn check(&self) -> Result<(), CooldownActive> {
        if kill_switched() {
            return Ok(());
        }
        let last = self.last_timeout.lock().await;
        if let Some(stamped) = *last {
            let since = stamped.elapsed();
            if since < self.cooldown {
                return Err(CooldownActive {
                    since,
                    retry_in: self.cooldown - since,
                });
            }
        }
        Ok(())
    }

    /// Un request acaba de agotar su timeout: armar (o re-armar) la
    /// ventana. El probe post-cooldown que vuelve a fallar cae acá y
    /// renueva el cooldown completo.
    pub(crate) async fn note_timeout(&self) {
        *self.last_timeout.lock().await = Some(Instant::now());
    }

    /// Un round-trip completó: el server está vivo, la caché se limpia.
    pub(crate) async fn clear(&self) {
        *self.last_timeout.lock().await = None;
    }
}

fn kill_switched() -> bool {
    std::env::var("BRAZE_MCP_NEGATIVE_CACHE").is_ok_and(|v| v.eq_ignore_ascii_case("off"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_timeout_arms_the_cache_and_the_cooldown_expiry_allows_a_probe() {
        let cache = NegativeCache::new(Duration::from_secs(60));
        assert!(cache.check().await.is_ok(), "sin timeouts, adelante");

        cache.note_timeout().await;
        let blocked = cache.check().await.expect_err("en cooldown debe frenar");
        assert!(blocked.retry_in <= Duration::from_secs(60));

        // El reloj pausado de tokio avanza determinístico.
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(
            cache.check().await.is_ok(),
            "vencida la ventana, la llamada pasa como probe"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_successful_round_trip_clears_the_cache() {
        let cache = NegativeCache::new(Duration::from_secs(60));
        cache.note_timeout().await;
        assert!(cache.check().await.is_err());

        cache.clear().await;
        assert!(cache.check().await.is_ok(), "el éxito limpia el cooldown");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_probe_rearms_the_full_window() {
        let cache = NegativeCache::new(Duration::from_secs(60));
        cache.note_timeout().await;
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(cache.check().await.is_ok(), "probe habilitado");

        // El probe volvió a agotar el timeout: ventana completa de nuevo.
        cache.note_timeout().await;
        let blocked = cache.check().await.expect_err("re-armada");
        assert!(blocked.retry_in > Duration::from_secs(59));
    }
}
