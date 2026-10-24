use crate::{
    context::initial_context,
    contracts::{
        ChatMessage, FileReferenceInfo, ObservedTokenUsage, RunDetail, RunStatus, TimelineEvent,
    },
    time,
};
use agent_core::TextFileReference;
use agent_core::agent::{RunEvent, RunEventEnvelope};
use agent_runtime::ContentBlock;

/// Project only public canonical content; opaque provider state never enters this DTO.
pub fn apply(detail: &mut RunDetail, envelope: &RunEventEnvelope) {
    if detail
        .events
        .last()
        .is_some_and(|last| last.sequence >= envelope.sequence)
    {
        return;
    }
    let id = format!("event-{}", envelope.sequence);
    let (kind, label, text) = match &envelope.event {
        RunEvent::RunStarted { instruction, files } => {
            // The public snapshot may already contain totals when reopening.
            // Rebuild them from durable samples exactly once during replay.
            if let Some(context) = &mut detail.context {
                context.observed_usage = None;
            }
            detail.messages.push(user_message(id, instruction, files));
            ("run_started", "运行开始", input_detail(instruction, files))
        }
        RunEvent::ModelRequestStarted => {
            detail.summary.status = RunStatus::Running;
            ("model_request_started", "请求模型", String::new())
        }
        RunEvent::ModelResponseReceived {
            content,
            response,
            token_usage,
            ..
        } => {
            if let Some(record) = token_usage {
                let usage = detail
                    .context
                    .get_or_insert_with(initial_context)
                    .observed_usage
                    .get_or_insert(ObservedTokenUsage {
                        input_tokens: 0,
                        output_tokens: 0,
                        responses: 0,
                    });
                usage.input_tokens = usage.input_tokens.saturating_add(record.usage.input_tokens);
                usage.output_tokens = usage
                    .output_tokens
                    .saturating_add(record.usage.output_tokens);
                usage.responses = usage.responses.saturating_add(1);
            }
            detail
                .messages
                .retain(|message| message.status != "streaming");
            let mut message = ChatMessage::complete(id, "assistant", content.clone());
            let reasoning = response
                .iter()
                .flat_map(|response| &response.content)
                .filter_map(|block| match block {
                    ContentBlock::Reasoning { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            message.reasoning = (!reasoning.is_empty()).then_some(reasoning);
            detail.messages.push(message);
            ("model_response_received", "响应已持久化", content.clone())
        }
        RunEvent::ToolCallRequested {
            call_id,
            tool_name,
            arguments,
        } => {
            let mut message =
                ChatMessage::complete(format!("tool-{call_id}"), "tool", arguments.clone());
            message.tool_name = Some(tool_name.clone());
            message.status = "pending".into();
            detail.messages.push(message);
            (
                "tool_call_requested",
                "工具调用",
                format!("{tool_name}\n{arguments}"),
            )
        }
        RunEvent::ToolCallSucceeded {
            call_id,
            output,
            images,
        } => {
            if let Some(message) = detail
                .messages
                .iter_mut()
                .find(|message| message.id == format!("tool-{call_id}"))
            {
                message.text = output.clone();
                message.images = images
                    .iter()
                    .map(|image| crate::contracts::ToolImageInfo {
                        sequence: envelope.sequence,
                        mime_type: image.mime_type.clone(),
                    })
                    .collect();
                message.is_error = Some(false);
                message.status = "complete".into();
            }
            (
                "tool_call_succeeded",
                "工具完成",
                output.chars().take(350).collect(),
            )
        }
        RunEvent::ToolCallFailed { call_id, message } => {
            if let Some(item) = detail
                .messages
                .iter_mut()
                .find(|item| item.id == format!("tool-{call_id}"))
            {
                item.text = message.clone();
                item.is_error = Some(true);
                item.status = "complete".into();
            }
            ("tool_call_failed", "工具失败", message.clone())
        }
        RunEvent::InputRequested { prompt } => {
            detail.summary.status = RunStatus::WaitingForInput;
            ("input_requested", "等待输入", prompt.clone())
        }
        RunEvent::InputReceived { content, files } => {
            detail.summary.status = RunStatus::Running;
            detail.messages.push(user_message(id, content, files));
            (
                "input_received",
                "收到补充输入",
                input_detail(content, files),
            )
        }
        RunEvent::RunResumed {
            content,
            interrupted_calls,
            files,
        } => {
            detail.summary.status = RunStatus::Running;
            detail.error = None;
            detail
                .messages
                .retain(|message| message.status != "streaming");
            for call in interrupted_calls {
                if let Some(message) = detail
                    .messages
                    .iter_mut()
                    .find(|message| message.id == format!("tool-{}", call.call_id))
                {
                    message.status = "complete".into();
                    message.is_error = Some(true);
                    message.text =
                        "运行中断，未收到持久化结果。恢复时不会重放此操作，将先核实实际状态。"
                            .into();
                }
            }
            detail.messages.push(user_message(id, content, files));
            ("run_resumed", "继续会话", input_detail(content, files))
        }
        RunEvent::RunCompleted { answer } => {
            detail.summary.status = RunStatus::Completed;
            ("run_completed", "运行完成", answer.clone())
        }
        RunEvent::RunFailed { message } => {
            detail.summary.status = RunStatus::Failed;
            detail.error = Some(message.clone());
            ("run_failed", "运行失败", message.clone())
        }
        RunEvent::RunCancelled => {
            detail.summary.status = RunStatus::Cancelled;
            ("run_cancelled", "运行已停止", String::new())
        }
        RunEvent::RunPaused { reason } => {
            detail.summary.status = RunStatus::Paused;
            detail.error = None;
            (
                "run_paused",
                "任务已暂停",
                match reason {
                    agent_core::agent::RunPauseReason::StepLimit { max_steps } => {
                        format!("已达到配置的 {max_steps} 步执行预算，进度已保存，可以继续任务。")
                    }
                    agent_core::agent::RunPauseReason::SharedTokenBudget { max_tokens } => {
                        format!("已达到配置的 {max_tokens} token 任务预算，进度已保存。")
                    }
                },
            )
        }
        RunEvent::ToolLoopWarning {
            tool_name,
            consecutive_calls,
            ..
        } => (
            "tool_loop_warning",
            "重复操作提醒",
            format!(
                "{tool_name} 已连续以相同参数调用 {consecutive_calls} 次，请核实进展并调整方法。"
            ),
        ),
        RunEvent::AgentMessageReceived { message } => {
            let mut item = ChatMessage::complete(id, "tool", message.text.clone());
            item.tool_name = Some(format!("协作 · {}", message.sender));
            detail.messages.push(item);
            (
                "agent_message_received",
                "协作消息",
                message.text.chars().take(350).collect(),
            )
        }
    };
    if matches!(
        detail.summary.status,
        RunStatus::Cancelled | RunStatus::Failed
    ) {
        detail
            .messages
            .retain(|message| message.status != "streaming");
        for message in &mut detail.messages {
            if message.status == "pending" {
                message.status = "complete".into();
                message.is_error = Some(true);
                message
                    .text
                    .push_str("\n工具执行随运行停止，未收到已提交的结果。");
            }
        }
    }
    detail.summary.updated_at = time::timestamp(envelope.occurred_at);
    detail.events.push(TimelineEvent {
        sequence: envelope.sequence,
        step: envelope.step,
        r#type: kind.into(),
        label: label.into(),
        detail: text,
        timestamp: time::timestamp(envelope.occurred_at),
    });
}

fn user_message(id: String, text: &str, files: &[TextFileReference]) -> ChatMessage {
    let mut message = ChatMessage::complete(id, "user", text.into());
    message.files = files
        .iter()
        .map(|file| FileReferenceInfo {
            name: file.name.clone(),
            path: file.path.clone(),
            size: file.text.len(),
        })
        .collect();
    message
}

fn input_detail(text: &str, files: &[TextFileReference]) -> String {
    if files.is_empty() {
        return text.into();
    }
    let names = files
        .iter()
        .map(|file| file.name.as_str())
        .collect::<Vec<_>>()
        .join("、");
    if text.is_empty() {
        format!("引用文件：{names}")
    } else {
        format!("{text}\n引用文件：{names}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::RunSummary;
    use agent_core::token_usage::{ModelUsage, ModelUsageRecord};

    fn detail() -> RunDetail {
        RunDetail {
            summary: RunSummary {
                delegation: None,
                execution_policy: Default::default(),
                compacting: false,
                id: "test".into(),
                revision: 0,
                title: "test".into(),
                prompt: "test".into(),
                status: RunStatus::Running,
                model_id: "local-demo".into(),
                workspace_path: None,
                permission_mode: Default::default(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
        }
    }

    fn envelope(sequence: u64, event: RunEvent) -> RunEventEnvelope {
        RunEventEnvelope {
            sequence,
            step: 1,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            event,
        }
    }

    #[test]
    fn usage_projection_is_observed_only_idempotent_and_rebuilt_on_reopen() {
        let mut detail = detail();
        let mut events = vec![envelope(
            0,
            RunEvent::RunStarted {
                instruction: "test".into(),
                files: Vec::new(),
            },
        )];
        for input_tokens in [Some(100), None, Some(200)] {
            events.push(envelope(
                events.len() as u64,
                RunEvent::ModelResponseReceived {
                    content: "response".into(),
                    response: None,
                    provider_state: None,
                    provider_continuation: None,
                    token_usage: input_tokens.map(|input_tokens| {
                        Box::new(ModelUsageRecord {
                            usage: ModelUsage {
                                input_tokens,
                                output_tokens: 50,
                                // Cached tokens are a subset, not an extra cost.
                                cached_input_tokens: Some(input_tokens),
                            },
                            baseline: None,
                        })
                    }),
                },
            ));
        }
        for event in &events {
            apply(&mut detail, event);
        }
        apply(&mut detail, events.last().unwrap());
        let usage = detail
            .context
            .as_ref()
            .unwrap()
            .observed_usage
            .as_ref()
            .unwrap();
        assert_eq!(
            (usage.input_tokens, usage.output_tokens, usage.responses),
            (300, 100, 2)
        );
        assert_eq!(detail.context.as_ref().unwrap().estimated_input_tokens, 0);
        // Service startup replays the durable log into the previously saved DTO.
        detail.messages.clear();
        detail.events.clear();
        for event in &events {
            apply(&mut detail, event);
        }
        let usage = detail
            .context
            .as_ref()
            .unwrap()
            .observed_usage
            .as_ref()
            .unwrap();
        assert_eq!(
            (usage.input_tokens, usage.output_tokens, usage.responses),
            (300, 100, 2)
        );
    }

    #[test]
    fn legacy_context_snapshots_keep_unknown_usage_optional() {
        let mut json = serde_json::to_value(initial_context()).unwrap();
        json.as_object_mut().unwrap().remove("accounting");
        json.as_object_mut().unwrap().remove("observed_usage");
        let snapshot: crate::contracts::ContextSnapshot = serde_json::from_value(json).unwrap();
        assert!(snapshot.accounting.is_none());
        assert!(snapshot.observed_usage.is_none());
    }
}
