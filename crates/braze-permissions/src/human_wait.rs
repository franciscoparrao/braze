//! Reloj de espera humana — el tiempo que el harness pasa BLOQUEADO
//! esperando que una persona decida (aprobación de permiso hoy;
//! cualquier prompt interactivo futuro).
//!
//! Existe por el incidente roam #3 (2026-07-20, tercera sesión de braze
//! en producción): un `shell_exec` que pedía aprobación fue CANCELADO
//! por el `tool_completion_timeout` de 120s mientras el humano miraba
//! el overlay — el modelo recibió "tool call timed out", reintentó, y
//! el usuario terminó aprobando dos veces la misma acción. El comentario
//! del propio `dispatch.rs` asumía que los prompts de aprobación
//! bloquean indefinidamente ("exactamente como los prompts de
//! aprobación"); no lo hacían: el reloj del turno corría contra la
//! deliberación de la persona.
//!
//! El timeout existe para matar una EJECUCIÓN desbocada, no para
//! apurar a un humano. Este módulo deja que el dispatcher descuente la
//! deliberación: `guard` marca el intervalo, el engine extiende su
//! deadline por lo que se acumuló.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static WAITING: AtomicUsize = AtomicUsize::new(0);
static ACCUMULATED_MS: AtomicU64 = AtomicU64::new(0);

/// RAII: marca que el harness está bloqueado esperando a una persona.
/// Al soltarse acumula lo esperado. Nunca falla ni panica — es
/// contabilidad, no control de flujo.
pub struct HumanWait {
    started: Instant,
}

impl HumanWait {
    pub fn start() -> Self {
        WAITING.fetch_add(1, Ordering::SeqCst);
        Self {
            started: Instant::now(),
        }
    }
}

impl Drop for HumanWait {
    fn drop(&mut self) {
        ACCUMULATED_MS.fetch_add(self.started.elapsed().as_millis() as u64, Ordering::SeqCst);
        WAITING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// ¿Hay alguna decisión humana pendiente AHORA? Un dispatcher que llega
/// a su deadline con esto en `true` debe esperar más: la tool no está
/// desbocada, está esperando a una persona.
pub fn is_waiting() -> bool {
    WAITING.load(Ordering::SeqCst) > 0
}

/// Milisegundos totales que el proceso lleva esperando a humanos.
/// Monótono; el llamador compara dos lecturas para saber cuánto se
/// esperó durante SU ventana.
pub fn accumulated() -> Duration {
    Duration::from_millis(ACCUMULATED_MS.load(Ordering::SeqCst))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_wait_is_visible_and_accumulates_on_drop() {
        let before = accumulated();
        assert!(!is_waiting() || WAITING.load(Ordering::SeqCst) > 0);
        {
            let _wait = HumanWait::start();
            assert!(is_waiting(), "una espera viva debe verse");
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(
            accumulated() >= before + Duration::from_millis(10),
            "al soltarse debe acumular lo esperado"
        );
    }

    /// Esperas anidadas/concurrentes: `is_waiting` sigue verdadero
    /// mientras quede alguna viva (dos tool calls del mismo round
    /// pueden pedir aprobación a la vez).
    #[test]
    fn nested_waits_keep_the_flag_until_the_last_one_ends() {
        let outer = HumanWait::start();
        {
            let _inner = HumanWait::start();
            assert!(is_waiting());
        }
        assert!(is_waiting(), "la externa sigue viva");
        drop(outer);
    }
}
