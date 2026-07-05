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
        name: "quit",
        description: "salir de braze",
    },
    SlashCommand {
        name: "exit",
        description: "salir de braze (alias de /quit)",
    },
];

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
}
