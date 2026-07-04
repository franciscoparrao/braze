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
    Shell {
        program: String,
        subcommand: Option<String>,
    },
    WriteFile {
        path: PathBuf,
    },
    DeleteFile {
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
            program: "mv".to_string(),
            subcommand: Some("a".to_string()),
        });
        round_trip(PermissionKey::Shell {
            program: "ls".to_string(),
            subcommand: None,
        });
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
    fn mcp_tool_call_round_trips() {
        round_trip(PermissionKey::McpToolCall {
            server: "filesystem".to_string(),
            tool: "read_file".to_string(),
        });
    }
}
