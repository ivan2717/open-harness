use crate::snapshot::Snapshot;
use agent_core::agent::RunEventEnvelope;
use agent_runtime::{AppendOutcome, EventStore, EventStoreError, JsonlEventStore, StoredRun};

/// Publish only after JSONL acknowledges durability, before the driver advances
/// to another model round. This avoids racing transient output against commits.
pub struct ObservedEventStore {
    pub inner: JsonlEventStore,
    pub snapshot: Snapshot,
}

impl EventStore for ObservedEventStore {
    async fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, EventStoreError> {
        let outcome = self.inner.append(run_id, agent_id, event).await?;
        if let Err(error) = self.snapshot.event(event) {
            tracing::error!(%error, "could not publish committed event; JSONL remains authoritative");
        }
        Ok(outcome)
    }
    async fn load(&self, run_id: &str) -> Result<Option<StoredRun>, EventStoreError> {
        self.inner.load(run_id).await
    }
}
