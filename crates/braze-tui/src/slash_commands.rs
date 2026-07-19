//! Built-in `/command` registry — "fase TUI 2" (PLAN.md). Handled
//! entirely client-side in `app.rs`'s `submit`: a recognized command
//! never reaches `Engine::run_turn` (the engine has no concept of slash
//! commands at all), matching how Codex/Gemini's built-in commands work.

pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

/// `quit`/`exit` alias each other, matching the plain `chat` loop's own
/// dual acceptance of typed "exit"/"quit" (`braze-cli::main`).
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        description: "atajos de teclado y comandos disponibles",
    },
    SlashCommand {
        name: "model",
        description: "cambiar de backend/modelo (picker, o /model backend[:modelo])",
    },
    SlashCommand {
        name: "skills",
        description: "listar las skills disponibles e insertar una mención $skill",
    },
    SlashCommand {
        name: "quit",
        description: "salir de braze",
    },
    SlashCommand {
        name: "exit",
        description: "salir de braze (alias de /quit)",
    },
];

/// Parses `body` (the text after a leading `/`, e.g. `"quit ahora"` from
/// `"/quit ahora"`) into `(command_name, args)` if the first
/// whitespace-delimited token exactly matches a registered command name —
/// `None` otherwise (not a recognized command, should fall through as
/// ordinary text). Bajo (docs/AUDITORIA-2026-07-v2.md, "slash command con
/// argumentos (/quit ahora) se manda al modelo"): matching the *whole*
/// string against a command name meant a recognized command followed by
/// trailing text never matched, and got sent to the model as if it were
/// ordinary conversation instead of running the command.
pub fn parse_slash_command(body: &str) -> Option<(&'static str, Option<&str>)> {
    let mut parts = body.splitn(2, char::is_whitespace);
    let candidate = parts.next().unwrap_or("");
    let command = SLASH_COMMANDS.iter().find(|c| c.name == candidate)?;
    let args = parts.next().map(str::trim).filter(|s| !s.is_empty());
    Some((command.name, args))
}

/// Commands whose name starts with `query` (case-insensitive), in
/// registry order — prefix match, not substring: command names are
/// short and known upfront, so "matches what you'd type next" is more
/// useful here than "appears anywhere in the name" (unlike file
/// mentions, see `mentions::matching_files`).
pub fn matching_commands(query: &str) -> Vec<&'static SlashCommand> {
    let query = query.to_lowercase();
    SLASH_COMMANDS
        .iter()
        .filter(|cmd| cmd.name.starts_with(&query))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_every_command() {
        assert_eq!(matching_commands("").len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn prefix_query_narrows_to_matching_commands() {
        let matches = matching_commands("qu");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "quit");
    }

    #[test]
    fn model_command_is_registered_and_parses_its_spec_argument() {
        let matches = matching_commands("mo");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "model");

        // The spec keeps its own `:`s intact (an Ollama tag) — args is
        // everything after the first whitespace, verbatim.
        assert_eq!(
            parse_slash_command("model ollama:qwen2.5:7b"),
            Some(("model", Some("ollama:qwen2.5:7b")))
        );
        assert_eq!(parse_slash_command("model"), Some(("model", None)));
    }

    #[test]
    fn query_matching_is_case_insensitive() {
        let matches = matching_commands("HE");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "help");
    }

    #[test]
    fn a_query_that_is_a_substring_but_not_a_prefix_does_not_match() {
        // "uit" is a substring of "quit" but not a prefix — this is a
        // prefix matcher, not substring.
        assert!(matching_commands("uit").is_empty());
    }

    #[test]
    fn unknown_query_matches_nothing() {
        assert!(matching_commands("zzz").is_empty());
    }

    #[test]
    fn parse_slash_command_recognizes_a_bare_command() {
        assert_eq!(parse_slash_command("quit"), Some(("quit", None)));
    }

    /// Regression test for the "slash command con argumentos" bajo
    /// (docs/AUDITORIA-2026-07-v2.md): a recognized command name followed
    /// by trailing text must still be recognized as that command.
    #[test]
    fn parse_slash_command_recognizes_a_command_with_trailing_args() {
        assert_eq!(
            parse_slash_command("quit ahora"),
            Some(("quit", Some("ahora")))
        );
    }

    #[test]
    fn parse_slash_command_returns_none_for_an_unrecognized_command() {
        assert_eq!(parse_slash_command("zzz ahora"), None);
    }

    #[test]
    fn parse_slash_command_trims_and_ignores_all_whitespace_args() {
        assert_eq!(parse_slash_command("quit   "), Some(("quit", None)));
    }
}
