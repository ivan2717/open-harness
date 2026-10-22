use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    #[default]
    ReadOnly,
    WorkspaceWrite,
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
    pub command: String,
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub id: String,
    pub command: String,
    pub status: String,
    pub output: String,
    pub exit_code: Option<i32>,
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
    pub review_incomplete: bool,
}
