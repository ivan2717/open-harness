use super::*;

impl AppState {
    pub(super) async fn spawn_child(
        &self,
        caller: &str,
        call_id: &str,
        input: SpawnInput,
    ) -> Result<String, ServiceError> {
        let _gate = self.inner.model_gate.lock().await;
        let parent = self.require_coordinator(caller)?;
        let root = parent.snapshot.get();
        // A tool result may fail to commit after admission. Never create a
        // second child for the same durable parent tool call.
        for member in self.members(caller) {
            let summary = member.snapshot.get().summary;
            if summary
                .delegation
                .as_ref()
                .is_some_and(|d| d.spawn_call_id == call_id)
            {
                return Ok(summary.id);
            }
        }
        if input.task_name.trim().is_empty()
            || input.task_name.trim() != input.task_name
            || input.task_name.contains('/')
            || matches!(input.task_name.as_str(), "self" | "parent" | "root")
            || input.task_name.chars().count() > 80
            || input.task_name.chars().any(char::is_control)
            || input.task.trim().is_empty()
            || input.task.len() > 32 * 1024
            || input
                .write_paths
                .as_ref()
                .is_some_and(|paths| paths.len() > 32)
        {
            return Err(ServiceError::BadRequest(
                "子任务名称须为 1–80 字符、无首尾空白和 /，不能使用 self、parent、root；任务描述最多 32 KiB，最多 32 个精确文件路径".into(),
            ));
        }
        if self.members(caller).iter().any(|entry| {
            entry
                .snapshot
                .get()
                .summary
                .delegation
                .as_ref()
                .is_some_and(|d| d.task_name == input.task_name)
        }) {
            return Err(ServiceError::Conflict(
                "此名称的子任务已存在，请使用 agent_control 查看或续派".into(),
            ));
        }
        self.require_capacity(&root)?;
        let write_paths = input.write_paths.as_deref().unwrap_or_default();
        if !write_paths.is_empty()
            && (root.summary.working_directory().is_none()
                || root.summary.permission_mode != agent_local::PermissionMode::WorkspaceWrite)
        {
            return Err(ServiceError::BadRequest(
                "当前主任务没有项目写入权限，不能委派文件修改".into(),
            ));
        }
        let selection =
            input
                .model
                .clone()
                .unwrap_or_else(|| crate::contracts::UpdateRunModelInput {
                    model_id: root.summary.model_id.clone(),
                    reasoning_effort: root.summary.reasoning_effort.clone(),
                });
        if input.model.is_some() {
            if (selection.model_id == MODEL_ID) != (root.summary.model_id == MODEL_ID) {
                return Err(ServiceError::BadRequest(
                    "子任务不能在离线演示和真实模型之间切换".into(),
                ));
            }
            self.validate_run_model(&selection.model_id, selection.reasoning_effort.as_deref())?;
        }
        let id = new_run_id();
        let now = time::now();
        let detail = RunDetail {
            summary: RunSummary {
                reasoning_settings_version: if input.model.is_some() {
                    1
                } else {
                    root.summary.reasoning_settings_version
                },
                reasoning_effort: selection.reasoning_effort,
                model_history_through: None,
                delegation: Some(Delegation {
                    parent_run_id: caller.into(),
                    root_run_id: caller.into(),
                    task_name: input.task_name.clone(),
                    write_paths: input.write_paths.clone(),
                    model_override: input.model.clone(),
                    spawn_call_id: call_id.into(),
                }),
                execution_policy: root.summary.execution_policy.clone(),
                id: id.clone(),
                revision: 0,
                title: input.task_name,
                prompt: input.task.clone(),
                status: RunStatus::Running,
                compacting: false,
                model_id: selection.model_id,
                workspace_path: root.summary.workspace_path.clone(),
                execution_directory: root.summary.execution_directory.clone(),
                permission_mode: if input.write_paths.as_ref().is_some_and(Vec::is_empty) {
                    agent_local::PermissionMode::ReadOnly
                } else {
                    root.summary.permission_mode
                },
                created_at: now.clone(),
                updated_at: now,
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
        };
        let entry = self.make_entry_with_snapshot(
            detail,
            0,
            None,
            None,
            input.model.is_none() && root.summary.reasoning_settings_version == 0,
        )?;
        Self::require_workspace_available(&entry)?;
        let access = root
            .summary
            .working_directory()
            .map(|path| self.workspace_access(path))
            .transpose()?;
        if let Some(access) = &access {
            access.try_acquire(&id, write_paths)?;
        }
        let mut admission = AdmissionLease::new(access.clone(), &id, !write_paths.is_empty());
        // From here until execute there are no unbounded waits. Roll back leases
        // on persistence failure; cancellation of a spawn is reconciled below.
        // Queue before starting the persisted run: a failed handoff cannot
        // leave an admitted child executing without the parent's constraints.
        self.inner
            .mailboxes
            .send(&id, caller, handoff::context(&root))?;
        let run_result = entry.runtime.start_run(&entry.agent, &id, input.task).await;
        let run = match run_result {
            Ok(run) => run,
            Err(error) => {
                if let Some(access) = &access {
                    access.release(&id).await;
                }
                admission.commit();
                let _ = entry.snapshot.update(|detail| {
                    detail.summary.status = RunStatus::Interrupted;
                    detail.error = Some(error.to_string());
                });
                return Err(ServiceError::internal(error));
            }
        };
        if let Err(error) = entry.snapshot.event(&run.events()[0]) {
            if let Some(access) = &access {
                access.release(&id).await;
            }
            admission.commit();
            return Err(error.into());
        }
        self.inner
            .runs
            .write()
            .expect("runs lock poisoned")
            .insert(id.clone(), entry.clone());
        self.inner.mailboxes.notify();
        execute(self.clone(), entry, run);
        admission.commit();
        Ok(id)
    }
}

/// Dropping a cancelled admission releases only its reserved files; it never
/// starts or repeats a model/tool operation. Once scheduled, execution owns it.
pub(super) struct AdmissionLease {
    access: Option<agent_local::WorkspaceAccess>,
    owner: String,
}
impl AdmissionLease {
    pub(super) fn new(
        access: Option<agent_local::WorkspaceAccess>,
        owner: &str,
        writes: bool,
    ) -> Self {
        Self {
            access: access.filter(|_| writes),
            owner: owner.into(),
        }
    }
    pub(super) fn commit(&mut self) {
        self.access = None;
    }
}
impl Drop for AdmissionLease {
    fn drop(&mut self) {
        if let Some(access) = self.access.take() {
            let owner = self.owner.clone();
            tokio::spawn(async move {
                access.release(&owner).await;
            });
        }
    }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod scope_tests;
