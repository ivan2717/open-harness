//! Bounded UI history views. The runtime and event log retain the full history.
use crate::contracts::{ChatMessage, RunDetail, RunStatus, TimelineEvent};
use serde::Serialize;

const MESSAGE_PAGE_SIZE: usize = 100;
const EVENT_PAGE_SIZE: usize = 300;

#[derive(Debug, Serialize)]
pub struct HistoryWindow {
    pub before: Option<String>,
    pub total_messages: usize,
    pub total_events: usize,
    pub events_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leading_turn: Option<LeadingTurn>,
}

/// Context for the turn cut by a page boundary, separate from the paged rows.
/// At most one user message and one earlier assistant message are included.
#[derive(Debug, Serialize)]
pub struct LeadingTurn {
    pub id: String,
    pub user: Option<ChatMessage>,
    pub preview: Option<ChatMessage>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunView {
    #[serde(flatten)]
    pub detail: RunDetail,
    pub history: HistoryWindow,
}

impl RunView {
    pub fn new(source: &RunDetail, before: Option<&str>) -> Option<Self> {
        let end = match before {
            Some(id) => source
                .messages
                .iter()
                .position(|message| message.id == id)?,
            None => source.messages.len(),
        };
        // A single user turn can contain thousands of tool calls. Keep the hard
        // window even inside a turn; the UI can load its earlier messages.
        let start = end.saturating_sub(MESSAGE_PAGE_SIZE);
        // Tool message IDs identify calls rather than event sequences. For an
        // earlier page, use the next canonical message as its event upper bound.
        // Exact timeline history is independently paged by EventPage below.
        let end_sequence = source.messages[end..]
            .iter()
            .find_map(|message| message.id.strip_prefix("event-")?.parse::<u64>().ok());
        let event_end = end_sequence.map_or(source.events.len(), |sequence| {
            source
                .events
                .partition_point(|event| event.sequence < sequence)
        });
        let event_start = event_end.saturating_sub(EVENT_PAGE_SIZE);
        Some(Self {
            detail: RunDetail {
                pending_inputs: source.pending_inputs.clone(),
                pending_inputs_paused: source.pending_inputs_paused,
                summary: source.summary.clone(),
                messages: source.messages[start..end].to_vec(),
                events: source.events[event_start..event_end].to_vec(),
                context: source.context.clone(),
                error: source.error.clone(),
            },
            history: HistoryWindow {
                before: (start > 0).then(|| source.messages[start].id.clone()),
                total_messages: source.messages.len(),
                total_events: source.events.len(),
                events_before: (event_start > 0).then(|| source.events[event_start].sequence),
                leading_turn: (start < end).then(|| leading_turn(source, start)).flatten(),
            },
        })
    }
}

fn event_sequence(message: &ChatMessage) -> Option<u64> {
    message.id.strip_prefix("event-")?.parse().ok()
}

fn leading_turn(source: &RunDetail, start: usize) -> Option<LeadingTurn> {
    if source.messages[start].role == "user" {
        return None;
    }
    // Find the boundary and preview in one reverse pass. Long tool-only turns
    // otherwise require scanning the same prefix twice on every stream update.
    let mut turn_start = 0;
    let mut preview = None;
    for (index, message) in source.messages[..start].iter().enumerate().rev() {
        if message.role == "user" {
            turn_start = index;
            break;
        }
        if preview.is_none()
            && message.role == "assistant"
            && message.status != "streaming"
            && (!message.text.is_empty()
                || message
                    .reasoning
                    .as_ref()
                    .is_some_and(|text| !text.is_empty()))
        {
            preview = Some(message);
        }
    }
    let turn_end = source.messages[start + 1..]
        .iter()
        .position(|message| message.role == "user")
        .map_or(source.messages.len(), |offset| start + 1 + offset);
    let first = &source.messages[turn_start];
    let user = (first.role == "user").then_some(first);
    let events = user.and_then(event_sequence).map_or(&[][..], |sequence| {
        let next_sequence = source.messages.get(turn_end).and_then(event_sequence);
        let event_start = source
            .events
            .partition_point(|event| event.sequence < sequence);
        let event_end = next_sequence.map_or(source.events.len(), |next| {
            source.events.partition_point(|event| event.sequence < next)
        });
        &source.events[event_start..event_end]
    });
    let active = turn_end == source.messages.len() && source.summary.status == RunStatus::Running;
    Some(LeadingTurn {
        id: first.id.clone(),
        user: user.cloned(),
        preview: preview.cloned(),
        started_at: first
            .started_at
            .clone()
            .or_else(|| events.first().map(|event| event.timestamp.clone())),
        completed_at: if active {
            None
        } else {
            events
                .iter()
                .find(|event| {
                    matches!(
                        event.r#type.as_str(),
                        "input_requested"
                            | "run_completed"
                            | "run_failed"
                            | "run_cancelled"
                            | "run_paused"
                    )
                })
                .or_else(|| events.last())
                .map(|event| event.timestamp.clone())
                .or_else(|| source.messages[turn_end - 1].completed_at.clone())
        },
    })
}

#[cfg(test)]
#[path = "run_view_tests.rs"]
mod tests;

#[derive(Debug, Serialize)]
pub struct EventPage {
    pub events: Vec<TimelineEvent>,
    pub before: Option<u64>,
}

impl EventPage {
    pub fn new(source: &RunDetail, before: u64) -> Self {
        let end = source
            .events
            .partition_point(|event| event.sequence < before);
        let start = end.saturating_sub(EVENT_PAGE_SIZE);
        Self {
            events: source.events[start..end].to_vec(),
            before: (start > 0).then(|| source.events[start].sequence),
        }
    }
}
