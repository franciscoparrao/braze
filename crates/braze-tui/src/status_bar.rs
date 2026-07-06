//! Status line rendered in the right half of the hint row (`app.rs`'s
//! `draw`) — backend/model, a short session id prefix, and cumulative
//! token usage (from `AgentEvent::Usage`, accumulated in `App`). A pure
//! string-formatting function rather than its own viewport row: keeps
//! `VIEWPORT_HEIGHT` unchanged from oleada 2/3 (the hint row grows a
//! second, right-aligned half instead of the viewport growing a row),
//! and makes the format independently testable without a `Terminal`.

use braze_types::SessionId;

/// Prefix length for the shortened session id — long enough to
/// disambiguate at a glance (like a short git commit hash), short enough
/// not to crowd out backend/model/tokens on a narrow terminal.
const SESSION_PREFIX_LEN: usize = 8;

pub fn render(
    status_line: &str,
    session: SessionId,
    total_input_tokens: u64,
    total_output_tokens: u64,
) -> String {
    let session_str = session.to_string();
    let short_session = &session_str[..SESSION_PREFIX_LEN.min(session_str.len())];
    format!("{status_line} · {short_session} · tokens {total_input_tokens}↑/{total_output_tokens}↓")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_the_status_line_and_token_totals() {
        let session = SessionId::new();
        let rendered = render("ollama:qwen2.5:3b", session, 42, 7);
        assert!(rendered.contains("ollama:qwen2.5:3b"));
        assert!(rendered.contains("42↑"));
        assert!(rendered.contains("7↓"));
    }

    #[test]
    fn shortens_the_session_id_to_a_fixed_prefix() {
        let session = SessionId::new();
        let rendered = render("x", session, 0, 0);
        let full = session.to_string();
        assert!(rendered.contains(&full[..SESSION_PREFIX_LEN]));
        assert!(!rendered.contains(&full));
    }
}
