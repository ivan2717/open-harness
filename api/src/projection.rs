use crate::{
    contracts::{ChatMessage, RunDetail, RunStatus, TimelineEvent},
    time,
};
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
        RunEvent::RunStarted { instruction } => {
            detail
                .messages
                .push(ChatMessage::complete(id, "user", instruction.clone()));
            ("run_started", "运行开始", instruction.clone())
        }
        RunEvent::ModelRequestStarted => {
            detail.summary.status = RunStatus::Running;
            ("model_request_started", "请求本地演示模型", String::new())
        }
        RunEvent::ModelResponseReceived {
            content, response, ..
        } => {
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
        RunEvent::ToolCallSucceeded { call_id, output } => {
            if let Some(message) = detail
                .messages
                .iter_mut()
                .find(|message| message.id == format!("tool-{call_id}"))
            {
                message.text = output.clone();
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
        RunEvent::InputReceived { content } => {
            detail.summary.status = RunStatus::Running;
            detail
                .messages
                .push(ChatMessage::complete(id, "user", content.clone()));
            ("input_received", "收到补充输入", content.clone())
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
