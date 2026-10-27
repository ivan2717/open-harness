use super::*;

impl AppState {
    pub(super) async fn control_agent(
        &self,
        caller: &str,
        input: ControlInput,
    ) -> Result<String, ServiceError> {
        let root = self.root_id(caller)?;
        if !Self::active(self.entry(caller)?.as_ref()) {
            return Err(ServiceError::Conflict("发送者已停止".into()));
        }
        if input.write_paths.is_some() && !matches!(input.action.as_str(), "resume" | "followup") {
            return Err(ServiceError::BadRequest(
                "只有续派可以调整 write_paths；运行中的任务需先停止".into(),
            ));
        }
        if input.include_reports.is_some() && input.action != "result" {
            return Err(ServiceError::BadRequest(
                "include_reports 仅适用于 result".into(),
            ));
        }
        if input.action == "models" {
            self.require_coordinator(caller)?;
            let models: Vec<_> = self
                .config()
                .models
                .into_iter()
                .filter(|model| model.available && model.id != MODEL_ID)
                .collect();
            return Ok(json!({"models":models,"default":"Inherit the parent model and reasoning; override only when appropriate for the delegated task."}).to_string());
        }
        if matches!(input.action.as_str(), "list" | "wait") {
            if input.action == "wait" {
                if input.message.is_some()
                    || input.offset.is_some()
                    || input.limit.is_some()
                    || input.message_id.is_some()
                    || input.file_index.is_some()
                    || input.text_offset.is_some()
                    || input.text_limit.is_some()
                    || input.include_tools.is_some()
                {
                    return Err(ServiceError::BadRequest(
                        "wait 只接受 action、可选 target 和 timeout_seconds".into(),
                    ));
                }
                let target = input
                    .agent_id
                    .as_deref()
                    .map(|address| self.resolve_agent_target(caller, address))
                    .transpose()?;
                if target.as_deref() == Some(caller) {
                    return Err(ServiceError::BadRequest(
                        "不能等待自己；请选择其他成员".into(),
                    ));
                }
                return self
                    .wait_for_target_activity(
                        caller,
                        &root,
                        input.timeout_seconds.unwrap_or(30),
                        target.as_deref(),
                    )
                    .await;
            }
            return Ok(self.member_view(&root));
        }
        let address = input.agent_id.as_deref().ok_or_else(|| {
            ServiceError::BadRequest("需要 target（parent、任务名或 list 返回的地址）".into())
        })?;
        let resolved = self.resolve_agent_target(caller, address)?;
        let target_id = resolved.as_str();
        if target_id == caller && !matches!(input.action.as_str(), "read" | "result") {
            return Err(ServiceError::BadRequest("只能操作本任务的其他成员".into()));
        }
        let target = self.entry(target_id)?;
        match input.action.as_str() {
            "send" => {
                let text = input
                    .message
                    .ok_or_else(|| ServiceError::BadRequest("需要 message".into()))?;
                let message_id = self.inner.mailboxes.send(target_id, caller, text)?;
                Ok(
                    json!({"message_id":message_id,"target":identity::target(&target.snapshot.get().summary),"accepted":true,"starts_idle_agent":false,"active":Self::active(&target),"notice":"Accepted into the inbox, not proof of processing or task completion. Use followup to start an idle child."})
                        .to_string(),
                )
            }
            "read" => self.read_agent_transcript(target_id, &input).await,
            "result" => self.agent_result(target_id, input.include_reports.unwrap_or(true)),
            "resume" | "followup" => {
                let message = input
                    .message
                    .ok_or_else(|| ServiceError::BadRequest("续派需要明确的 message".into()))?;
                let state = self.clone();
                let owner = caller.to_owned();
                let target_id_owned = target_id.to_owned();
                tokio::spawn(async move {
                    state
                        .resume_child(&owner, &target_id_owned, message, input.write_paths)
                        .await
                })
                .await
                .map_err(ServiceError::internal)??;
                Ok(json!({"agent_id":target_id,"target":identity::target(&target.snapshot.get().summary),"accepted":true,"status":"running","notice":"Task accepted for execution; collect its result before treating it as complete."}).to_string())
            }
            "cancel" | "interrupt" => {
                self.require_coordinator(caller)?;
                if target
                    .snapshot
                    .get()
                    .summary
                    .delegation
                    .as_ref()
                    .is_none_or(|d| d.parent_run_id != caller)
                {
                    return Err(ServiceError::BadRequest("只能停止直接子任务".into()));
                }
                self.cancel_member(target_id, false).await?;
                Ok(json!({"agent_id":target_id,"stopped":true,"notice":"Current execution stopped and resources released. History and identity are retained; followup can continue this child."}).to_string())
            }
            _ => Err(ServiceError::BadRequest("未知协作操作".into())),
        }
    }

    pub(super) fn member_view(&self, root: &str) -> String {
        let members: Vec<_> = self.members(root).iter().map(|entry| {
            let detail = entry.snapshot.get();
            json!({"agent_id":detail.summary.id,"target":identity::target(&detail.summary),"task_name":detail.summary.delegation.as_ref().map_or(detail.summary.title.as_str(), |d| d.task_name.as_str()),"status":detail.summary.status,"active":Self::active(entry),
                "model_id":detail.summary.model_id,"reasoning_effort":detail.summary.reasoning_effort,
                "permission_mode":detail.summary.permission_mode,
                "delegation":detail.summary.delegation,"observed_usage":detail.context.and_then(|c| c.observed_usage)})
        }).collect();
        json!({"members":members,"notice":"Each member has an independent context. active=false means no execution is running, not that its task succeeded; inspect the result and status. observed_usage is cumulative, not context size."}).to_string()
    }
}
