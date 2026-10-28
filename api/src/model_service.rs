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
        self.replace_model_configuration(id, replacement_id, false)
            .await
    }

    /// Apply an explicit saved-profile edit to every idle binding. When the
    /// provider contract changes, retain portable history but stop replaying
    /// opaque state from the previous model, just like switching a single run.
    pub async fn replace_model_configuration(
        &self,
        id: &str,
        replacement_id: &str,
        allow_configuration_change: bool,
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
                .any(|entry| references_model(&entry.snapshot.get(), id));
            return if still_referenced {
                Err(ServiceError::NotFound)
            } else {
                Ok(replacement.info)
            };
        };
        if id == replacement_id {
            return Ok(replacement.info);
        }
        if !allow_configuration_change
            && previous.model.provider_context_identity()
                != replacement.model.provider_context_identity()
        {
            return Err(ServiceError::BadRequest(
                "更新容量须保持相同的模型、服务地址、协议和思考设置".into(),
            ));
        }
        let mut entries: Vec<_> = self
            .inner
            .runs
            .read()
            .expect("runs lock poisoned")
            .values()
            .filter(|entry| references_model(&entry.snapshot.get(), id))
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
                || entry
                    .workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.commands_running())
            {
                return Err(ServiceError::Conflict(
                    "此模型仍有运行、压缩或命令任务，请等待结束后更新配置".into(),
                ));
            }
        }
        let update_binding = |detail: &mut RunDetail| {
            if detail.summary.model_id == id {
                detail.summary.model_id = replacement_id.to_owned();
                if allow_configuration_change {
                    retain_supported_effort(
                        &replacement.model,
                        &mut detail.summary.reasoning_effort,
                    );
                    detail.summary.reasoning_settings_version = 1;
                }
            }
            // Explicit child selections are reused on every follow-up. Migrate
            // the durable pin together with its current runtime binding.
            if let Some(selection) = detail
                .summary
                .delegation
                .as_mut()
                .and_then(|delegation| delegation.model_override.as_mut())
                .filter(|selection| selection.model_id == id)
            {
                selection.model_id = replacement_id.to_owned();
                if allow_configuration_change {
                    retain_supported_effort(&replacement.model, &mut selection.reasoning_effort);
                }
            }
        };
        for entry in entries {
            entry
                .runtime
                .context_manager()
                .stop_memory_background()
                .await;
            let mut detail = entry.snapshot.get();
            let run_id = detail.summary.id.clone();
            let rebind = detail.summary.model_id == id;
            update_binding(&mut detail);
            let replacement = if !rebind {
                // A child can retain an explicit future selection while its
                // current runtime uses another model. Migrate only that pin.
                None
            } else {
                if allow_configuration_change {
                    let effective = match detail.summary.reasoning_effort.as_deref() {
                        Some(effort) => replacement.model.with_reasoning_effort(effort),
                        None => Ok(replacement.model.clone()),
                    }
                    .map_err(ServiceError::internal)?;
                    if entry.runtime.model().provider_context_identity()
                        != effective.provider_context_identity()
                    {
                        let run = entry
                            .runtime
                            .restore_run(&run_id)
                            .await
                            .map_err(ServiceError::internal)?
                            .ok_or(ServiceError::NotFound)?;
                        detail.summary.model_history_through =
                            run.events().last().map(|e| e.sequence);
                    }
                }
                let restored = self.make_entry_with_snapshot(
                    detail.clone(),
                    0,
                    Some(entry.snapshot.clone()),
                    Some(&replacement.model),
                    !allow_configuration_change,
                )?;
                let policy = context_policy(restored.runtime.model())?
                    .resolve(restored.runtime.model().context_metadata())
                    .map_err(ServiceError::internal)?;
                Some((restored, policy))
            };
            // Preserve the Snapshot and its live subscriptions. Persist the
            // binding before exposing the new runtime. On failure, both model
            // configurations remain available for earlier and remaining runs.
            entry.snapshot.update(|current| {
                update_binding(current);
                let Some((_, policy)) = &replacement else {
                    return;
                };
                current.summary.reasoning_effort = detail.summary.reasoning_effort.clone();
                current.summary.reasoning_settings_version =
                    detail.summary.reasoning_settings_version;
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
            if let Some((restored, _)) = replacement {
                self.inner
                    .runs
                    .write()
                    .expect("runs lock poisoned")
                    .insert(detail.summary.id, restored);
            }
        }
        self.inner
            .models
            .write()
            .expect("models lock poisoned")
            .remove(id);
        Ok(replacement.info)
    }
}

pub(super) fn references_model(detail: &RunDetail, id: &str) -> bool {
    detail.summary.model_id == id
        || detail
            .summary
            .delegation
            .as_ref()
            .and_then(|delegation| delegation.model_override.as_ref())
            .is_some_and(|selection| selection.model_id == id)
}

fn retain_supported_effort(model: &agent_providers::HttpModel, effort: &mut Option<String>) {
    if effort
        .as_deref()
        .is_some_and(|effort| model.with_reasoning_effort(effort).is_err())
    {
        // None selects the replacement profile's configured default. Never
        // send a stale effort value to a different provider or a reduced budget.
        *effort = None;
    }
}

#[cfg(test)]
#[path = "model_service_tests.rs"]
mod tests;
