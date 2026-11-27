use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PermissionError {
    #[error("action denied: {0}")]
    Denied(String),

    /// Prohibida por una regla de la política declarativa (Enclave M3):
    /// no se preguntó a nadie y no se va a preguntar — cambiar la política
    /// es la única salida.
    #[error("action forbidden by policy rule `{rule}`{}: {action}", reason_suffix(.reason))]
    Forbidden {
        action: String,
        rule: String,
        reason: String,
    },

    /// Denegada por el modo automático (regla determinista o juez), sin
    /// preguntar a nadie. No se ejecutó.
    #[error("action blocked by auto mode (`{decided_by}`){}: {action}", reason_suffix(.reason))]
    AutoDenied {
        action: String,
        decided_by: String,
        reason: String,
    },
}

fn reason_suffix(reason: &str) -> String {
    if reason.is_empty() {
        String::new()
    } else {
        format!(" ({reason})")
    }
}
