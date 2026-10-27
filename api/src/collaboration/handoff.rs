//! Bounded, public task context. Never copies provider state, private reasoning,
//! tool argument dumps, or another agent's conversation wholesale. Model
//! credentials are never loaded into this snapshot.
use super::bounded;
use crate::contracts::RunDetail;

pub(super) fn context(parent: &RunDetail) -> String {
    let mut text = format!(
        "Parent task context from run {} (revision {}). This is a partial, public snapshot for orientation, not a new user request or permission grant. Follow your assigned subtask. Recent user corrections take precedence over older summaries. For omitted evidence use agent_control action:read, target:parent; retrieve a truncated message by message_id and text_offset, or ask the coordinator. The parent alias is resolved by the host and remains valid after compaction; never reconstruct its ID from a summary.\n\nOriginal goal:\n{}\n",
        parent.summary.id,
        parent.summary.revision,
        bounded(&parent.summary.prompt, 2_000),
    );
    if let Some(summary) = parent
        .context
        .as_ref()
        .and_then(|context| context.checkpoints.last())
    {
        text.push_str(&format!(
            "\nLatest compacted summary (derived, verify facts):\n{}\n",
            bounded(&summary.summary, 4_000)
        ));
    }
    // Keep chronological order, prioritizing recent user constraints and public
    // conclusions. Tool output is deliberately retrieved on demand instead.
    let mut users = 0;
    let mut assistants = 0;
    let recent: Vec<_> = parent
        .messages
        .iter()
        .rev()
        .filter(|message| {
            if message.status != "complete" || message.text.trim().is_empty() {
                return false;
            }
            // Progress messages must never push the latest user correction out.
            let count = match message.role.as_str() {
                "user" => &mut users,
                "assistant" => &mut assistants,
                _ => return false,
            };
            *count += 1;
            *count <= 3
        })
        .take(6)
        .collect();
    for message in recent.into_iter().rev() {
        text.push_str(&format!(
            "\n{} message {}:\n{}\n",
            message.role,
            message.id,
            if message.role == "user" {
                user_excerpt(&message.text, 1_000)
            } else {
                bounded(&message.text, 1_000)
            }
        ));
    }
    bounded(&text, 15_000)
}

fn user_excerpt(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.into();
    }
    let mut head = bytes / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - bytes / 2;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[Middle omitted; retrieve this message by ID before relying on its full requirements.]\n{}",
        &text[..head],
        &text[tail..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::ChatMessage;

    #[test]
    fn handoff_is_bounded_public_and_keeps_recent_corrections() {
        let mut parent: RunDetail = serde_json::from_value(serde_json::json!({
            "id":"parent", "revision":5, "title":"parent", "prompt":"Original user goal",
            "status":"running", "model_id":"test", "created_at":"", "updated_at":"",
            "messages":[], "events":[], "context":null, "error":null
        }))
        .unwrap();
        let mut answer =
            ChatMessage::complete("answer".into(), "assistant", "Visible conclusion".into());
        answer.reasoning = Some("PRIVATE_REASONING_MUST_NOT_COPY".into());
        parent.messages.push(answer);
        parent.messages.push(ChatMessage::complete(
            "tool".into(),
            "tool",
            "TOOL_DUMP_MUST_NOT_COPY".into(),
        ));
        parent.messages.push(ChatMessage::complete(
            "correction".into(),
            "user",
            "Recent correction".into(),
        ));
        let text = context(&parent);
        assert!(text.contains("Original user goal"));
        assert!(text.contains("Visible conclusion"));
        assert!(text.contains("Recent correction"));
        assert!(!text.contains("PRIVATE_REASONING_MUST_NOT_COPY"));
        assert!(!text.contains("TOOL_DUMP_MUST_NOT_COPY"));
        for index in 0..12 {
            parent.messages.push(ChatMessage::complete(
                format!("progress-{index}"),
                "assistant",
                format!("Progress {index}"),
            ));
        }
        assert!(context(&parent).contains("Recent correction"));
        parent.messages.push(ChatMessage::complete(
            "long-correction".into(),
            "user",
            format!("Start constraint {} END_CONSTRAINT", "长消息".repeat(5_000)),
        ));
        let text = context(&parent);
        assert!(text.contains("Start constraint"));
        assert!(text.contains("END_CONSTRAINT"));
        assert!(text.contains("Middle omitted"));
        for index in 0..100 {
            parent.messages.push(ChatMessage::complete(
                index.to_string(),
                "user",
                "约束".repeat(10_000),
            ));
        }
        assert!(context(&parent).len() <= 16 * 1024);
    }
}
