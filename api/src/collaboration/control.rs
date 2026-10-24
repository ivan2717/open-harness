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
        if matches!(input.action.as_str(), "list" | "wait") {
            if input.action == "wait" {
                let seconds = input.timeout_seconds.unwrap_or(30);
                if seconds > 30 {
                    return Err(ServiceError::BadRequest("等待最多 30 秒".into()));
                }
                let mut changed = self.inner.mailboxes.subscribe();
                if self.inner.mailboxes.pending(caller)?.is_empty()
                    && self.has_active_children(&root)
                {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(seconds), changed.changed()).await;
                }
            }
            return Ok(self.member_view(&root));
        }
        let target_id = input
            .agent_id
            .as_deref()
            .ok_or_else(|| ServiceError::BadRequest("需要 agent_id".into()))?;
        if self.root_id(target_id)? != root || target_id == caller {
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
                    json!({"message_id":message_id,"accepted":true,"starts_idle_agent":false})
                        .to_string(),
                )
            }
            "read" => {
                let detail = target.snapshot.get();
                let offset = input.offset.unwrap_or(0);
                let limit = input.limit.unwrap_or(5);
                if !(1..=20).contains(&limit) {
                    return Err(ServiceError::BadRequest("limit 必须在 1–20 之间".into()));
                }
                let messages: Vec<_> = detail.messages.iter().skip(offset).take(limit).map(|m| json!({"id":m.id,"role":m.role,"tool_name":m.tool_name,"text":bounded(&m.text, 4000)})).collect();
                let end = offset.saturating_add(messages.len());
                Ok(json!({"agent_id":target_id,"messages":messages,"next_offset":if end < detail.messages.len() { Some(end) } else { None }}).to_string())
            }
            "resume" => {
                let message = input
                    .message
                    .ok_or_else(|| ServiceError::BadRequest("续派需要明确的 message".into()))?;
                let state = self.clone();
                let owner = caller.to_owned();
                let target_id_owned = target_id.to_owned();
                tokio::spawn(
                    async move { state.resume_child(&owner, &target_id_owned, message).await },
                )
                .await
                .map_err(ServiceError::internal)??;
                Ok(json!({"agent_id":target_id,"status":"running"}).to_string())
            }
            "cancel" => {
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
                Ok(json!({"agent_id":target_id,"stopped":true}).to_string())
            }
            _ => Err(ServiceError::BadRequest("未知协作操作".into())),
        }
    }

    pub(super) fn member_view(&self, root: &str) -> String {
        let members: Vec<_> = self.members(root).iter().map(|entry| {
            let detail = entry.snapshot.get();
            json!({"agent_id":detail.summary.id,"task_name":detail.summary.title,"status":detail.summary.status,
                "write_paths":detail.summary.delegation.map(|d| d.write_paths),"observed_usage":detail.context.and_then(|c| c.observed_usage)})
        }).collect();
        json!({"members":members,"notice":"Each member has an independent context; observed usage is cumulative usage, not context size. Missing usage/failed attempts are not estimated."}).to_string()
    }
}
