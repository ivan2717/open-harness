use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub version: String,
    pub models: Vec<ModelInfo>,
    pub default_model: String,
    pub storage_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub available: bool,
    pub context_window: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    WaitingForInput,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub revision: u64,
    pub title: String,
    pub prompt: String,
    pub status: RunStatus,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub permission_mode: agent_local::PermissionMode,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    pub status: String,
}

impl ChatMessage {
    pub fn complete(id: String, role: &str, text: String) -> Self {
        Self {
            id,
            role: role.into(),
            text,
            reasoning: None,
            tool_name: None,
            is_error: None,
            status: "complete".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub sequence: u64,
    pub step: u32,
    pub r#type: String,
    pub label: String,
    pub detail: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionRecord {
    pub id: String,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub summary: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedTokenUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub responses: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub window_tokens: usize,
    pub input_budget: usize,
    pub estimated_input_tokens: usize,
    pub trigger_tokens: usize,
    pub target_tokens: usize,
    pub reserved_output_tokens: usize,
    pub compaction_count: usize,
    pub measurement: String,
    /// The same request measurement used by context budget decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting: Option<agent_runtime::ContextMeasurement>,
    /// Sum of recorded successful response samples, not context occupancy.
    /// Missing samples and failed attempts cannot be inferred from local logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_usage: Option<ObservedTokenUsage>,
    pub checkpoints: Vec<CompactionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDetail {
    #[serde(default)]
    pub compacting: bool,
    #[serde(flatten)]
    pub summary: RunSummary,
    pub messages: Vec<ChatMessage>,
    pub events: Vec<TimelineEvent>,
    pub context: Option<ContextSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRunInput {
    pub prompt: String,
    pub model_id: String,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub permission_mode: agent_local::PermissionMode,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRunInput {
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalInput {
    pub allow: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevertInput {
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitInput {
    pub message: String,
}

/// Credentials are accepted only on configuration and never included in responses.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureModel {
    pub protocol: agent_providers::ProviderProtocol,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub reasoning_mode: String,
    pub reasoning_effort: String,
}
