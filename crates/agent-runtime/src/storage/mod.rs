//! Event persistence contracts and local store implementations.

mod images;
mod jsonl;
mod memory;

pub use jsonl::JsonlEventStore;
pub use memory::InMemoryEventStore;

use std::{collections::HashSet, error::Error, fmt, future::Future, ops::Range, path::PathBuf};

use agent_core::agent::{RunEvent, RunEventEnvelope};
use tokio::task::JoinError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended,
    AlreadyPresent,
}

/// A run's identity and the events returned by a storage query.
///
/// [`EventStore::load`] returns its complete history, while
/// [`EventStore::load_range`] may return only a subset. Range results keep the
/// original event sequence numbers and are not necessarily valid for restoring
/// an [`agent_core::agent::AgentRun`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRun {
    pub run_id: String,
    pub agent_id: String,
    pub events: Vec<RunEventEnvelope>,
}

pub trait EventStore: Send + Sync {
    fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> impl Future<Output = Result<AppendOutcome, EventStoreError>> + Send;

    fn load(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<Option<StoredRun>, EventStoreError>> + Send;

    /// Restore active execution without retaining historical screenshot bytes.
    /// The latest image group remains inline; older images retain content IDs.
    /// Public `load` and `load_range` still return complete image payloads.
    fn load_for_runtime(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<Option<StoredRun>, EventStoreError>> + Send {
        async move {
            Ok(self.load(run_id).await?.map(|mut stored| {
                agent_core::agent::release_historical_image_data(&mut stored.events);
                stored
            }))
        }
    }

    /// Load a half-open sequence range. An existing run returns `Some`, even
    /// when the range is empty or lies beyond its last event.
    ///
    /// The default keeps custom stores compatible; indexed stores should
    /// override it to avoid loading unrelated event payloads.
    fn load_range(
        &self,
        run_id: &str,
        sequences: Range<u64>,
    ) -> impl Future<Output = Result<Option<StoredRun>, EventStoreError>> + Send {
        async move {
            Ok(self.load(run_id).await?.map(|mut stored| {
                stored
                    .events
                    .retain(|event| sequences.contains(&event.sequence));
                stored
            }))
        }
    }

    /// Load the request events associated with these tool call IDs in sequence
    /// order. Missing IDs produce no events. Repeated stored IDs retain every
    /// request, while repeated IDs in the query do not duplicate results.
    ///
    /// This lets a range reader resolve result metadata without loading
    /// unrelated result payloads. The default remains compatible with stores
    /// that only implement `load`.
    fn load_tool_calls(
        &self,
        run_id: &str,
        call_ids: &[String],
    ) -> impl Future<Output = Result<Vec<RunEventEnvelope>, EventStoreError>> + Send {
        async move {
            if call_ids.is_empty() {
                return Ok(Vec::new());
            }
            let requested: HashSet<&str> = call_ids.iter().map(String::as_str).collect();
            Ok(self
                .load(run_id)
                .await?
                .into_iter()
                .flat_map(|stored| stored.events)
                .filter(|envelope| {
                    matches!(&envelope.event, RunEvent::ToolCallRequested { call_id, .. }
                        if requested.contains(call_id.as_str()))
                })
                .collect())
        }
    }
}

fn bounded_range(sequences: Range<u64>, event_count: usize) -> Range<usize> {
    let end = sequences.end.min(event_count as u64) as usize;
    let start = sequences.start.min(end as u64) as usize;
    start..end
}

fn validate_first_event(run_id: &str, envelope: &RunEventEnvelope) -> Result<(), EventStoreError> {
    if envelope.sequence != 0
        || envelope.step != 0
        || !matches!(envelope.event, RunEvent::RunStarted { .. })
    {
        return Err(EventStoreError::InvalidFirstEvent {
            run_id: run_id.to_owned(),
        });
    }

    Ok(())
}

#[derive(Debug)]
pub enum EventStoreError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Task(JoinError),
    Backend(Box<dyn Error + Send + Sync>),
    Serialization(serde_json::Error),
    CorruptEvent {
        path: PathBuf,
        line: usize,
        source: serde_json::Error,
    },
    FileSizeOverflow {
        path: PathBuf,
    },
    UnsupportedSchemaVersion {
        path: PathBuf,
        line: usize,
        version: u32,
    },
    InvalidFirstEvent {
        run_id: String,
    },
    RunIdMismatch {
        expected: String,
        actual: String,
    },
    AgentMismatch {
        run_id: String,
        expected: String,
        actual: String,
    },
    SequenceConflict {
        run_id: String,
        expected: u64,
        actual: u64,
    },
    SequenceOverflow {
        run_id: String,
    },
    LockPoisoned,
}

impl EventStoreError {
    pub fn backend(source: impl Error + Send + Sync + 'static) -> Self {
        Self::Backend(Box::new(source))
    }

    fn io(operation: &'static str, path: PathBuf, source: std::io::Error) -> EventStoreError {
        Self::Io {
            operation,
            path,
            source,
        }
    }
}

impl fmt::Display for EventStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} event log {}: {source}",
                path.display()
            ),
            Self::Task(source) => write!(formatter, "event store task failed: {source}"),
            Self::Backend(source) => write!(formatter, "event store backend failed: {source}"),
            Self::Serialization(source) => write!(formatter, "failed to serialize event: {source}"),
            Self::CorruptEvent { path, line, source } => write!(
                formatter,
                "event log {} contains invalid JSON at line {line}: {source}",
                path.display()
            ),
            Self::FileSizeOverflow { path } => {
                write!(formatter, "event log {} is too large", path.display())
            }
            Self::UnsupportedSchemaVersion {
                path,
                line,
                version,
            } => write!(
                formatter,
                "event log {} uses unsupported schema version {version} at line {line}",
                path.display()
            ),
            Self::InvalidFirstEvent { run_id } => write!(
                formatter,
                "the first event for run {run_id} must be RunStarted at sequence 0 and step 0"
            ),
            Self::RunIdMismatch { expected, actual } => write!(
                formatter,
                "event log belongs to run {actual}, expected run {expected}"
            ),
            Self::AgentMismatch {
                run_id,
                expected,
                actual,
            } => write!(
                formatter,
                "run {run_id} belongs to agent {expected}, not agent {actual}"
            ),
            Self::SequenceConflict {
                run_id,
                expected,
                actual,
            } => write!(
                formatter,
                "run {run_id} expected event sequence {expected}, got {actual}"
            ),
            Self::SequenceOverflow { run_id } => {
                write!(formatter, "event sequence for run {run_id} overflowed")
            }
            Self::LockPoisoned => formatter.write_str("event store lock was poisoned"),
        }
    }
}

impl Error for EventStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Task(source) => Some(source),
            Self::Backend(source) => Some(source.as_ref()),
            Self::Serialization(source) | Self::CorruptEvent { source, .. } => Some(source),
            Self::FileSizeOverflow { .. }
            | Self::UnsupportedSchemaVersion { .. }
            | Self::InvalidFirstEvent { .. }
            | Self::RunIdMismatch { .. }
            | Self::AgentMismatch { .. }
            | Self::SequenceConflict { .. }
            | Self::SequenceOverflow { .. }
            | Self::LockPoisoned => None,
        }
    }
}
