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
    assert_eq!(source.messages.len(), 10_001);
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
