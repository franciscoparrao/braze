//! Built-in local tools (file read/write/edit, shell exec, grep/glob)
//! implementing [`braze_tools_core::ToolProvider`].
//!
//! [`LocalToolsProvider`] is the single [`ToolProvider`] this crate
//! exposes — per the trait's "one provider, many tools" shape, it fronts
//! all six built-in tools (`read_file`, `write_file`, `edit_file`,
//! `shell_exec`, `grep`, `glob`) through one `list_stubs`/
//! `resolve_schema`/`invoke` implementation, not one provider per tool.
//!
//! Every write/edit/shell action is checked against a caller-supplied
//! [`braze_permissions::PermissionGuard`] before it runs. This crate never
//! constructs its own guard — `braze-engine` (Fase 5) owns that policy
//! decision and hands a ready guard to [`LocalToolsProvider::new`]. Reads
//! (`read_file`, `grep`, `glob`) never go through the guard: PLAN.md only
//! requires confirmation for writes/deletes/irreversible commands, not
//! reads.

mod ask_user;
mod edit_file;
mod glob;
mod grep;
mod post_edit_check;
mod provider;
mod read_file;
mod schema;
mod shell_exec;
mod write_file;

#[cfg(test)]
mod test_support;

pub use ask_user::AskUserProvider;
pub use provider::LocalToolsProvider;
