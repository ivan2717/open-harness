use std::{
    collections::{HashMap, HashSet},
    future::Future,
    ops::Range,
    sync::{Arc, Mutex},
};

use agent_core::agent::{RunEvent, RunEventEnvelope};

use super::{
    AppendOutcome, EventStore, EventStoreError, StoredRun, bounded_range, validate_first_event,
};

type MemoryRun = Arc<Mutex<Option<StoredRun>>>;

#[derive(Debug, Clone, Default)]
pub struct InMemoryEventStore {
    runs: Arc<Mutex<HashMap<String, MemoryRun>>>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventStore for InMemoryEventStore {
    async fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, EventStoreError> {
        let run = {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| EventStoreError::LockPoisoned)?;
            Arc::clone(runs.entry(run_id.to_owned()).or_default())
        };
        let mut run = run.lock().map_err(|_| EventStoreError::LockPoisoned)?;
        match run.as_mut() {
            Some(stored) => match classify_append(stored, run_id, agent_id, event)? {
                AppendDecision::Append => {
                    stored.events.push(event.clone());
                    Ok(AppendOutcome::Appended)
                }
                AppendDecision::AlreadyPresent => Ok(AppendOutcome::AlreadyPresent),
            },
            None => {
                validate_first_event(run_id, event)?;
                *run = Some(StoredRun {
                    run_id: run_id.to_owned(),
                    agent_id: agent_id.to_owned(),
                    events: vec![event.clone()],
                });
                Ok(AppendOutcome::Appended)
            }
        }
    }

    fn load(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<Option<StoredRun>, EventStoreError>> + Send {
        self.load_range(run_id, 0..u64::MAX)
    }

    async fn load_range(
        &self,
        run_id: &str,
        sequences: Range<u64>,
    ) -> Result<Option<StoredRun>, EventStoreError> {
        let run = {
            let runs = self
                .runs
                .lock()
                .map_err(|_| EventStoreError::LockPoisoned)?;
            runs.get(run_id).cloned()
        };
        let Some(run) = run else {
            return Ok(None);
        };
        let run = run.lock().map_err(|_| EventStoreError::LockPoisoned)?;
        Ok(run.as_ref().map(|stored| {
            let range = bounded_range(sequences, stored.events.len());
            StoredRun {
                run_id: stored.run_id.clone(),
                agent_id: stored.agent_id.clone(),
                events: stored.events[range].to_vec(),
            }
        }))
    }

    async fn load_tool_calls(
        &self,
        run_id: &str,
        call_ids: &[String],
    ) -> Result<Vec<RunEventEnvelope>, EventStoreError> {
        if call_ids.is_empty() {
            return Ok(Vec::new());
        }
        let run = {
            let runs = self
                .runs
                .lock()
                .map_err(|_| EventStoreError::LockPoisoned)?;
            runs.get(run_id).cloned()
        };
        let Some(run) = run else {
            return Ok(Vec::new());
        };
        let run = run.lock().map_err(|_| EventStoreError::LockPoisoned)?;
        let requested: HashSet<&str> = call_ids.iter().map(String::as_str).collect();
        Ok(run
            .iter()
            .flat_map(|stored| &stored.events)
            .filter(|envelope| {
                matches!(&envelope.event, RunEvent::ToolCallRequested { call_id, .. }
                    if requested.contains(call_id.as_str()))
            })
            .cloned()
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendDecision {
    Append,
    AlreadyPresent,
}

fn classify_append(
    stored: &StoredRun,
    run_id: &str,
    agent_id: &str,
    event: &RunEventEnvelope,
) -> Result<AppendDecision, EventStoreError> {
    if stored.agent_id != agent_id {
        return Err(EventStoreError::AgentMismatch {
            run_id: run_id.to_owned(),
            expected: stored.agent_id.clone(),
            actual: agent_id.to_owned(),
        });
    }

    let expected = match stored.events.last() {
        Some(last) => {
            last.sequence
                .checked_add(1)
                .ok_or_else(|| EventStoreError::SequenceOverflow {
                    run_id: run_id.to_owned(),
                })?
        }
        None => 0,
    };

    if event.sequence == expected {
        return Ok(AppendDecision::Append);
    }

    if event.sequence < expected
        && usize::try_from(event.sequence)
            .ok()
            .and_then(|index| stored.events.get(index))
            == Some(event)
    {
        return Ok(AppendDecision::AlreadyPresent);
    }

    Err(EventStoreError::SequenceConflict {
        run_id: run_id.to_owned(),
        expected,
        actual: event.sequence,
    })
}
