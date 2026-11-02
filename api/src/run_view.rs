//! Bounded UI history views. The runtime and event log retain the full history.
use crate::contracts::{RunDetail, TimelineEvent};
use serde::Serialize;

const MESSAGE_PAGE_SIZE: usize = 100;
const EVENT_PAGE_SIZE: usize = 300;

#[derive(Debug, Serialize)]
pub struct HistoryWindow {
    pub before: Option<String>,
    pub total_messages: usize,
    pub total_events: usize,
    pub events_before: Option<u64>,
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
            },
        })
    }
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
