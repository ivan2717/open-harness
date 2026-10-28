//! Retrieve the exact text or raster snapshot attached to a persisted user message.
use super::{AppState, ServiceError};
use agent_core::{TextFileReference, agent::RunEvent};
use agent_runtime::EventStore;

impl AppState {
    pub async fn message_image(
        &self,
        id: &str,
        message_id: &str,
        index: usize,
    ) -> Result<agent_core::ModelImage, ServiceError> {
        let entry = self.entry(id)?;
        let detail = entry.snapshot.get();
        let image = detail
            .messages
            .iter()
            .find(|message| message.id == message_id && message.role == "tool")
            .and_then(|message| message.images.get(index))
            .ok_or(ServiceError::NotFound)?;
        let stored = entry
            .runtime
            .event_store()
            .inner
            .load_range(
                id,
                image.sequence
                    ..image
                        .sequence
                        .checked_add(1)
                        .ok_or(ServiceError::NotFound)?,
            )
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)?;
        stored
            .events
            .iter()
            .find_map(|event| match &event.event {
                RunEvent::ToolCallSucceeded {
                    call_id, images, ..
                } if format!("tool-{call_id}") == message_id => images.get(index).cloned(),
                _ => None,
            })
            .ok_or(ServiceError::NotFound)
    }
    pub async fn message_file(
        &self,
        id: &str,
        message_id: &str,
        index: usize,
    ) -> Result<TextFileReference, ServiceError> {
        let entry = self.entry(id)?;
        let detail = entry.snapshot.get();
        if !detail.messages.iter().any(|message| {
            message.id == message_id && message.role == "user" && index < message.files.len()
        }) {
            return Err(ServiceError::NotFound);
        }
        let sequence = message_id
            .strip_prefix("event-")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(ServiceError::NotFound)?;
        let end = sequence.checked_add(1).ok_or(ServiceError::NotFound)?;
        // Reuse the runtime's indexed store and writer lock. Reading only this
        // event avoids loading the whole conversation or racing a separate store.
        let stored = entry
            .runtime
            .event_store()
            .inner
            .load_range(id, sequence..end)
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)?;
        stored
            .events
            .iter()
            .find(|envelope| envelope.sequence == sequence)
            .and_then(|envelope| match &envelope.event {
                RunEvent::RunStarted { files, .. }
                | RunEvent::InputReceived { files, .. }
                | RunEvent::RunResumed { files, .. } => files.get(index).cloned(),
                _ => None,
            })
            .ok_or(ServiceError::NotFound)
    }
}
