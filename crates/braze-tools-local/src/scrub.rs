//! Scrubber de secretos en el output de las tools (perfil operador,
//! 2026-09-28). Primer reporte de uso real: preguntado "¿qué modelo
//! eres?", el modelo corrió `bash -lc env | grep -i model` y el tool
//! result — con un `ZENODO_TOKEN=…` entero — viajó al proveedor en la
//! nube dentro del contexto. El guard no puede impedir que un comando
//! aprobado imprima lo que imprime; lo que sí puede hacer braze es no
//! reenviar valores que PARECEN credenciales.
//!
//! Heurística deliberadamente simple y conservadora (sin regex crate):
//! - líneas `NOMBRE=valor` / `export NOMBRE=valor` / `NOMBRE: valor` /
//!   `"NOMBRE": "valor"` donde NOMBRE contiene TOKEN, SECRET, PASSWORD,
//!   PASSWD, CREDENTIAL, API_KEY, ACCESS_KEY, PRIVATE_KEY o termina en
//!   `_KEY` → el valor se reemplaza por `[redacted]`;
//! - `Authorization: Bearer <x>` / `Bearer <x>` → `Bearer [redacted]`.
//!
//! Se aplica en `LocalToolsProvider::wrap`, ANTES de truncar y de
//! spillear, así ni el contexto ni `.braze/spill/` retienen el valor.
//! Falsos positivos posibles (un `DEPLOY_KEY=staging` que no era secreto
//! queda redactado); es el lado correcto del error: el modelo puede pedir
//! el valor por otra vía si de verdad lo necesita, y el usuario decide.

const NAME_MARKERS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
];

pub const REDACTED: &str = "[redacted]";

/// ¿Es `name` un nombre de variable/campo que suele guardar un secreto?
fn looks_like_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    NAME_MARKERS.iter().any(|m| upper.contains(m)) || upper.ends_with("_KEY")
}

/// Redacta los valores de secretos aparentes en `text` (ver el doc del
/// módulo). Devuelve el texto sin cambios (misma asignación) cuando no
/// hay nada que redactar.
pub fn scrub_secrets(text: String) -> String {
    if !text.contains('=') && !text.contains(':') && !text.contains("Bearer ") {
        return text;
    }
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match scrub_line(line) {
            Some(scrubbed) => {
                changed = true;
                out.push_str(&scrubbed);
            }
            None => out.push_str(line),
        }
    }
    if changed { out } else { text }
}

fn scrub_line(line: &str) -> Option<String> {
    let mut result: Option<String> = None;
    let current = line;
    // `Bearer <token>`: el token es el siguiente token no-blanco.
    if let Some(idx) = current.find("Bearer ") {
        let after = &current[idx + 7..];
        let end = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
            .unwrap_or(after.len());
        if end > 0 && after[..end] != *REDACTED {
            let mut s = String::with_capacity(current.len());
            s.push_str(&current[..idx + 7]);
            s.push_str(REDACTED);
            s.push_str(&after[end..]);
            result = Some(s);
        }
    }
    let current_owned;
    let current: &str = match &result {
        Some(s) => {
            current_owned = s.clone();
            &current_owned
        }
        None => current,
    };
    // `NOMBRE=valor`, `export NOMBRE=valor`, `NOMBRE: valor`, `"NOMBRE": "valor"`.
    let trimmed = current.trim_start();
    let indent = current.len() - trimmed.len();
    let body = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let body_offset = indent + (trimmed.len() - body.len());
    let name_end = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '"' || c == '\''))
        .unwrap_or(body.len());
    let name = body[..name_end].trim_matches(['"', '\'']);
    if name.is_empty() || !looks_like_secret_name(name) {
        return result;
    }
    let rest = &body[name_end..];
    if !(rest.starts_with('=') || rest.starts_with(':')) {
        return result;
    }
    let sep_len = 1;
    let value_start = body_offset + name_end + sep_len;
    let value = &current[value_start..];
    let value_trimmed = value.trim_start();
    if value_trimmed.is_empty() || value_trimmed.starts_with(REDACTED) {
        return result;
    }
    // Conservar comillas de apertura/cierre y una coma final (JSON).
    let lead_ws = value.len() - value_trimmed.len();
    let (open_quote, inner) = match value_trimmed.chars().next() {
        Some(q @ ('"' | '\'')) => (Some(q), &value_trimmed[1..]),
        _ => (None, value_trimmed),
    };
    let inner_end = match open_quote {
        Some(q) => inner.find(q).unwrap_or(inner.len()),
        None => inner
            .find(|c: char| c.is_whitespace() || c == ',')
            .unwrap_or(inner.len()),
    };
    if inner_end == 0 {
        return result;
    }
    let mut s = String::with_capacity(current.len());
    s.push_str(&current[..value_start + lead_ws]);
    if let Some(q) = open_quote {
        s.push(q);
    }
    s.push_str(REDACTED);
    s.push_str(&inner[inner_end..]);
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_dumps_config_json_and_bearer_headers_are_redacted_but_normal_lines_are_not() {
        let input = "PATH=/usr/bin\nZENODO_TOKEN=jxBZabc123\nexport OPENAI_API_KEY='sk-xyz'\n\
                     \"zen_api_key\": \"opc_123\",\n  db_password: hunter2\nAuthorization: Bearer eyJ.abc\n\
                     MAX_KEY_LEN=32\nHOME=/home/u\nnormal text = with equals"
            .to_string();
        let out = scrub_secrets(input);
        assert_eq!(
            out,
            "PATH=/usr/bin\nZENODO_TOKEN=[redacted]\nexport OPENAI_API_KEY='[redacted]'\n\
             \"zen_api_key\": \"[redacted]\",\n  db_password: [redacted]\nAuthorization: Bearer [redacted]\n\
             MAX_KEY_LEN=32\nHOME=/home/u\nnormal text = with equals"
        );
        // `_KEY` al final sí (parece credencial); `_KEY_LEN` no.
        assert_eq!(scrub_secrets("SSH_KEY=abc".to_string()), "SSH_KEY=[redacted]");
        // Sin secretos: mismo texto.
        let plain = "fn main() {}\nlet key = 1;".to_string();
        assert_eq!(scrub_secrets(plain.clone()), plain);
        // Idempotente.
        let once = scrub_secrets("TOKEN=abc".to_string());
        assert_eq!(scrub_secrets(once.clone()), once);
    }
}
