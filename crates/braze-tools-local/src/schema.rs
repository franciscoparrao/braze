//! JSON Schema + one-line summaries for the six built-in local tools.
//!
//! Unlike the permissive placeholder schema `braze-model` sends to the
//! wire for a stub before it's resolved (`{"type":"object",
//! "additionalProperties":true}`, PLAN.md Fase 3 note), this crate is the
//! authority that actually defines each tool — so `schema_for` returns a
//! real, tool-specific `input_schema`.

use braze_tools_core::ToolSchema;
use braze_types::ToolStub;
use serde_json::json;

/// The six tool names this provider owns, in the order they're advertised
/// via `list_stubs`.
pub const TOOL_NAMES: [&str; 6] = [
    "read_file",
    "write_file",
    "edit_file",
    "shell_exec",
    "grep",
    "glob",
];

pub fn all_stubs(source: &str) -> Vec<ToolStub> {
    TOOL_NAMES
        .iter()
        .map(|&name| ToolStub {
            name: name.to_string(),
            summary: summary_for(name).to_string(),
            source: source.to_string(),
        })
        .collect()
}

fn summary_for(name: &str) -> &'static str {
    match name {
        "read_file" => "Read the full text contents of a file at a given path.",
        "write_file" => "Create or overwrite a file with the given content.",
        "edit_file" => {
            "Replace one exact, unambiguous occurrence of old_string with new_string in a file."
        }
        "shell_exec" => "Run an argv-style command and capture its stdout, stderr, and exit code.",
        "grep" => {
            "Search for a pattern (literal substring or regex) inside files under a directory."
        }
        "glob" => "List files matching a glob pattern under a directory.",
        _ => "",
    }
}

/// `Some(schema)` for one of the six known tool names, `None` for
/// anything else — the `ToolProvider::resolve_schema` contract requires
/// `Ok(None)` (not an error) when this provider doesn't own `name`.
pub fn schema_for(name: &str) -> Option<ToolSchema> {
    let input_schema = match name {
        "read_file" => json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read, absolute or relative to the working directory."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "write_file" => json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to create or overwrite."
                },
                "content": {
                    "type": "string",
                    "description": "Full content to write to the file."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
        "edit_file" => json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to replace. Must appear exactly once in the file, or the edit is rejected as ambiguous."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text."
                }
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        }),
        "shell_exec" => json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Argv-style command: command[0] is the program, remaining elements are its arguments. Never a raw shell string."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
        "grep" => json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Text to search for. Literal substring by default; a POSIX extended regular expression when regex=true."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search under. Defaults to \".\" if omitted."
                },
                "regex": {
                    "type": "boolean",
                    "description": "If true, interpret pattern as an extended regex (grep -E) instead of a literal substring (grep -F). Defaults to false."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
        "glob" => json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Shell glob pattern matched against file basenames, e.g. \"*.rs\"."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search under. Defaults to \".\" if omitted."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
        _ => return None,
    };

    Some(ToolSchema {
        name: name.to_string(),
        description: summary_for(name).to_string(),
        input_schema,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_stubs_covers_every_tool_name() {
        let stubs = all_stubs("local");
        let names: Vec<&str> = stubs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, TOOL_NAMES.to_vec());
        assert!(stubs.iter().all(|s| s.source == "local"));
    }

    #[test]
    fn schema_for_unknown_tool_is_none() {
        assert!(schema_for("does_not_exist").is_none());
    }

    #[test]
    fn schema_for_every_known_tool_is_some() {
        for name in TOOL_NAMES {
            assert!(schema_for(name).is_some(), "missing schema for {name}");
        }
    }
}
