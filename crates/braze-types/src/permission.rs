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

/// `deserialize_with` for the `key: Option<PermissionKey>` field on
/// `AgentEvent::PermissionRequested`/`PermissionDecided` — N-40
/// (docs/AUDITORIA-2026-07-v2.md, "forward-compat parcial (grupo G)").
///
/// `PermissionKey` has no `#[serde(other)]` catch-all like
/// `AgentEvent::Unknown` does: that attribute only works for
/// internally/adjacently tagged enums, and `PermissionKey` deliberately
/// keeps serde's default externally-tagged representation (already
/// persisted to real rollout logs — retagging it now would itself be a
/// breaking wire-format change). Deserializing through an intermediate
/// `serde_json::Value` and falling back to `None` on a shape this binary
/// doesn't recognize (a new variant from a newer binary) means only the
/// coarse "remembered" identity for that one decision is lost — the
/// containing `AgentEvent::PermissionRequested`/`PermissionDecided` (and
/// the rest of the session log) still deserializes normally, instead of
/// the old behavior (a hard error aborting `load()` for the *entire*
/// session at that line).
pub fn deserialize_permission_key_lossy<'de, D>(
    deserializer: D,
) -> Result<Option<PermissionKey>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
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

    /// Regression test for N-40 (docs/AUDITORIA-2026-07-v2.md): an
    /// unrecognized `PermissionKey` shape (simulating a variant a newer
    /// binary added) must deserialize to `None`, not a hard error.
    #[test]
    fn deserialize_lossy_falls_back_to_none_for_an_unrecognized_shape() {
        let json = serde_json::json!({"SomeFutureVariant": {"field": "value"}});
        let key: Option<PermissionKey> =
            deserialize_permission_key_lossy(json).expect("must not error");
        assert_eq!(key, None);
    }

    #[test]
    fn deserialize_lossy_still_parses_a_recognized_shape() {
        let json = serde_json::json!({"WriteFile": {"path": "/tmp/foo.txt"}});
        let key: Option<PermissionKey> =
            deserialize_permission_key_lossy(json).expect("must not error");
        assert_eq!(
            key,
            Some(PermissionKey::WriteFile {
                path: PathBuf::from("/tmp/foo.txt")
            })
        );
    }

    #[test]
    fn deserialize_lossy_handles_a_missing_key_as_none() {
        let key: Option<PermissionKey> =
            deserialize_permission_key_lossy(serde_json::Value::Null).expect("must not error");
        assert_eq!(key, None);
    }
}
