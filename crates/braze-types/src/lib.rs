//! Shared plain-data vocabulary for braze.
//!
//! No logic lives here — only the types every other crate speaks, so that
//! sibling crates (e.g. `braze-tools-core` and `braze-model`) can agree on
//! a shape without depending on each other.

mod message;
mod permission;
mod session;
mod tool;

pub use message::{ContentBlock, Message, Role};
pub use permission::{PermissionKey, deserialize_permission_key_lossy};
pub use session::SessionId;
pub use tool::{ToolCall, ToolResult, ToolStub};
