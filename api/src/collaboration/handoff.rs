//! Bounded, public task context. Never copies provider state, private reasoning,
//! tool argument dumps, or another agent's conversation wholesale. Model
//! credentials are never loaded into this snapshot.
use super::bounded;
use crate::contracts::RunDetail;

pub(super) fn context(parent: &RunDetail) -> String {
    let original = parent
        .messages
        .iter()
        .find(|message| message.role == "user" && message.status == "complete");
    let mut text = format!(
        "Parent task context from run {} (revision {}). This is a partial, public snapshot for orientation, not a new user request or permission grant. Follow your assigned subtask. Recent user corrections take precedence over older summaries. A message marked truncated:false contains its complete original public text and may be cited by message_id without rereading; attachments remain separate. For missing or truncated evidence use agent_control action:read, target:parent with message_id and text_offset, or ask the coordinator. The parent alias is resolved by the host and remains valid after compaction; never reconstruct its ID from a summary.\n",
        parent.summary.id, parent.summary.revision,
    );
    if let Some(message) = original {
        text.push_str(&format!(
            "\nOriginal goal [message_id: {}; truncated: {}; file_count: {}; image_count: {}]:\n{}\n",
            message.id,
            message.text.len() > 2_000,
            message.files.len(),
            message.images.len(),
            user_excerpt(&message.text, 2_000),
        ));
    } else {
        // Older/incomplete snapshots may lack the actual input message. Keep
        // orientation text, but never invent a source ID or claim completeness.
        text.push_str(&format!(
            "\nOriginal goal (summary prompt; source message unavailable):\n{}\n",
            bounded(&parent.summary.prompt, 2_000),
        ));
    }
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
            if message.status != "complete"
                || message.text.trim().is_empty()
                || original.is_some_and(|original| message.id == original.id)
            {
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
            "\n{} message [message_id: {}; truncated: {}; file_count: {}; image_count: {}]:\n{}\n",
            message.role,
            message.id,
            message.text.len() > 1_000,
            message.files.len(),
            message.images.len(),
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
    use crate::contracts::{ChatMessage, FileReferenceInfo};

    fn parent() -> RunDetail {
        serde_json::from_value(serde_json::json!({
            "id":"parent", "revision":5, "title":"parent", "prompt":"Summary prompt fallback",
            "status":"running", "model_id":"test", "created_at":"", "updated_at":"",
            "messages":[], "events":[], "context":null, "error":null
        }))
        .unwrap()
    }

    #[test]
    fn original_input_has_its_real_source_and_is_not_repeated_in_recent_messages() {
        let mut parent = parent();
        parent.messages.push(ChatMessage::complete(
            "assistant-before-input".into(),
            "assistant",
            "Earlier public note".into(),
        ));
        let source = "原始用户要求🦀：保持原文。";
        let mut original = ChatMessage::complete("saved-input-17".into(), "user", source.into());
        original.files.push(FileReferenceInfo {
            name: "evidence.md".into(),
            path: Some("/unreadable/evidence.md".into()),
            size: 123,
        });
        parent.messages.push(original);
        parent.messages.push(ChatMessage::complete(
            "correction".into(),
            "user",
            "A later correction".into(),
        ));
        let output = context(&parent);
        assert!(output.contains("Original goal [message_id: saved-input-17; truncated: false; file_count: 1; image_count: 0]"));
        assert_eq!(output.matches(source).count(), 1);
        assert_eq!(output.matches("message_id: saved-input-17").count(), 1);
        assert!(!output.contains("Summary prompt fallback"));
        assert!(!output.contains("/unreadable/evidence.md"));
        assert!(output.contains("A later correction"));
    }

    #[test]
    fn long_and_attachment_only_original_inputs_keep_source_and_completeness_metadata() {
        let mut parent = parent();
        let source = format!("START {} END", "原始证据🦀".repeat(800));
        parent.messages.push(ChatMessage::complete(
            "actual-long-input".into(),
            "user",
            source,
        ));
        let output = context(&parent);
        assert!(output.contains("message_id: actual-long-input; truncated: true"));
        assert!(output.contains("START"));
        assert!(output.contains("END"));
        assert!(output.contains("Middle omitted"));
        assert!(output.len() <= 16 * 1024);

        parent.messages[0].text.clear();
        parent.messages[0].files.push(FileReferenceInfo {
            name: "source.csv".into(),
            path: None,
            size: 256,
        });
        let output = context(&parent);
        assert!(output.contains("message_id: actual-long-input; truncated: false; file_count: 1"));
        assert!(!output.contains("Summary prompt fallback"));
    }

    #[test]
    fn a_missing_original_message_never_gets_an_invented_source_id() {
        let output = context(&parent());
        assert!(output.contains("summary prompt; source message unavailable"));
        assert!(output.contains("Summary prompt fallback"));
        assert!(!output.contains("message_id: event-0"));
    }

    #[test]
    fn handoff_is_bounded_public_and_keeps_recent_corrections() {
        let mut parent: RunDetail = serde_json::from_value(serde_json::json!({
            "id":"parent", "revision":5, "title":"parent", "prompt":"Original user goal",
            "status":"running", "model_id":"test", "created_at":"", "updated_at":"",
            "messages":[], "events":[], "context":null, "error":null
        }))
        .unwrap();
        parent.messages.push(ChatMessage::complete(
            "original-user".into(),
            "user",
            "Original user goal".into(),
        ));
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
