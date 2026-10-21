use agent_core::agent::AgentRun;
use agent_runtime::{EventStore, InMemoryEventStore};

pub async fn persist_run(store: &InMemoryEventStore, run: &AgentRun) {
    for event in run.events() {
        store.append(run.id(), run.agent_id(), event).await.unwrap();
    }
}
