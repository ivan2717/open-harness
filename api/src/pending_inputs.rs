use super::*;
use crate::contracts::PendingInputAction;

impl AppState {
    pub async fn update_pending_input(
        &self,
        id: &str,
        action: PendingInputAction,
    ) -> Result<RunDetail, ServiceError> {
        self.reject_direct_child_input(id)?;
        if let PendingInputAction::Enqueue { content, files } = &action {
            crate::input::validate(content, files)?;
            self.validate_stored_attachments(files).await?;
        }
        let _gate = self.inner.model_gate.lock().await;
        self.ensure_accepting_work()?;
        let entry = self.entry(id)?;
        if let PendingInputAction::Enqueue { files, .. } = &action {
            crate::input::validate_images(
                files,
                entry.runtime.model().capabilities().supports_images,
            )?;
        }
        let should_drain = matches!(action, PendingInputAction::Steer { .. });
        let queued_id = new_run_id();
        if let PendingInputAction::Enqueue { files, .. } = &action {
            entry.snapshot.save_input_files(&queued_id, files)?;
        }
        let removed_id = match &action {
            PendingInputAction::Remove { id, .. } => Some(id.clone()),
            _ => None,
        };
        let result = entry.snapshot.try_update(|detail| {
            if detail.summary.compacting {
                return Err(ServiceError::Conflict("请等待上下文整理结束".into()));
            }
            match action {
                PendingInputAction::Enqueue { content, mut files } => {
                    if !matches!(
                        detail.summary.status,
                        RunStatus::Running | RunStatus::WaitingForInput | RunStatus::Completed
                    ) {
                        return Err(ServiceError::Conflict(
                            "任务已经停止，请重新发送以继续".into(),
                        ));
                    }
                    if detail.pending_inputs.len() >= 16 {
                        return Err(ServiceError::BadRequest(
                            "最多暂存 16 条消息，请等待处理或移除一些消息".into(),
                        ));
                    }
                    for file in &mut files {
                        file.text.clear();
                        file.image = None;
                    }
                    detail.pending_inputs.push(agent_runtime::PendingUserInput {
                        id: queued_id.clone(),
                        content,
                        files,
                        steer: false,
                        delivering: false,
                        revision: 0,
                    });
                }
                action => {
                    let (id, revision) = match &action {
                        PendingInputAction::Edit { id, revision, .. }
                        | PendingInputAction::Steer { id, revision }
                        | PendingInputAction::Remove { id, revision } => (id, *revision),
                        PendingInputAction::Enqueue { .. } => unreachable!(),
                    };
                    let index = detail
                        .pending_inputs
                        .iter()
                        .position(|input| &input.id == id)
                        .ok_or_else(|| {
                            ServiceError::Conflict("消息已开始处理，请刷新会话".into())
                        })?;
                    let input = &mut detail.pending_inputs[index];
                    if input.delivering || input.revision != revision {
                        return Err(ServiceError::Conflict(
                            "消息已更新或正在送达，请刷新后重试".into(),
                        ));
                    }
                    match action {
                        PendingInputAction::Edit { content, .. } => {
                            crate::input::validate(&content, &input.files)?;
                            input.content = content;
                            input.steer = false;
                            input.revision += 1;
                        }
                        PendingInputAction::Steer { .. } => {
                            input.steer = true;
                            input.revision += 1;
                            detail.pending_inputs_paused = false;
                        }
                        PendingInputAction::Remove { .. } => {
                            detail.pending_inputs.remove(index);
                        }
                        _ => unreachable!(),
                    }
                }
            }
            Ok::<_, ServiceError>(())
        });
        if let Err(error) = result {
            entry.snapshot.remove_input_files(&queued_id);
            return Err(error);
        }
        if let Some(id) = removed_id {
            entry.snapshot.remove_input_files(&id);
        }
        let snapshot = entry.snapshot.get();
        if !should_drain {
            return Ok(snapshot);
        }
        let state = self.clone();
        let id = id.to_owned();
        // Only an explicit steer may schedule delivery. A single serialized
        // admission owns the next turn if the current execution has just ended.
        tokio::spawn(async move {
            if let Err(error) = state.drain_pending_inputs(&id).await {
                tracing::warn!(%error, run_id = id, "queued input remains available for retry");
                let _gate = state.inner.model_gate.lock().await;
                if let Ok(entry) = state.entry(&id) {
                    let _operation = entry.operations.lock().await;
                    if !entry.background.load(Ordering::Acquire) && !entry.runtime.is_executing(&id)
                    {
                        // Reconcile uncertain delivery before allowing edits/retry.
                        if let Ok(Some(run)) = entry.runtime.restore_run(&id).await {
                            for event in run.events() {
                                let _ = entry.snapshot.event(event);
                            }
                            let _ = entry.snapshot.update(|detail| {
                                detail.pending_inputs_paused = true;
                                for input in &mut detail.pending_inputs {
                                    input.delivering = false;
                                    input.steer = false;
                                }
                                detail.error = Some(format!("消息尚未送达：{error}"));
                            });
                        }
                    }
                }
            }
        });
        Ok(snapshot)
    }

    async fn drain_pending_inputs(&self, id: &str) -> Result<(), ServiceError> {
        let mut changed = self.inner.mailboxes.subscribe();
        loop {
            let entry = self.entry(id)?;
            if !entry.background.load(Ordering::Acquire) && !entry.runtime.is_executing(id) {
                break;
            }
            if changed.changed().await.is_err() {
                return Ok(());
            }
        }
        let _gate = self.inner.model_gate.lock().await;
        self.ensure_accepting_work()?;
        let entry = self.entry(id)?;
        let _operation = entry.operations.lock().await;
        let detail = entry.snapshot.get();
        if entry.background.load(Ordering::Acquire)
            || entry.runtime.is_executing(id)
            || detail.summary.compacting
            || detail.pending_inputs_paused
            || detail.pending_inputs.is_empty()
            || !entry.runtime.model().available()
        {
            return Ok(());
        }
        if !detail.pending_inputs.iter().any(|input| input.steer) {
            return Ok(());
        }
        Self::require_workspace_available(&entry)?;
        if let Some(root) = detail.summary.working_directory() {
            self.ensure_workspace_idle(root, Some(id))?;
        }
        let mut run = entry
            .runtime
            .restore_run(id)
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)?;
        // A failed append can leave a claim behind. Recover against durable
        // delivery IDs before retrying, just as startup replay does.
        entry.snapshot.update(|detail| {
            for input in &mut detail.pending_inputs {
                input.delivering = false;
            }
        })?;
        if entry
            .runtime
            .start_queued_turn(&mut run)
            .await
            .map_err(ServiceError::internal)?
        {
            execute(self.clone(), entry.clone(), run);
        }
        Ok(())
    }
}
