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
            || input.task_name.chars().count() > 80
            || input.task_name.chars().any(char::is_control)
            || input.task.trim().is_empty()
            || input.task.len() > 32 * 1024
            || input.write_paths.len() > 32
        {
            return Err(ServiceError::BadRequest(
                "子任务需有简短名称、完整任务描述（最多 32 KiB）以及最多 32 个精确文件路径".into(),
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
        if !input.write_paths.is_empty()
            && (root.summary.workspace_path.is_none()
                || root.summary.permission_mode != agent_local::PermissionMode::WorkspaceWrite)
        {
            return Err(ServiceError::BadRequest(
                "当前主任务没有项目写入权限，不能委派文件修改".into(),
            ));
        }
        let id = new_run_id();
        let now = time::now();
        let detail = RunDetail {
            summary: RunSummary {
                delegation: Some(Delegation {
                    parent_run_id: caller.into(),
                    root_run_id: caller.into(),
                    task_name: input.task_name.clone(),
                    write_paths: input.write_paths.clone(),
                    spawn_call_id: call_id.into(),
                }),
                execution_policy: root.summary.execution_policy.clone(),
                id: id.clone(),
                revision: 0,
                title: input.task_name,
                prompt: input.task.clone(),
                status: RunStatus::Running,
                compacting: false,
                model_id: root.summary.model_id,
                workspace_path: root.summary.workspace_path.clone(),
                permission_mode: if input.write_paths.is_empty() {
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
        let entry = self.make_entry(detail, 0)?;
        Self::require_workspace_available(&entry)?;
        let access = root
            .summary
            .workspace_path
            .as_ref()
            .map(|path| self.workspace_access(path))
            .transpose()?;
        if let Some(access) = &access {
            access.try_acquire(&id, &input.write_paths)?;
        }
        let mut admission = AdmissionLease::new(access.clone(), &id, !input.write_paths.is_empty());
        // From here until execute there are no unbounded waits. Roll back leases
        // on persistence failure; cancellation of a spawn is reconciled below.
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

    pub(super) async fn resume_child(
        &self,
        caller: &str,
        id: &str,
        message: String,
    ) -> Result<(), ServiceError> {
        if message.trim().is_empty() || message.len() > 32 * 1024 {
            return Err(ServiceError::BadRequest(
                "续派内容需为 1–32768 bytes".into(),
            ));
        }
        let _gate = self.inner.model_gate.lock().await;
        let parent = self.require_coordinator(caller)?;
        let entry = self.entry(id)?;
        let _operation = entry.operations.lock().await;
        let detail = entry.snapshot.get();
        let delegation = detail
            .summary
            .delegation
            .as_ref()
            .filter(|d| d.parent_run_id == caller)
            .ok_or_else(|| ServiceError::BadRequest("只能续派直接子任务".into()))?;
        if self.inner.mailboxes.was_stopped_by_user(id) {
            return Err(ServiceError::Conflict(
                "用户已停止此子任务，agent 不能自动重新启动它".into(),
            ));
        }
        if Self::active(&entry) || detail.summary.compacting {
            return Err(ServiceError::Conflict(
                "子任务仍在执行，请发送消息调整方向".into(),
            ));
        }
        if !entry.runtime.model().available() {
            return Err(ServiceError::BadRequest("请重新配置此模型".into()));
        }
        Self::require_workspace_available(&entry)?;
        self.require_capacity(&parent.snapshot.get())?;
        let access = detail
            .summary
            .workspace_path
            .as_ref()
            .map(|path| self.workspace_access(path))
            .transpose()?;
        if let Some(access) = &access {
            access.try_acquire(id, &delegation.write_paths)?;
        }
        let mut admission =
            AdmissionLease::new(access.clone(), id, !delegation.write_paths.is_empty());
        let outcome = async {
            let mut run = entry
                .runtime
                .restore_run(id)
                .await
                .map_err(ServiceError::internal)?
                .ok_or(ServiceError::NotFound)?;
            if run.status() == CoreStatus::WaitingForInput {
                entry
                    .runtime
                    .submit_input(&mut run, message)
                    .await
                    .map_err(ServiceError::internal)?;
            } else {
                entry
                    .runtime
                    .resume_run(&mut run, message)
                    .await
                    .map_err(ServiceError::internal)?;
            }
            for event in run.events() {
                entry.snapshot.event(event)?;
            }
            Ok::<_, ServiceError>(run)
        }
        .await;
        match outcome {
            Ok(run) => {
                execute(self.clone(), entry.clone(), run);
                admission.commit();
                self.inner.mailboxes.notify();
                Ok(())
            }
            Err(error) => {
                workspace::reconcile_admission_error(&entry, id, &error).await;
                if let Some(access) = &access {
                    access.release(id).await;
                }
                admission.commit();
                Err(error)
            }
        }
    }
}

/// Dropping a cancelled admission releases only its reserved files; it never
/// starts or repeats a model/tool operation. Once scheduled, execution owns it.
struct AdmissionLease {
    access: Option<agent_local::WorkspaceAccess>,
    owner: String,
}
impl AdmissionLease {
    fn new(access: Option<agent_local::WorkspaceAccess>, owner: &str, writes: bool) -> Self {
        Self {
            access: access.filter(|_| writes),
            owner: owner.into(),
        }
    }
    fn commit(&mut self) {
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
