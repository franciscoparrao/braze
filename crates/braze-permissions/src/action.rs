use std::fmt;
use std::path::PathBuf;

/// A concrete action a tool is about to perform, described generically
/// enough that the classifier and a confirmation prompt can reason about
/// it without knowing which tool produced it.
/// ShellCommand is argv-style (command[0] is the program), not a raw
/// shell string.
#[derive(Debug, Clone)]
pub enum ActionDescriptor {
    WriteFile {
        path: PathBuf,
    },
    DeleteFile {
        path: PathBuf,
    },
    /// A read (of a file, or of a directory tree for `grep`/`glob`).
    /// Reversible inside the `WorkdirAllowlist` like `WriteFile`/
    /// `DeleteFile`; outside it, treated as Irreversible so reading e.g.
    /// `~/.ssh/id_rsa` or `/etc/shadow` requires confirmation instead of
    /// happening silently.
    ReadPath {
        path: PathBuf,
    },
    ShellCommand {
        command: Vec<String>,
    },
    /// An invocation of a tool exposed by an external MCP server. Always
    /// classified `Irreversible` by `DefaultClassifier` — an MCP server is
    /// arbitrary, unaudited code chosen by whoever wired it up, so there is
    /// no safe-by-construction subset analogous to `is_safe_shell_command`.
    McpToolCall {
        server: String,
        tool: String,
    },
    /// Anything not classifiable by the fixed MVP table. Always treated as
    /// Reversible by DefaultClassifier.
    Other {
        label: String,
    },
}

/// Human-readable one-liner — this is exactly the string that ends up in
/// braze-events::AgentEvent::PermissionRequested/PermissionDecided once
/// braze-engine wires this crate to braze-session (Fase 5), AND the string
/// both approval prompts (braze-cli's terminal prompt, braze-tui's
/// overlay) put in front of the human.
///
/// Control characters are neutralized to caret notation here, at the
/// single seam every consumer shares (J-19, docs/AUDITORIA-2026-07-v7.md):
/// the payloads are model-controlled (a shell argv) or third-party-
/// controlled (an MCP server's tool/server names), and raw ANSI escapes in
/// them could repaint the very prompt the user is deciding on — hide the
/// dangerous half of a command, recolor `rm -rf` as benign, or forge the
/// "y permitir · n denegar" hint line. `^[` where an ESC would have been
/// is ugly, visible, and honest — exactly what a confirmation prompt
/// should show.
impl fmt::Display for ActionDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = match self {
            Self::WriteFile { path } => format!("write file {}", path.display()),
            Self::DeleteFile { path } => format!("delete file {}", path.display()),
            Self::ReadPath { path } => format!("read path {}", path.display()),
            Self::ShellCommand { command } => format!("run `{}`", command.join(" ")),
            Self::McpToolCall { server, tool } => {
                format!("call MCP tool `{tool}` on server `{server}`")
            }
            Self::Other { label } => label.clone(),
        };
        f.write_str(&sanitize_control_chars(&raw))
    }
}

/// Replaces every control character with a visible stand-in: C0 controls
/// (ESC, CR, backspace, newline, ...) become caret notation (`^[`, `^M`,
/// `^H`, `^J`), DEL becomes `^?`, and any other Unicode control (C1 range)
/// becomes U+FFFD. Plain text passes through byte-identical. Public so
/// other user-facing surfaces that print attacker-influenced strings
/// outside this Display (e.g. `braze permissions suggest`'s report, which
/// renders persisted `PermissionKey`s) can share the exact same treatment.
pub fn sanitize_control_chars(text: &str) -> String {
    // Fast path: nothing to do for the overwhelmingly common clean case.
    if !text.chars().any(char::is_control) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\u{00}'..='\u{1f}' => {
                out.push('^');
                out.push(char::from(c as u8 + 0x40));
            }
            '\u{7f}' => out.push_str("^?"),
            c if c.is_control() => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_write_file() {
        let action = ActionDescriptor::WriteFile {
            path: PathBuf::from("/tmp/foo.txt"),
        };
        assert_eq!(action.to_string(), "write file /tmp/foo.txt");
    }

    #[test]
    fn display_delete_file() {
        let action = ActionDescriptor::DeleteFile {
            path: PathBuf::from("/tmp/foo.txt"),
        };
        assert_eq!(action.to_string(), "delete file /tmp/foo.txt");
    }

    #[test]
    fn display_shell_command() {
        let action = ActionDescriptor::ShellCommand {
            command: vec!["git".to_string(), "push".to_string()],
        };
        assert_eq!(action.to_string(), "run `git push`");
    }

    #[test]
    fn display_mcp_tool_call() {
        let action = ActionDescriptor::McpToolCall {
            server: "filesystem".to_string(),
            tool: "read_file".to_string(),
        };
        assert_eq!(
            action.to_string(),
            "call MCP tool `read_file` on server `filesystem`"
        );
    }

    #[test]
    fn display_read_path() {
        let action = ActionDescriptor::ReadPath {
            path: PathBuf::from("/etc/shadow"),
        };
        assert_eq!(action.to_string(), "read path /etc/shadow");
    }

    #[test]
    fn display_other() {
        let action = ActionDescriptor::Other {
            label: "call MCP tool foo".to_string(),
        };
        assert_eq!(action.to_string(), "call MCP tool foo");
    }

    /// J-19 (docs/AUDITORIA-2026-07-v7.md): a model-controlled argv (or a
    /// hostile MCP server's tool name) carrying ANSI escapes must not be
    /// able to repaint the approval prompt — every control char renders
    /// as a visible stand-in instead of being interpreted by the
    /// terminal.
    #[test]
    fn display_neutralizes_ansi_escapes_and_control_chars() {
        let action = ActionDescriptor::ShellCommand {
            command: vec![
                "echo".to_string(),
                // ESC[2K erases the prompt line; \r returns the cursor;
                // both are classic prompt-forgery primitives.
                "\u{1b}[2K\rrm -rf /".to_string(),
            ],
        };
        assert_eq!(action.to_string(), "run `echo ^[[2K^Mrm -rf /`");

        let action = ActionDescriptor::McpToolCall {
            server: "srv".to_string(),
            tool: "read\u{1b}[1A\u{7f}file".to_string(),
        };
        assert_eq!(
            action.to_string(),
            "call MCP tool `read^[[1A^?file` on server `srv`"
        );
    }

    /// The sanitizer is a byte-identical pass-through for clean text —
    /// the 99.9% case must not pay any rendering difference.
    #[test]
    fn sanitize_control_chars_passes_clean_text_through() {
        let clean = "run `cargo test --workspace` con acentos y ñ";
        assert_eq!(sanitize_control_chars(clean), clean);
    }
}
