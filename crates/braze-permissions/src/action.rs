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
    ShellCommand {
        command: Vec<String>,
    },
    /// Anything not classifiable by the fixed MVP table (e.g. an MCP tool
    /// call). Always treated as Reversible by DefaultClassifier.
    Other {
        label: String,
    },
}

/// Human-readable one-liner — this is exactly the string that ends up in
/// braze-events::AgentEvent::PermissionRequested/PermissionDecided once
/// braze-engine wires this crate to braze-session (Fase 5).
impl fmt::Display for ActionDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriteFile { path } => write!(f, "write file {}", path.display()),
            Self::DeleteFile { path } => write!(f, "delete file {}", path.display()),
            Self::ShellCommand { command } => write!(f, "run `{}`", command.join(" ")),
            Self::Other { label } => write!(f, "{label}"),
        }
    }
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
    fn display_other() {
        let action = ActionDescriptor::Other {
            label: "call MCP tool foo".to_string(),
        };
        assert_eq!(action.to_string(), "call MCP tool foo");
    }
}
