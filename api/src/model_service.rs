use super::*;

pub(super) fn context_policy(model: &impl Model) -> io::Result<ContextPolicy> {
    match model.context_metadata() {
        Some(metadata) => ContextPolicy::builder()
            .model_context_window(metadata.context_window)
            .output_token_reserve(metadata.max_output_tokens.unwrap_or(8192))
            .safety_margin((metadata.context_window / 100).clamp(64, 2048))
            .build()
            .map_err(io::Error::other),
        None => Ok(ContextPolicy::default()),
    }
}

impl AppState {
    /// Move idle conversations to a separately registered configuration without
    /// changing the provider's replay identity or rewriting their event history.
    /// The desktop must persist both configurations before calling this method.
    pub async fn replace_model(
        &self,
        id: &str,
        replacement_id: &str,
    ) -> Result<ModelInfo, ServiceError> {
        if !valid_id(id) || !valid_id(replacement_id) {
            return Err(ServiceError::NotFound);
        }
        if id == MODEL_ID || replacement_id == MODEL_ID {
            return Err(ServiceError::BadRequest("离线演示模型不能替换".into()));
        }
        let _gate = self.inner.model_gate.lock().await;
        self.ensure_accepting_work()?;
        let (previous, replacement) = {
            let models = self.inner.models.read().expect("models lock poisoned");
            (
                models.get(id).cloned(),
                models
                    .get(replacement_id)
                    .cloned()
                    .ok_or(ServiceError::NotFound)?,
            )
        };
        let Some(previous) = previous else {
            // A successful response may have been lost after all bindings
            // moved. A retry is safe only when that postcondition still holds.
            let still_referenced = self
                .inner
                .runs
                .read()
                .expect("runs lock poisoned")
                .values()
                .any(|entry| entry.snapshot.get().summary.model_id == id);
            return if still_referenced {
                Err(ServiceError::NotFound)
            } else {
                Ok(replacement.info)
            };
        };
        if id == replacement_id {
            return Ok(replacement.info);
        }
        if previous.model.provider_context_identity()
            != replacement.model.provider_context_identity()
        {
            return Err(ServiceError::BadRequest(
                "更新容量须保持相同的模型、服务地址、协议和思考设置".into(),
            ));
        }
        let policy = context_policy(&replacement.model)?
            .resolve(replacement.model.context_metadata())
            .map_err(ServiceError::internal)?;
        let mut entries: Vec<_> = self
            .inner
            .runs
            .read()
            .expect("runs lock poisoned")
            .values()
            .filter(|entry| entry.snapshot.get().summary.model_id == id)
            .cloned()
            .collect();
        entries.sort_by_key(|entry| entry.snapshot.get().summary.id);
        // Check every affected conversation before modifying any binding. All
        // execution admissions use model_gate, including delegated work.
        for entry in &entries {
            let detail = entry.snapshot.get();
            if entry.background.load(Ordering::Acquire)
                || entry.runtime.is_executing(&detail.summary.id)
                || detail.summary.compacting
                || detail.summary.status == RunStatus::Running
            {
                return Err(ServiceError::Conflict(
                    "此模型仍有运行或压缩任务，请等待结束后更新容量".into(),
                ));
            }
        }
        for entry in entries {
            entry
                .runtime
                .context_manager()
                .stop_memory_background()
                .await;
            let mut detail = entry.snapshot.get();
            let run_id = detail.summary.id.clone();
            detail.summary.model_id = replacement_id.to_owned();
            let restored = self.make_entry_with_snapshot(
                detail,
                0,
                Some(entry.snapshot.clone()),
                Some(&replacement.model),
                true,
            )?;
            // Preserve the Snapshot and its live subscriptions. Persist the
            // binding before exposing the new runtime. On failure, both model
            // configurations remain available for earlier and remaining runs.
            entry.snapshot.update(|detail| {
                detail.summary.model_id = replacement_id.to_owned();
                let context = detail.context.get_or_insert_with(initial_context);
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
                .insert(run_id, restored);
        }
        self.inner
            .models
            .write()
            .expect("models lock poisoned")
            .remove(id);
        Ok(replacement.info)
    }
}

#[cfg(test)]
#[path = "model_service_tests.rs"]
mod tests;
