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
    let timestamp = time::timestamp(envelope.occurred_at);
    let (kind, label, text) = match &envelope.event {
        RunEvent::RunStarted { instruction, files } => {
            // The public snapshot may already contain totals when reopening.
            // Rebuild them from durable samples exactly once during replay.
            if let Some(context) = &mut detail.context {
                context.observed_usage = None;
            }
            detail
                .messages
                .push(user_message(id, instruction, files, &timestamp));
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
            message.started_at = model_request_started_at(detail);
            message.completed_at = Some(timestamp.clone());
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
        RunEvent::ModelResponseInterrupted { content } => {
            detail
                .messages
                .retain(|message| message.status != "streaming");
            if !content.is_empty() {
                let mut message = ChatMessage::complete(id, "assistant", content.clone());
                message.started_at = model_request_started_at(detail);
                message.completed_at = Some(timestamp.clone());
                message.status = "interrupted".into();
                detail.messages.push(message);
            }
            ("model_response_interrupted", "回复已中断", String::new())
        }
        RunEvent::ToolCallRequested {
            call_id,
            tool_name,
            arguments,
        } => {
            let mut message =
                ChatMessage::complete(format!("tool-{call_id}"), "tool", arguments.clone());
            message.tool_name = Some(tool_name.clone());
            message.tool_arguments = Some(arguments.clone());
            message.started_at = Some(timestamp.clone());
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
                message.completed_at = Some(timestamp.clone());
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
                item.completed_at = Some(timestamp.clone());
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
            detail
                .messages
                .push(user_message(id, content, files, &timestamp));
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
        }
        | RunEvent::UserInputReceived {
            content,
            interrupted_calls,
            files,
            ..
        } => {
            if let RunEvent::UserInputReceived { id, .. } = &envelope.event {
                detail.pending_inputs.retain(|input| &input.id != id);
            }
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
                    message
                        .completed_at
                        .get_or_insert_with(|| timestamp.clone());
                    message.is_error = Some(true);
                    message.text =
                        "运行中断，未收到持久化结果。恢复时不会重放此操作，将先核实实际状态。"
                            .into();
                }
            }
            detail
                .messages
                .push(user_message(id, content, files, &timestamp));
            if matches!(envelope.event, RunEvent::UserInputReceived { .. }) {
                (
                    "user_input_received",
                    "收到新要求",
                    input_detail(content, files),
                )
            } else {
                ("run_resumed", "继续会话", input_detail(content, files))
            }
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
            item.started_at = Some(timestamp.clone());
            item.completed_at = Some(timestamp.clone());
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
                message.completed_at = Some(timestamp.clone());
                message.is_error = Some(true);
                message
                    .text
                    .push_str("\n工具执行随运行停止，未收到已提交的结果。");
            }
        }
    }
    detail.summary.updated_at = timestamp.clone();
    detail.events.push(TimelineEvent {
        sequence: envelope.sequence,
        step: envelope.step,
        r#type: kind.into(),
        label: label.into(),
        detail: text,
        timestamp,
    });
}

pub(crate) fn model_request_started_at(detail: &RunDetail) -> Option<String> {
    detail
        .events
        .iter()
        .rev()
        .find(|event| event.r#type == "model_request_started")
        .map(|event| event.timestamp.clone())
}

fn user_message(
    id: String,
    text: &str,
    files: &[TextFileReference],
    timestamp: &str,
) -> ChatMessage {
    let mut message = ChatMessage::complete(id, "user", text.into());
    message.started_at = Some(timestamp.into());
    message.completed_at = Some(timestamp.into());
    message.files = files
        .iter()
        .map(|file| FileReferenceInfo {
            name: file.name.clone(),
            path: file.path.clone(),
            size: file.attachment.as_ref().map_or_else(
                || {
                    file.image.as_ref().map_or(file.text.len(), |image| {
                        image.decoded_bytes().map_or(0, |bytes| bytes.len())
                    })
                },
                |attachment| attachment.size as usize,
            ),
            mime_type: file.image.as_ref().map(|image| image.mime_type.clone()),
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
            pending_inputs: Vec::new(),
            pending_inputs_paused: false,
            summary: RunSummary {
                reasoning_settings_version: 1,
                reasoning_effort: None,
                model_history_through: None,
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
                execution_directory: None,
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
            occurred_at: std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(sequence),
            event,
        }
    }

    #[test]
    fn tool_metadata_preserves_arguments_through_success_and_failure() {
        let mut detail = detail();
        let arguments =
            r#"{"server":"chrome","tool":"chrome_open","arguments":{"url":"https://example.com"}}"#;
        for (index, succeeds) in [true, false].into_iter().enumerate() {
            let sequence = index as u64 * 2;
            let call_id = format!("call-{index}");
            apply(
                &mut detail,
                &envelope(
                    sequence,
                    RunEvent::ToolCallRequested {
                        call_id: call_id.clone(),
                        tool_name: "call_mcp_tool".into(),
                        arguments: arguments.into(),
                    },
                ),
            );
            let pending = detail.messages.last().unwrap();
            assert_eq!(pending.tool_arguments.as_deref(), Some(arguments));
            assert_eq!(
                pending.started_at,
                Some(detail.events.last().unwrap().timestamp.clone())
            );
            assert!(pending.completed_at.is_none());
            let finished = if succeeds {
                RunEvent::ToolCallSucceeded {
                    call_id,
                    output: "opened".into(),
                    images: Vec::new(),
                }
            } else {
                RunEvent::ToolCallFailed {
                    call_id,
                    message: "connection unavailable".into(),
                }
            };
            apply(&mut detail, &envelope(sequence + 1, finished));
            let complete = detail.messages.last().unwrap();
            assert_eq!(complete.tool_arguments.as_deref(), Some(arguments));
            assert_ne!(complete.text, arguments);
            assert_eq!(complete.is_error, Some(!succeeds));
            assert_eq!(
                complete.completed_at,
                Some(detail.events.last().unwrap().timestamp.clone())
            );
            assert!(complete.started_at < complete.completed_at);
        }
    }

    #[test]
    fn legacy_message_metadata_is_optional_and_rebuilt_from_events() {
        let mut detail = detail();
        let legacy: ChatMessage = serde_json::from_value(serde_json::json!({
            "id": "tool-first", "role": "tool", "text": "done",
            "tool_name": "run_command", "status": "complete"
        }))
        .unwrap();
        assert!(legacy.tool_arguments.is_none());
        assert!(legacy.started_at.is_none());
        assert!(legacy.completed_at.is_none());
        detail.messages.push(legacy);
        // Startup recreates public messages from the durable event log.
        detail.messages.clear();
        for event in [
            envelope(
                0,
                RunEvent::RunStarted {
                    instruction: "test".into(),
                    files: Vec::new(),
                },
            ),
            envelope(1, RunEvent::ModelRequestStarted),
            envelope(
                2,
                RunEvent::ModelResponseReceived {
                    content: "Checking the page.".into(),
                    response: None,
                    provider_state: None,
                    provider_continuation: None,
                    token_usage: None,
                },
            ),
            envelope(
                3,
                RunEvent::ToolCallRequested {
                    call_id: "first".into(),
                    tool_name: "run_command".into(),
                    arguments: r#"{"command":"pwd"}"#.into(),
                },
            ),
            envelope(
                4,
                RunEvent::ToolCallSucceeded {
                    call_id: "first".into(),
                    output: "done".into(),
                    images: Vec::new(),
                },
            ),
        ] {
            apply(&mut detail, &event);
        }
        assert_eq!(
            detail.messages[0].started_at.as_deref(),
            Some("1970-01-01T00:00:00.000Z")
        );
        assert_eq!(
            detail.messages[1].started_at.as_deref(),
            Some("1970-01-01T00:00:01.000Z")
        );
        assert_eq!(
            detail.messages[1].completed_at.as_deref(),
            Some("1970-01-01T00:00:02.000Z")
        );
        assert_eq!(
            detail.messages[2].tool_arguments.as_deref(),
            Some(r#"{"command":"pwd"}"#)
        );
        let round_trip: RunDetail =
            serde_json::from_slice(&serde_json::to_vec(&detail).unwrap()).unwrap();
        assert_eq!(
            round_trip.messages[2].tool_arguments,
            detail.messages[2].tool_arguments
        );
        assert_eq!(
            round_trip.messages[2].started_at,
            detail.messages[2].started_at
        );
        assert_eq!(
            round_trip.messages[2].completed_at,
            detail.messages[2].completed_at
        );
    }

    #[test]
    fn cancellation_finishes_tool_metadata_without_losing_arguments() {
        let mut detail = detail();
        apply(
            &mut detail,
            &envelope(
                0,
                RunEvent::ToolCallRequested {
                    call_id: "cancelled".into(),
                    tool_name: "run_command".into(),
                    arguments: r#"{"command":"sleep 10"}"#.into(),
                },
            ),
        );
        apply(&mut detail, &envelope(2, RunEvent::RunCancelled));
        let tool = &detail.messages[0];
        assert_eq!(
            tool.tool_arguments.as_deref(),
            Some(r#"{"command":"sleep 10"}"#)
        );
        assert_eq!(
            tool.completed_at.as_deref(),
            Some("1970-01-01T00:00:02.000Z")
        );
        assert_eq!(tool.is_error, Some(true));
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
