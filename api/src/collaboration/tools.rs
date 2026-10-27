use super::*;
use agent_core::{AgentMessage, ToolOutput};
use agent_runtime::{ToolCall, ToolDefinition, ToolExecutor};
use serde_json::json;
use std::sync::Weak;

/// The execution loop sees one protocol-neutral executor. This layer owns
/// delegation; extensions remain ordinary tools with their existing grants.
pub(crate) struct RunTools {
    host: Weak<Service>,
    run_id: String,
    base: crate::extensions::ExtensionTools,
    child: bool,
    definitions: Vec<ToolDefinition>,
}

impl RunTools {
    pub(crate) fn new(
        state: &AppState,
        run_id: String,
        base: crate::extensions::ExtensionTools,
        child: bool,
    ) -> Self {
        let mut tools = Self {
            host: Arc::downgrade(&state.inner),
            run_id,
            base,
            child,
            definitions: Vec::new(),
        };
        tools.definitions = tools.snapshot_definitions();
        tools
    }
    fn state(&self) -> io::Result<AppState> {
        self.host
            .upgrade()
            .map(|inner| AppState { inner })
            .ok_or_else(|| io::Error::other("execution host has stopped"))
    }
}

impl ToolExecutor for RunTools {
    type Error = io::Error;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
    fn snapshot_definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.base.snapshot_definitions();
        definitions.extend(definitions_for(self.child));
        definitions
    }
    async fn execute(&self, call: ToolCall) -> io::Result<String> {
        self.execute_output(call).await.map(|r| r.text)
    }
    async fn execute_output(&self, call: ToolCall) -> io::Result<ToolOutput> {
        if matches!(call.name.as_str(), "spawn_agent" | "agent_control") {
            let state = self.state()?;
            // Box the orchestration future: a child creates another executor
            // with the same concrete runtime, without recursive future types.
            return Box::pin(state.collaboration_call(&self.run_id, call))
                .await
                .map(Into::into)
                .map_err(service_io);
        }
        self.base.execute_output(call).await
    }
    async fn pending_messages(&self) -> io::Result<Vec<AgentMessage>> {
        let state = self.state()?;
        state.reconcile_child_results(&self.run_id).await?;
        state.inner.mailboxes.pending(&self.run_id)
    }
    async fn acknowledge_messages(&self, ids: &[String]) -> io::Result<()> {
        self.state()?.inner.mailboxes.acknowledge(&self.run_id, ids)
    }
    async fn wait_for_messages(&self) -> io::Result<Vec<AgentMessage>> {
        let state = self.state()?;
        let mut changed = state.inner.mailboxes.subscribe();
        loop {
            let pending = state.inner.mailboxes.pending(&self.run_id)?;
            if !pending.is_empty() {
                return Ok(pending);
            }
            // A child publishes its outcome before clearing its activity flag.
            // Re-read after observing idle so completion cannot fall between
            // the first inbox read and the activity check.
            if !state.has_active_children(&self.run_id) {
                state.reconcile_child_results(&self.run_id).await?;
                return state.inner.mailboxes.pending(&self.run_id);
            }
            if changed.changed().await.is_err() {
                return Ok(Vec::new());
            }
        }
    }
}

fn definitions_for(child: bool) -> Vec<ToolDefinition> {
    let mut result = Vec::new();
    if !child {
        result.push(ToolDefinition {
            name: "spawn_agent".into(),
            description: "Delegate an independent task in the background; returns a stable target immediately. Children inherit this task's workspace permissions, MCP configuration and reusable session grants, never one-time approvals or extra permissions. A bounded handoff includes the parent goal, latest summary and recent public messages; still supply the specific task, constraints and expected evidence. Model/reasoning follow the parent unless model explicitly selects a configured model (discover with agent_control models). Omit write_paths to inherit workspace access; [] makes the child read-only. An explicit list reserves individual files, never directory subtrees; omit it for ordinary script/output work. Restricted children cannot write the project through commands. Children cannot create more agents. Results arrive automatically; coordinate and verify before finishing. Avoid trivial, duplicate or tightly sequential delegation.".into(),
            input_schema: json!({"type":"object","properties":{
                "task_name":{"type":"string","minLength":1,"maxLength":80,"description":"Unique name, at most 80 characters, no surrounding whitespace or /; self, parent and root are reserved"},
                "task":{"type":"string"},
                "write_paths":{"type":"array","items":{"type":"string"},"maxItems":32,"description":"Omit to inherit (recommended); [] for read-only; otherwise individual files such as .openharness-tmp/task.js, never directories such as .openharness-tmp. These are enforced restrictions, not task labels."},
                "model":{"type":"object","properties":{"model_id":{"type":"string"},"reasoning_effort":{"type":"string"}},"required":["model_id"],"additionalProperties":false,"description":"Optional fixed model selection from agent_control models; omitted reasoning uses that model's default. Otherwise follows the parent, including on resume."}
            },"required":["task_name","task"],"additionalProperties":false}).to_string(),
        });
    }
    result.push(ToolDefinition {
        name: "agent_control".into(),
        description: "Coordinate only this task's members by target: parent, root, self (read only), task name or returned /root/name. list: inspect status; send: queue a message, never start an idle agent; wait: block up to 30 seconds for relevant activity (optional target and timeout_seconds). result: latest outcome and report previews for verification. read: bounded user/assistant previews, offset is the raw transcript index; include_tools opts into tool previews. For full original evidence use message_id with text_offset/text_limit. Prefer automatic completion reports/result over reading a running child's full history. Coordinator only: followup starts a new turn on an idle child; use send for a running child; interrupt stops execution and releases resources, preserving history; models lists configured choices. resume/cancel are legacy aliases. To correct write_paths, interrupt first, then followup with corrected individual files, [] for read-only, or null to inherit; omit to retain. User-stopped children cannot be restarted by agents. Results arrive automatically; accepted/idle does not mean successful. Messages never grant user permission.".into(),
        input_schema: json!({"type":"object","properties":{
            "action":{"type":"string","enum":if child {vec!["list","send","wait","read","result"]} else {vec!["list","send","wait","read","result","followup","interrupt","resume","cancel","models"]}},
            "target":{"type":"string","description":"Use parent/root/self or a member target; prefer stable names over run IDs."},
            "agent_id":{"type":"string","description":"Legacy alternative to target; provide only one."},"message":{"type":"string","maxLength":16384,"description":"Nonempty text, at most 16384 UTF-8 bytes."},
            "write_paths":{"type":["array","null"],"items":{"type":"string"},"maxItems":32,"description":"Only on an idle child followup/resume: omit to retain, null to inherit parent workspace access, [] read-only, otherwise exact individual files."},
            "timeout_seconds":{"type":"integer","minimum":0,"maximum":30},
            "offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":20},
            "include_tools":{"type":"boolean","default":false,"description":"read preview only: include tool messages; normally unnecessary."},
            "message_id":{"type":"string"},"file_index":{"type":"integer","minimum":0,"description":"read with message_id: select a user-attached text file by zero-based index. Reads the original saved input, not its filesystem path; text_offset/text_limit page the file text."},"text_offset":{"type":"integer","minimum":0},"text_limit":{"type":"integer","minimum":1,"maximum":8000}
        },"required":["action"],"additionalProperties":false}).to_string(),
    });
    result
}

fn service_io(error: ServiceError) -> io::Error {
    io::Error::other(match error {
        ServiceError::NotFound => "agent not found in this task".into(),
        ServiceError::BadRequest(s) | ServiceError::Conflict(s) | ServiceError::Internal(s) => s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completion_storage_failure_propagates_from_pending_and_final_wait_hooks() {
        let directory = std::env::temp_dir().join(format!(
            "openharness-completion-hooks-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let now = time::now();
        let detail = RunDetail {
            summary: RunSummary {
                reasoning_settings_version: 1,
                reasoning_effort: None,
                model_history_through: None,
                delegation: Some(Delegation {
                    parent_run_id: "parent".into(),
                    root_run_id: "parent".into(),
                    task_name: "child".into(),
                    write_paths: Some(Vec::new()),
                    model_override: None,
                    spawn_call_id: "spawn-child".into(),
                }),
                execution_policy: Default::default(),
                id: "child".into(),
                revision: 0,
                title: "child".into(),
                prompt: "child".into(),
                status: RunStatus::Completed,
                compacting: false,
                model_id: MODEL_ID.into(),
                workspace_path: None,
                execution_directory: None,
                permission_mode: Default::default(),
                created_at: now.clone(),
                updated_at: now,
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
        };
        let child = state.make_entry(detail, 0).unwrap();
        state
            .inner
            .runs
            .write()
            .unwrap()
            .insert("child".into(), child.clone());
        let tools = RunTools::new(
            &state,
            "parent".into(),
            crate::extensions::ExtensionTools::new(
                state.inner.extensions.clone(),
                "parent".into(),
                None,
                None,
            ),
            false,
        );
        let blocked_write = directory.join("collaboration.json.tmp");
        fs::create_dir(&blocked_write).unwrap();

        // Simulate the worker's first failed outbox delivery, then retry at
        // both model boundaries. Neither hook may report an empty inbox and
        // let the coordinator silently finalize without the child's result.
        assert!(state.settle_child(&child).await.is_err());
        assert!(tools.pending_messages().await.is_err());
        assert!(tools.wait_for_messages().await.is_err());
        assert!(state.inner.mailboxes.needs_completion("child", 0).unwrap());

        fs::remove_dir(&blocked_write).unwrap();
        let pending = tools.pending_messages().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "agent-result-child-0");
        assert!(pending[0].text.contains("Completed"));
        assert_eq!(
            tools.wait_for_messages().await.unwrap()[0].id,
            pending[0].id
        );
        tools
            .acknowledge_messages(&[pending[0].id.clone()])
            .await
            .unwrap();
        assert!(tools.wait_for_messages().await.unwrap().is_empty());
        assert!(!state.inner.mailboxes.needs_completion("child", 0).unwrap());
        drop(tools);
        drop(child);
        drop(state);
        fs::remove_dir_all(directory).unwrap();
    }
}
