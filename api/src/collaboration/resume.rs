//! Resume a stopped delegate without changing its identity or historical evidence.
use super::admission::AdmissionLease;
use super::*;

impl AppState {
    pub(super) async fn resume_child(
        &self,
        caller: &str,
        id: &str,
        message: String,
        write_paths: Option<Option<Vec<String>>>,
    ) -> Result<(), ServiceError> {
        if message.trim().is_empty() || message.len() > 16 * 1024 {
            return Err(ServiceError::BadRequest(
                "续派内容需为 1–16384 bytes".into(),
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
            // A queued message can arrive after the execution loop's final
            // inbox check. Do not advertise successful follow-up execution for
            // a live turn: `send` explicitly offers queue-only delivery, while
            // followup/resume starts a fully idle child under this admission gate.
            return Err(ServiceError::Conflict(
                "followup 只能继续空闲子任务；当前仍在执行或清理，请用 send 投递消息（不保证已处理），或等待结束后再 followup。修改文件范围须先停止并等待清理完成".into(),
            ));
        }
        if entry
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.commands_running())
        {
            return Err(ServiceError::Conflict(
                "子任务仍有命令在运行，请先停止并等待清理完成".into(),
            ));
        }
        let parent_detail = parent.snapshot.get();
        self.require_capacity(&parent_detail)?;
        let next_paths = write_paths.unwrap_or_else(|| delegation.write_paths.clone());
        if next_paths.as_ref().is_some_and(|paths| paths.len() > 32) {
            return Err(ServiceError::BadRequest(
                "最多指定 32 个精确文件路径".into(),
            ));
        }
        if next_paths.as_ref().is_some_and(|paths| !paths.is_empty())
            && (parent_detail.summary.working_directory().is_none()
                || parent_detail.summary.permission_mode
                    != agent_local::PermissionMode::WorkspaceWrite)
        {
            return Err(ServiceError::BadRequest(
                "当前主任务没有项目写入权限，不能委派文件修改".into(),
            ));
        }
        let permission = if next_paths.as_ref().is_some_and(Vec::is_empty) {
            agent_local::PermissionMode::ReadOnly
        } else {
            parent_detail.summary.permission_mode
        };
        let selection = delegation.model_override.clone().unwrap_or_else(|| {
            crate::contracts::UpdateRunModelInput {
                model_id: parent_detail.summary.model_id.clone(),
                reasoning_effort: parent_detail.summary.reasoning_effort.clone(),
            }
        });
        let settings_version = if delegation.model_override.is_some() {
            1
        } else {
            parent_detail.summary.reasoning_settings_version
        };
        let model_changed = !entry.runtime.model().available()
            || detail.summary.model_id != selection.model_id
            || detail.summary.reasoning_effort != selection.reasoning_effort
            || detail.summary.reasoning_settings_version != settings_version;
        let binding_changed = model_changed
            || detail.summary.permission_mode != permission
            || delegation.write_paths != next_paths;
        // Prepare the replacement without publishing a new permission or model
        // binding. A failed reservation, restore or durable resume keeps the old
        // binding intact. Runtime/snapshot publication occurs only at commit.
        let mut next_detail = detail.clone();
        next_detail.summary.model_id = selection.model_id;
        next_detail.summary.reasoning_effort = selection.reasoning_effort;
        next_detail.summary.reasoning_settings_version = settings_version;
        next_detail.summary.permission_mode = permission;
        next_detail.summary.delegation.as_mut().unwrap().write_paths = next_paths.clone();
        let previous_entry = entry.clone();
        if (next_detail.summary.model_id == MODEL_ID) != (detail.summary.model_id == MODEL_ID) {
            return Err(ServiceError::BadRequest(
                "子任务不能在离线演示和真实模型之间切换".into(),
            ));
        }
        let access = detail
            .summary
            .working_directory()
            .map(|path| self.workspace_access(path))
            .transpose()?;
        if let Some(access) = &access {
            access.try_acquire(id, next_paths.as_deref().unwrap_or_default())?;
        }
        let mut admission = AdmissionLease::new(
            access.clone(),
            id,
            next_paths.as_ref().is_some_and(|paths| !paths.is_empty()),
        );
        let outcome = async {
            let entry = if !binding_changed {
                entry.clone()
            } else {
                if model_changed {
                    self.validate_run_model(
                        &next_detail.summary.model_id,
                        next_detail.summary.reasoning_effort.as_deref(),
                    )?;
                }
                entry
                    .runtime
                    .context_manager()
                    .stop_memory_background()
                    .await;
                let mut prepared = self.make_entry_with_snapshot(
                    next_detail.clone(),
                    0,
                    Some(entry.snapshot.clone()),
                    None,
                    settings_version == 0,
                )?;
                if entry.runtime.model().provider_context_identity()
                    != prepared.runtime.model().provider_context_identity()
                {
                    let run = entry
                        .runtime
                        .restore_run(id)
                        .await
                        .map_err(ServiceError::internal)?
                        .ok_or(ServiceError::NotFound)?;
                    next_detail.summary.model_history_through =
                        run.events().last().map(|e| e.sequence);
                    prepared = self.make_entry_with_snapshot(
                        next_detail.clone(),
                        0,
                        Some(entry.snapshot.clone()),
                        None,
                        settings_version == 0,
                    )?;
                }
                prepared
            };
            Self::require_workspace_available(&entry)?;
            let policy = if model_changed {
                Some(
                    (if next_detail.summary.model_id == MODEL_ID {
                        policy()
                    } else {
                        models::context_policy(entry.runtime.model())?
                    })
                    .resolve(entry.runtime.model().context_metadata())
                    .map_err(ServiceError::internal)?,
                )
            } else {
                None
            };
            self.inner
                .mailboxes
                .send(id, caller, handoff::context(&parent_detail))?;
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
            if binding_changed {
                entry.snapshot.update(|current| {
                    current.summary.model_id = next_detail.summary.model_id;
                    current.summary.reasoning_effort = next_detail.summary.reasoning_effort;
                    current.summary.reasoning_settings_version = settings_version;
                    current.summary.model_history_through =
                        next_detail.summary.model_history_through;
                    current.summary.permission_mode = permission;
                    current.summary.delegation.as_mut().unwrap().write_paths = next_paths;
                    if let Some(policy) = policy {
                        let context = current.context.get_or_insert_with(initial_context);
                        context.window_tokens = policy.model_budget().context_window();
                        context.input_budget = policy.input_token_limit();
                        context.trigger_tokens = policy.compaction_threshold();
                        context.target_tokens = policy.compaction_target();
                        context.reserved_output_tokens =
                            policy.model_budget().output_token_reserve();
                        if let Some(accounting) = context.accounting.take() {
                            context.estimated_input_tokens = accounting.raw_estimated_input_tokens;
                        }
                        context.measurement = "estimated".into();
                    }
                })?;
            }
            Ok::<_, ServiceError>((entry, run))
        }
        .await;
        match outcome {
            Ok((entry, run)) => {
                if binding_changed {
                    self.inner
                        .runs
                        .write()
                        .expect("runs lock poisoned")
                        .insert(id.to_owned(), entry.clone());
                }
                execute(self.clone(), entry.clone(), run);
                admission.commit();
                self.inner.mailboxes.notify();
                Ok(())
            }
            Err(error) => {
                workspace::reconcile_admission_error(&previous_entry, id, &error).await;
                if let Some(access) = &access {
                    access.release(id).await;
                }
                admission.commit();
                Err(error)
            }
        }
    }
}
