use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Identifies a class of already-approved irreversible action, coarse
/// enough that similar future actions (e.g. the same shell verb, the same
/// file path) can be auto-approved without re-prompting within a session
/// — and, once persisted in `AgentEvent::PermissionDecided`, replayed back
/// into a fresh `PermissionGuard` when a session is resumed.
///
/// Lives in `braze-types` (not `braze-permissions`) so that `braze-events`
/// can embed it in `AgentEvent` without depending on `braze-permissions` —
/// same rationale as `ToolStub`'s placement (see `tool.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionKey {
    /// The full argv, not just `command[0]`/`command[1]`. A key derived
    /// from only the program and first argument would make approving one
    /// invocation (e.g. `rm -rf /tmp/build`) silently auto-approve any
    /// other invocation of the same program+subcommand regardless of its
    /// remaining arguments (e.g. `rm -rf /`) — remembering must be as
    /// specific as the action actually confirmed.
    Shell {
        command: Vec<String>,
    },
    WriteFile {
        path: PathBuf,
    },
    DeleteFile {
        path: PathBuf,
    },
    ReadPath {
        path: PathBuf,
    },
    McpToolCall {
        server: String,
        tool: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(key: PermissionKey) {
        let json = serde_json::to_string(&key).expect("serialize");
        let decoded: PermissionKey = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(key, decoded);
    }

    #[test]
    fn shell_round_trips() {
        round_trip(PermissionKey::Shell {
            command: vec!["mv".to_string(), "a".to_string(), "b".to_string()],
        });
        round_trip(PermissionKey::Shell {
            command: vec!["ls".to_string()],
        });
    }

    #[test]
    fn shell_keys_with_different_arguments_are_distinct() {
        let narrow = PermissionKey::Shell {
            command: vec![
                "rm".to_string(),
                "-rf".to_string(),
                "/tmp/build".to_string(),
            ],
        };
        let broad = PermissionKey::Shell {
            command: vec!["rm".to_string(), "-rf".to_string(), "/".to_string()],
        };
        assert_ne!(
            narrow, broad,
            "approving one target must not derive the same key as a different target"
        );
    }

    #[test]
    fn write_file_round_trips() {
        round_trip(PermissionKey::WriteFile {
            path: PathBuf::from("/tmp/foo.txt"),
        });
    }

    #[test]
    fn delete_file_round_trips() {
        round_trip(PermissionKey::DeleteFile {
            path: PathBuf::from("/tmp/foo.txt"),
        });
    }

    #[test]
    fn read_path_round_trips() {
        round_trip(PermissionKey::ReadPath {
            path: PathBuf::from("/etc/shadow"),
        });
    }

    #[test]
    fn mcp_tool_call_round_trips() {
        round_trip(PermissionKey::McpToolCall {
            server: "filesystem".to_string(),
            tool: "read_file".to_string(),
        });
    }
}
