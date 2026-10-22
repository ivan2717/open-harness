use super::{CompactingContextError, ContextSummary};
use agent_core::agent::RunEventEnvelope;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    future::Future,
    sync::{Arc, RwLock},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCheckpoint {
    pub run_id: String,
    pub id: String,
    /// Last summarized event when `summary` is nonempty. For pruning-only
    /// records this binds the source prefix and does not suppress any events.
    pub through_sequence: u64,
    /// Deterministic integrity fingerprint of the exact event prefix.
    pub source_fingerprint: String,
    /// Estimated tokens before summarization, using the same tool definitions
    /// as the compacted request so the comparison is like-for-like.
    pub before_tokens: usize,
    /// Estimated tokens in the resulting compacted request.
    pub after_tokens: usize,
    pub summary: ContextSummary,
    /// Exact derived replacements, saved independently of model summarization.
    /// Absent in older checkpoint records. Canonical events are never changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pruning: Option<ContextPruning>,
}

impl ContextCheckpoint {
    pub fn summary_through_sequence(&self) -> Option<u64> {
        (!self.summary.is_empty()).then_some(self.through_sequence)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPruning {
    pub through_sequence: u64,
    pub source_fingerprint: String,
    pub replacements: Vec<PrunedToolOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrunedToolOutput {
    pub sequence: u64,
    pub content: String,
    pub is_error: bool,
}

pub trait ContextCheckpointStore: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn save(
        &self,
        checkpoint: ContextCheckpoint,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Return the latest saved projection. A pruning-only record may cover
    /// a later event than a subsequently saved summary boundary.
    fn latest(
        &self,
        run_id: String,
    ) -> impl Future<Output = Result<Option<ContextCheckpoint>, Self::Error>> + Send;

    /// Discards derived summaries for one run so they can be rebuilt from the
    /// canonical event log after an integrity or schema mismatch.
    fn clear(&self, run_id: String) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryContextCheckpointStore {
    inner: Arc<RwLock<HashMap<String, Vec<ContextCheckpoint>>>>,
}

impl InMemoryContextCheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ContextCheckpointStore for InMemoryContextCheckpointStore {
    type Error = InMemoryContextCheckpointStoreError;

    fn save(
        &self,
        checkpoint: ContextCheckpoint,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let inner = Arc::clone(&self.inner);
        async move {
            let mut checkpoints = inner
                .write()
                .map_err(|_| InMemoryContextCheckpointStoreError::LockPoisoned)?;
            let run_checkpoints = checkpoints.entry(checkpoint.run_id.clone()).or_default();

            if let Some(existing) = run_checkpoints
                .iter()
                .find(|existing| existing.id == checkpoint.id)
            {
                if existing == &checkpoint {
                    return Ok(());
                }
                return Err(InMemoryContextCheckpointStoreError::Conflict {
                    run_id: checkpoint.run_id,
                    checkpoint_id: checkpoint.id,
                    through_sequence: checkpoint.through_sequence,
                });
            }

            run_checkpoints.push(checkpoint);
            Ok(())
        }
    }

    fn latest(
        &self,
        run_id: String,
    ) -> impl Future<Output = Result<Option<ContextCheckpoint>, Self::Error>> + Send {
        let inner = Arc::clone(&self.inner);
        async move {
            let checkpoints = inner
                .read()
                .map_err(|_| InMemoryContextCheckpointStoreError::LockPoisoned)?;
            Ok(checkpoints
                .get(&run_id)
                .and_then(|run_checkpoints| run_checkpoints.last().cloned()))
        }
    }

    fn clear(&self, run_id: String) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let inner = Arc::clone(&self.inner);
        async move {
            let mut checkpoints = inner
                .write()
                .map_err(|_| InMemoryContextCheckpointStoreError::LockPoisoned)?;
            checkpoints.remove(&run_id);
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InMemoryContextCheckpointStoreError {
    Conflict {
        run_id: String,
        checkpoint_id: String,
        through_sequence: u64,
    },
    LockPoisoned,
}

impl fmt::Display for InMemoryContextCheckpointStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict {
                run_id,
                checkpoint_id,
                through_sequence,
            } => write!(
                formatter,
                "checkpoint {checkpoint_id} for run {run_id} conflicts at sequence {through_sequence}"
            ),
            Self::LockPoisoned => write!(formatter, "in-memory checkpoint store lock was poisoned"),
        }
    }
}

impl Error for InMemoryContextCheckpointStoreError {}

pub(super) fn validate_event_order<CompactorError, CheckpointStoreError>(
    events: &[RunEventEnvelope],
) -> Result<(), CompactingContextError<CompactorError, CheckpointStoreError>> {
    for pair in events.windows(2) {
        if pair[1].sequence <= pair[0].sequence {
            return Err(CompactingContextError::EventSequenceNotIncreasing {
                previous: pair[0].sequence,
                current: pair[1].sequence,
            });
        }
    }
    Ok(())
}

pub(super) fn validate_checkpoint<CompactorError, CheckpointStoreError>(
    run_id: &str,
    events: &[RunEventEnvelope],
    checkpoint: Option<&ContextCheckpoint>,
) -> Result<(), CompactingContextError<CompactorError, CheckpointStoreError>> {
    let Some(checkpoint) = checkpoint else {
        return Ok(());
    };

    if checkpoint.run_id != run_id {
        return Err(CompactingContextError::CheckpointRunMismatch {
            expected_run_id: run_id.to_owned(),
            actual_run_id: checkpoint.run_id.clone(),
        });
    }
    if checkpoint.summary.is_empty() && checkpoint.pruning.is_none() {
        return Err(CompactingContextError::EmptyCheckpointSummary {
            checkpoint_id: checkpoint.id.clone(),
        });
    }

    let Some(index) = events
        .iter()
        .position(|event| event.sequence == checkpoint.through_sequence)
    else {
        return Err(CompactingContextError::CheckpointSequenceMissing {
            checkpoint_id: checkpoint.id.clone(),
            through_sequence: checkpoint.through_sequence,
        });
    };

    let actual_fingerprint =
        event_prefix_fingerprint(&events[..=index]).map_err(CompactingContextError::Fingerprint)?;
    if checkpoint.source_fingerprint != actual_fingerprint {
        return Err(CompactingContextError::CheckpointHistoryMismatch {
            checkpoint_id: checkpoint.id.clone(),
            expected_fingerprint: checkpoint.source_fingerprint.clone(),
            actual_fingerprint,
        });
    }

    if !checkpoint.summary.is_empty()
        && let Some(next) = events.get(index + 1)
        && next.step == events[index].step
    {
        return Err(CompactingContextError::CheckpointSplitsStep {
            checkpoint_id: checkpoint.id.clone(),
            through_sequence: checkpoint.through_sequence,
            step: next.step,
        });
    }

    if let Some(pruning) = &checkpoint.pruning {
        let Some(pruned_index) = events
            .iter()
            .position(|event| event.sequence == pruning.through_sequence)
        else {
            return Err(CompactingContextError::CheckpointSequenceMissing {
                checkpoint_id: checkpoint.id.clone(),
                through_sequence: pruning.through_sequence,
            });
        };
        let actual_fingerprint = event_prefix_fingerprint(&events[..=pruned_index])
            .map_err(CompactingContextError::Fingerprint)?;
        if pruning.source_fingerprint != actual_fingerprint
            || pruning.replacements.is_empty()
            || pruning.replacements.iter().any(|replacement| {
                !events[..=pruned_index].iter().any(|event| {
                    event.sequence == replacement.sequence
                        && matches!(
                            event.event,
                            agent_core::agent::RunEvent::ToolCallSucceeded { .. }
                                | agent_core::agent::RunEvent::ToolCallFailed { .. }
                        )
                })
            })
        {
            return Err(CompactingContextError::CheckpointHistoryMismatch {
                checkpoint_id: checkpoint.id.clone(),
                expected_fingerprint: pruning.source_fingerprint.clone(),
                actual_fingerprint,
            });
        }
    }

    Ok(())
}

pub(super) fn checkpoint_id(
    run_id: &str,
    through_sequence: u64,
    source_fingerprint: &str,
) -> String {
    format!("context:{run_id}:{through_sequence}:{source_fingerprint}")
}

pub(super) fn event_prefix_fingerprint(
    events: &[RunEventEnvelope],
) -> Result<String, serde_json::Error> {
    // Two independent deterministic FNV-1a streams make accidental reuse
    // vanishingly unlikely. This is an integrity key, not an authentication
    // primitive; authorization remains the EventStore's responsibility.
    let encoded = serde_json::to_vec(events)?;
    let mut first = 0xcbf2_9ce4_8422_2325_u64;
    let mut second = 0x8422_2325_cbf2_9ce4_u64;
    for byte in b"agent-runtime-context-v1".iter().chain(&encoded) {
        first ^= u64::from(*byte);
        first = first.wrapping_mul(0x0000_0100_0000_01b3);
        second ^= u64::from(*byte).rotate_left(1);
        second = second.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(format!("{first:016x}{second:016x}"))
}
