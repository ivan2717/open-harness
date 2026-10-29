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
    #[serde(default)]
    pub supports_images: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ModelReasoning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelReasoning {
    pub default_effort: Option<String>,
    pub supported_efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<agent_providers::ReasoningCapability>,
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
    Paused,
}

/// Persisted ownership; children never become independent roots on reload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegation {
    pub parent_run_id: String,
    pub root_run_id: String,
    pub task_name: String,
    /// None inherits the parent's workspace permissions. An explicit list
    /// restricts writes to leased files; an empty list is read-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_paths: Option<Vec<String>>,
    /// None follows the parent's model at each subsequent delegation turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<UpdateRunModelInput>,
    pub spawn_call_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicy {
    /// User-selected preset: reviewed sandbox access, or explicit full host access.
    #[serde(default)]
    pub approval_mode: agent_local::ApprovalMode,
    /// Optional per-turn model step limit. Interactive conversations default to none.
    pub max_steps: Option<u32>,
    /// Number of concurrently active direct children; the coordinator is excluded.
    pub max_agents: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<Delegation>,
    #[serde(default)]
    pub execution_policy: ExecutionPolicy,
    pub id: String,
    pub revision: u64,
    pub title: String,
    pub prompt: String,
    pub status: RunStatus,
    #[serde(default)]
    pub compacting: bool,
    pub model_id: String,
    /// None uses the registered model's reasoning settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Zero identifies selections persisted before model capability validation.
    /// Only historical restoration may retain these legacy request semantics.
    #[serde(default)]
    pub reasoning_settings_version: u8,
    /// Explicit boundary after which provider-native replay belongs to this binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_history_through: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    /// Private, host-created files for a conversation without a selected project.
    /// This is never accepted from a create request or listed as a user project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_directory: Option<String>,
    #[serde(default)]
    pub permission_mode: agent_local::PermissionMode,
    pub created_at: String,
    pub updated_at: String,
}

impl RunSummary {
    pub fn working_directory(&self) -> Option<&str> {
        self.workspace_path
            .as_deref()
            .or(self.execution_directory.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileReferenceInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ToolImageInfo>,
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
            files: Vec::new(),
            images: Vec::new(),
            reasoning: None,
            tool_name: None,
            is_error: None,
            status: "complete".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolImageInfo {
    pub sequence: u64,
    pub mime_type: String,
}

/// Display metadata only; the durable user event owns the immutable attachment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileReferenceInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub size: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
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
    #[serde(default)]
    pub execution_policy: ExecutionPolicy,
    pub prompt: String,
    #[serde(default)]
    pub files: Vec<agent_core::TextFileReference>,
    pub model_id: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub permission_mode: agent_local::PermissionMode,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRunInput {
    pub message: Option<String>,
    #[serde(default)]
    pub files: Vec<agent_core::TextFileReference>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalInput {
    pub allow: bool,
    #[serde(default)]
    pub scope: crate::service::permissions::CommandDecisionScope,
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
    #[serde(default)]
    pub files: Vec<agent_core::TextFileReference>,
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
    #[serde(default)]
    pub reasoning_capability: Option<agent_providers::ReasoningCapability>,
    #[serde(default)]
    pub reasoning_default: Option<String>,
    #[serde(default)]
    pub supports_images: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceModelInput {
    pub replacement_model_id: String,
    /// Explicit profile edits may change the provider and reasoning contract.
    /// Capacity refreshes retain their stricter replay-compatible behavior.
    #[serde(default)]
    pub allow_configuration_change: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateRunModelInput {
    pub model_id: String,
    /// Omitted/null resets to the chosen model's configured default.
    pub reasoning_effort: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::RunDetail;

    #[test]
    fn compaction_activity_is_shared_without_changing_detail_json_shape() {
        let mut saved = serde_json::json!({
            "id": "run-1", "revision": 2, "title": "test", "prompt": "test",
            "status": "waiting_for_input", "model_id": "local-demo",
            "created_at": "2026-10-22T00:00:00Z", "updated_at": "2026-10-22T00:00:00Z",
            "messages": [], "events": [], "context": null,
            "compacting": true
        });
        let detail: RunDetail = serde_json::from_value(saved.clone()).unwrap();
        assert!(detail.summary.compacting);
        let summary = serde_json::to_value(&detail.summary).unwrap();
        assert_eq!(summary["compacting"], true);
        let serialized = serde_json::to_string(&detail).unwrap();
        assert_eq!(serialized.matches("\"compacting\"").count(), 1);
        assert!(!serialized.contains("\"summary\""));
        let restored: RunDetail = serde_json::from_str(&serialized).unwrap();
        assert!(restored.summary.compacting);

        saved.as_object_mut().unwrap().remove("compacting");
        let legacy: RunDetail = serde_json::from_value(saved).unwrap();
        assert!(!legacy.summary.compacting);
    }
}
