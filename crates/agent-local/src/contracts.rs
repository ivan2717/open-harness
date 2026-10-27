use crate::{CommandAccess, CommandGrant};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    #[default]
    ReadOnly,
    WorkspaceWrite,
}

/// A grant applies only to the approved command and its child processes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    #[default]
    Denied,
    Allowed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub status: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub revertible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub id: String,
    /// API projection only: identifies delegated work in its parent's UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
    pub command: String,
    pub cwd: String,
    #[serde(default)]
    pub execution_mode: String,
    #[serde(default)]
    pub network_access: NetworkAccess,
    #[serde(default)]
    pub access: CommandAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
    pub command: String,
    pub status: String,
    pub output: String,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub network_access: NetworkAccess,
    #[serde(default)]
    pub access: CommandAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceReview {
    pub root: String,
    pub permission_mode: PermissionMode,
    pub changes: Vec<FileChange>,
    pub approvals: Vec<PendingApproval>,
    pub commands: Vec<CommandRecord>,
    pub busy: bool,
    pub sandbox_available: bool,
    #[serde(default)]
    pub execution_mode: String,
    pub review_incomplete: bool,
    #[serde(default)]
    pub command_grants: Vec<CommandGrant>,
}

/// An output cursor is local to a command. Reading output never consumes it for
/// another observer; old bytes may be evicted from the bounded live buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandStatus {
    pub command_id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub output: String,
    pub next_cursor: u64,
    pub output_truncated: bool,
    pub has_more_output: bool,
}
