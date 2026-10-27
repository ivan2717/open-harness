use super::*;
use crate::contracts::UpdateRunModelInput;

impl AppState {
    pub(super) fn validate_run_model(
        &self,
        id: &str,
        effort: Option<&str>,
    ) -> Result<(), ServiceError> {
        if id == MODEL_ID {
            return if effort.is_some() {
                Err(ServiceError::BadRequest(
                    "离线演示不支持设置思考强度".into(),
                ))
            } else {
                Ok(())
            };
        }
        let models = self.inner.models.read().expect("models lock poisoned");
        let model = models
            .get(id)
            .ok_or_else(|| ServiceError::BadRequest("请先配置此模型".into()))?;
        if let Some(effort) = effort {
            model.model.with_reasoning_effort(effort).map_err(|_| {
                ServiceError::BadRequest(
                    "此思考强度不适用于当前模型配置，请使用可选档位或提高最大输出预算".into(),
                )
            })?;
        }
        Ok(())
    }

    /// A conversation binding is independent from saved model configurations.
    /// Switch only between turns; keep the same durable history and subscriptions.
    pub async fn update_run_model(
        &self,
        id: &str,
        input: UpdateRunModelInput,
    ) -> Result<RunDetail, ServiceError> {
        let _gate = self.inner.model_gate.lock().await;
        self.ensure_accepting_work()?;
        let entry = self.entry(id)?;
        let _operation = entry.operations.lock().await;
        if entry.snapshot.get().summary.delegation.is_some() {
            return Err(ServiceError::Conflict(
                "请在主任务中设置模型与思考强度".into(),
            ));
        }
        let rebound = self.rebind_idle_model(&entry, input).await?;
        Ok(rebound.snapshot.get())
    }

    /// Rebind an idle root or delegated run without changing its history or
    /// subscriptions. The caller holds model_gate and entry.operations, so a
    /// resume cannot race this transition. Build and validate the replacement
    /// before committing its binding to the existing durable snapshot.
    pub(super) async fn rebind_idle_model(
        &self,
        entry: &Arc<RunEntry>,
        input: UpdateRunModelInput,
    ) -> Result<Arc<RunEntry>, ServiceError> {
        self.ensure_accepting_work()?;
        let mut detail = entry.snapshot.get();
        let id = detail.summary.id.clone();
        if entry.background.load(Ordering::Acquire)
            || entry.runtime.is_executing(&id)
            || detail.summary.compacting
            || detail.summary.status == RunStatus::Running
            || entry
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.commands_running())
        {
            return Err(ServiceError::Conflict(
                "请等待当前回复、压缩或命令完成，或停止运行后再切换模型".into(),
            ));
        }
        if (input.model_id == MODEL_ID) != (detail.summary.model_id == MODEL_ID) {
            return Err(ServiceError::BadRequest(
                "离线演示与真实模型的会话不能互相切换，请新建会话".into(),
            ));
        }
        self.validate_run_model(&input.model_id, input.reasoning_effort.as_deref())?;
        if detail.summary.model_id == input.model_id
            && detail.summary.reasoning_effort == input.reasoning_effort
            && detail.summary.reasoning_settings_version == 1
        {
            return Ok(entry.clone());
        }
        let configured = self
            .inner
            .models
            .read()
            .expect("models lock poisoned")
            .get(&input.model_id)
            .cloned()
            .ok_or(ServiceError::NotFound)?;
        let effective = match input.reasoning_effort.as_deref() {
            Some(effort) => configured.model.with_reasoning_effort(effort),
            None => Ok(configured.model.clone()),
        }
        .map_err(ServiceError::internal)?;
        if entry.runtime.model().provider_context_identity()
            != effective.provider_context_identity()
        {
            let run = entry
                .runtime
                .restore_run(&id)
                .await
                .map_err(ServiceError::internal)?
                .ok_or(ServiceError::NotFound)?;
            detail.summary.model_history_through = run.events().last().map(|e| e.sequence);
        }
        detail.summary.model_id = input.model_id;
        detail.summary.reasoning_effort = input.reasoning_effort;
        detail.summary.reasoning_settings_version = 1;
        entry
            .runtime
            .context_manager()
            .stop_memory_background()
            .await;
        let restored = self.make_entry_with_snapshot(
            detail.clone(),
            0,
            Some(entry.snapshot.clone()),
            Some(&configured.model),
            false,
        )?;
        let policy = models::context_policy(restored.runtime.model())?
            .resolve(restored.runtime.model().context_metadata())
            .map_err(ServiceError::internal)?;
        // Snapshot.update writes atomically before replacing the live value. A
        // failure keeps the old binding/runtime and remains safely retryable.
        entry.snapshot.update(|current| {
            current.summary.model_id = detail.summary.model_id;
            current.summary.reasoning_effort = detail.summary.reasoning_effort;
            current.summary.reasoning_settings_version = detail.summary.reasoning_settings_version;
            current.summary.model_history_through = detail.summary.model_history_through;
            let context = current.context.get_or_insert_with(initial_context);
            context.window_tokens = policy.model_budget().context_window();
            context.input_budget = policy.input_token_limit();
            context.trigger_tokens = policy.compaction_threshold();
            context.target_tokens = policy.compaction_target();
            context.reserved_output_tokens = policy.model_budget().output_token_reserve();
            if let Some(accounting) = context.accounting.take() {
                context.estimated_input_tokens = accounting.raw_estimated_input_tokens;
            }
            context.measurement = "estimated".into();
        })?;
        self.inner
            .runs
            .write()
            .expect("runs lock poisoned")
            .insert(id, restored.clone());
        Ok(restored)
    }
}

#[cfg(test)]
#[path = "run_model_tests.rs"]
mod tests;
