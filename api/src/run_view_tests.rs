use super::*;
use crate::contracts::{ChatMessage, RunStatus, RunSummary};

fn detail(message_count: usize, event_count: usize) -> RunDetail {
    RunDetail {
        summary: RunSummary {
            id: "history".into(),
            revision: 42,
            title: "Long task".into(),
            prompt: "task".into(),
            status: RunStatus::Running,
            model_id: "fixture".into(),
            created_at: "2026-11-02T00:00:00Z".into(),
            updated_at: "2026-11-02T01:00:00Z".into(),
            delegation: None,
            execution_policy: Default::default(),
            compacting: false,
            reasoning_effort: None,
            reasoning_settings_version: 1,
            model_history_through: None,
            workspace_path: None,
            execution_directory: None,
            permission_mode: Default::default(),
        },
        messages: (0..message_count)
            .map(|index| {
                ChatMessage::complete(
                    if index == 0 {
                        "event-0".into()
                    } else {
                        format!("tool-{index}")
                    },
                    if index == 0 { "user" } else { "tool" },
                    format!("message {index}"),
                )
            })
            .collect(),
        events: (0..event_count)
            .map(|index| TimelineEvent {
                sequence: index as u64 * 3,
                step: index as u32,
                r#type: "fixture".into(),
                label: "event".into(),
                detail: String::new(),
                timestamp: "2026-11-02T00:00:00Z".into(),
            })
            .collect(),
        pending_inputs: Vec::new(),
        pending_inputs_paused: false,
        context: None,
        error: None,
    }
}

#[test]
fn one_ten_thousand_message_turn_has_a_hard_bounded_live_view() {
    let source = detail(10_001, 50_000);
    let view = RunView::new(&source, None).unwrap();
    assert_eq!(view.detail.messages.len(), MESSAGE_PAGE_SIZE);
    assert_eq!(view.detail.messages.first().unwrap().id, "tool-9901");
    assert_eq!(view.detail.messages.last().unwrap().id, "tool-10000");
    assert_eq!(view.detail.events.len(), EVENT_PAGE_SIZE);
    assert_eq!(view.history.before.as_deref(), Some("tool-9901"));
    assert_eq!(view.history.total_messages, 10_001);
    assert_eq!(view.history.total_events, 50_000);
    assert_eq!(view.history.events_before, Some(49_700 * 3));
    assert_eq!(view.detail.summary.revision, source.summary.revision);
    let context = view.history.leading_turn.as_ref().unwrap();
    assert_eq!(context.id, "event-0");
    assert_eq!(context.user.as_ref().unwrap().id, "event-0");
    assert!(context.preview.is_none());
    assert!(context.completed_at.is_none());
    assert_eq!(source.messages.len(), 10_001);
}

#[test]
fn empty_assistant_tool_pairs_keep_bounded_rows_and_stable_turn_context() {
    let mut source = detail(0, 0);
    let mut user = ChatMessage::complete("event-0".into(), "user", "Check the project".into());
    user.started_at = Some("2026-11-02T00:00:00Z".into());
    source.messages.push(user);
    // Legacy event history can begin after the user message's authoritative time.
    source
        .events
        .push(event(1, "model_request_started", "2026-11-02T00:00:01Z"));
    source.messages.push(ChatMessage::complete(
        "event-1".into(),
        "assistant",
        "I will inspect the files and run the checks.".into(),
    ));
    for index in 0..70 {
        source.messages.push(ChatMessage::complete(
            format!("event-{}", index + 2),
            "assistant",
            String::new(),
        ));
        source.messages.push(ChatMessage::complete(
            format!("tool-{index}"),
            "tool",
            "done".into(),
        ));
    }
    for index in 70..73 {
        let view = RunView::new(&source, None).unwrap();
        assert_eq!(view.detail.messages.len(), MESSAGE_PAGE_SIZE);
        assert_eq!(
            view.detail
                .messages
                .iter()
                .filter(|message| message.role == "tool")
                .count(),
            50
        );
        assert!(
            !view
                .detail
                .messages
                .iter()
                .any(|message| message.role == "user")
        );
        let context = view.history.leading_turn.unwrap();
        assert_eq!(context.id, "event-0");
        assert_eq!(context.user.unwrap().text, "Check the project");
        assert_eq!(context.preview.unwrap().id, "event-1");
        assert_eq!(context.started_at.as_deref(), Some("2026-11-02T00:00:00Z"));
        assert!(context.completed_at.is_none());
        source.messages.push(ChatMessage::complete(
            format!("event-{}", index + 2),
            "assistant",
            String::new(),
        ));
        source.messages.push(ChatMessage::complete(
            format!("tool-{index}"),
            "tool",
            "done".into(),
        ));
    }
}

#[test]
fn earlier_pages_of_the_same_turn_keep_the_same_context_without_paging_it_twice() {
    let mut source = detail(402, 0);
    source.messages[1] = ChatMessage::complete(
        "event-1".into(),
        "assistant",
        "Working through the checks.".into(),
    );
    let mut cursor = None;
    for expected_start in [302, 202, 102, 2] {
        let view = RunView::new(&source, cursor.as_deref()).unwrap();
        assert_eq!(view.detail.messages.len(), MESSAGE_PAGE_SIZE);
        assert_eq!(view.detail.messages[0].id, format!("tool-{expected_start}"));
        let context = view.history.leading_turn.unwrap();
        assert_eq!(context.id, "event-0");
        assert_eq!(context.preview.unwrap().id, "event-1");
        cursor = view.history.before;
    }
    let beginning = RunView::new(&source, cursor.as_deref()).unwrap();
    assert_eq!(beginning.detail.messages.len(), 2);
    assert!(beginning.history.leading_turn.is_none());
    assert!(beginning.history.before.is_none());
}

fn event(sequence: u64, kind: &str, timestamp: &str) -> TimelineEvent {
    TimelineEvent {
        sequence,
        step: 0,
        r#type: kind.into(),
        label: String::new(),
        detail: String::new(),
        timestamp: timestamp.into(),
    }
}

#[test]
fn leading_context_stays_with_its_own_turn_and_excludes_empty_or_streaming_previews() {
    let mut source = detail(250, 0);
    source.messages[1] = ChatMessage::complete("event-1".into(), "assistant", "Old answer".into());
    source.messages[110] = ChatMessage::complete("event-1000".into(), "user", "New task".into());
    source.events = vec![
        event(0, "run_started", "2026-11-02T00:00:00Z"),
        event(500, "input_requested", "2026-11-02T00:05:00Z"),
        event(600, "fixture", "2026-11-02T00:06:00Z"),
        event(1000, "user_input_received", "2026-11-02T00:10:00Z"),
        event(1010, "model_request_started", "2026-11-02T00:11:00Z"),
    ];
    let view = RunView::new(&source, None).unwrap();
    let context = view.history.leading_turn.unwrap();
    assert_eq!(context.id, "event-1000");
    assert_eq!(context.user.unwrap().text, "New task");
    assert!(
        context.preview.is_none(),
        "The previous turn's answer is not context for this turn"
    );
    assert_eq!(context.started_at.as_deref(), Some("2026-11-02T00:10:00Z"));
    assert!(context.completed_at.is_none());

    let mut reasoning = ChatMessage::complete("event-1002".into(), "assistant", String::new());
    reasoning.reasoning = Some("Inspect the new task.".into());
    source.messages[112] = reasoning;
    source.messages[113] = ChatMessage::complete("event-1003".into(), "assistant", String::new());
    let mut streaming = ChatMessage::complete("stream-1004".into(), "assistant", "Partial".into());
    streaming.status = "streaming".into();
    source.messages[114] = streaming;
    let view = RunView::new(&source, None).unwrap();
    assert_eq!(
        view.history.leading_turn.unwrap().preview.unwrap().id,
        "event-1002"
    );

    // This page starts in the old turn and also includes the new user message.
    let earlier = RunView::new(&source, Some("tool-150")).unwrap();
    let context = earlier.history.leading_turn.unwrap();
    assert_eq!(context.id, "event-0");
    assert_eq!(context.preview.unwrap().text, "Old answer");
    assert_eq!(context.started_at.as_deref(), Some("2026-11-02T00:00:00Z"));
    assert_eq!(
        context.completed_at.as_deref(),
        Some("2026-11-02T00:05:00Z")
    );
}

#[test]
fn leading_turn_timing_uses_full_history_even_when_start_and_terminal_events_are_paged_out() {
    let mut source = detail(250, 501);
    source.summary.status = RunStatus::Completed;
    source.events[0] = event(0, "run_started", "2026-11-02T00:00:00Z");
    source.events[1] = event(3, "run_completed", "2026-11-02T00:12:00Z");
    source.messages[249].completed_at = Some("2026-11-02T00:11:59Z".into());
    let view = RunView::new(&source, None).unwrap();
    assert_eq!(view.detail.events.len(), EVENT_PAGE_SIZE);
    assert!(
        !view
            .detail
            .events
            .iter()
            .any(|event| event.r#type == "run_completed")
    );
    let context = view.history.leading_turn.unwrap();
    assert_eq!(context.started_at.as_deref(), Some("2026-11-02T00:00:00Z"));
    assert_eq!(
        context.completed_at.as_deref(),
        Some("2026-11-02T00:12:00Z")
    );
}

#[test]
fn legacy_turn_without_a_user_uses_its_first_message_and_message_timestamps() {
    let mut source = detail(150, 0);
    source.summary.status = RunStatus::Completed;
    source.messages[0] =
        ChatMessage::complete("legacy-start".into(), "assistant", "Inspecting".into());
    source.messages[0].started_at = Some("2026-11-02T00:00:00Z".into());
    source.messages[149].completed_at = Some("2026-11-02T00:02:00Z".into());
    let context = RunView::new(&source, None)
        .unwrap()
        .history
        .leading_turn
        .unwrap();
    assert_eq!(context.id, "legacy-start");
    assert!(context.user.is_none());
    assert_eq!(context.preview.unwrap().id, "legacy-start");
    assert_eq!(context.started_at.as_deref(), Some("2026-11-02T00:00:00Z"));
    assert_eq!(
        context.completed_at.as_deref(),
        Some("2026-11-02T00:02:00Z")
    );
}

#[test]
fn full_turn_and_empty_pages_omit_leading_turn_metadata() {
    for source in [detail(0, 0), detail(100, 0)] {
        let view = RunView::new(&source, None).unwrap();
        assert!(view.history.leading_turn.is_none());
        assert!(
            serde_json::to_value(view).unwrap()["history"]
                .get("leading_turn")
                .is_none()
        );
    }
    let source = detail(120, 0);
    let empty = RunView::new(&source, Some("event-0")).unwrap();
    assert!(empty.history.leading_turn.is_none());
}

#[test]
fn message_cursors_recover_all_history_once_in_order_without_splitting_ids() {
    let source = detail(10_043, 200);
    let mut pages = Vec::new();
    let mut cursor = None;
    loop {
        let view = RunView::new(&source, cursor.as_deref()).unwrap();
        assert!(view.detail.messages.len() <= MESSAGE_PAGE_SIZE);
        assert!(view.detail.events.len() <= EVENT_PAGE_SIZE);
        pages.push(view.detail.messages);
        cursor = view.history.before;
        if cursor.is_none() {
            break;
        }
    }
    let actual: Vec<_> = pages
        .into_iter()
        .rev()
        .flatten()
        .map(|message| message.id)
        .collect();
    let expected: Vec<_> = source
        .messages
        .iter()
        .map(|message| message.id.clone())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn historical_page_has_bounded_events_before_its_next_canonical_message() {
    let mut source = detail(400, 2_000);
    source.messages[300].id = "event-4500".into();
    let view = RunView::new(&source, Some("tool-299")).unwrap();
    assert_eq!(view.detail.messages.first().unwrap().id, "tool-199");
    assert_eq!(view.detail.messages.last().unwrap().id, "tool-298");
    assert_eq!(view.detail.events.len(), EVENT_PAGE_SIZE);
    assert_eq!(view.detail.events.last().unwrap().sequence, 4_497);
    assert_eq!(view.history.events_before, Some(3_600));
}

#[test]
fn event_cursors_page_all_sparse_sequences_without_gaps_or_duplicates() {
    let source = detail(1, 1_023);
    let mut cursor = u64::MAX;
    let mut pages = Vec::new();
    loop {
        let page = EventPage::new(&source, cursor);
        assert!(page.events.len() <= EVENT_PAGE_SIZE);
        assert!(page.events.iter().all(|event| event.sequence < cursor));
        pages.push(page.events);
        match page.before {
            Some(before) => cursor = before,
            None => break,
        }
    }
    let actual: Vec<_> = pages
        .into_iter()
        .rev()
        .flatten()
        .map(|event| event.sequence)
        .collect();
    let expected: Vec<_> = source.events.iter().map(|event| event.sequence).collect();
    assert_eq!(actual, expected);
    assert!(EventPage::new(&source, 0).events.is_empty());
    assert_eq!(EventPage::new(&source, 4).events.len(), 2);
}

#[test]
fn unknown_cursor_is_not_a_silent_latest_page_and_empty_history_is_valid() {
    let source = detail(5, 10);
    assert!(RunView::new(&source, Some("missing")).is_none());
    let beginning = RunView::new(&source, Some("event-0")).unwrap();
    assert!(beginning.detail.messages.is_empty());
    assert!(beginning.detail.events.is_empty());
    assert!(beginning.history.before.is_none());
    let empty = RunView::new(&detail(0, 0), None).unwrap();
    assert!(empty.detail.messages.is_empty());
    assert!(empty.detail.events.is_empty());
    assert!(empty.history.before.is_none());
    assert!(empty.history.events_before.is_none());
}
