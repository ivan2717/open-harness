use std::{
    collections::HashMap,
    error::Error,
    fmt::{self, Write as _},
    fs::{self, File, OpenOptions},
    future::Future,
    io::{BufRead, BufReader, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use agent_core::agent::{RunEvent, RunEventEnvelope};
use serde::{Deserialize, Serialize};
use tokio::task::JoinError;

const EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended,
    AlreadyPresent,
}

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
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryEventStore {
    runs: Arc<Mutex<HashMap<String, StoredRun>>>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventStore for InMemoryEventStore {
    fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> impl Future<Output = Result<AppendOutcome, EventStoreError>> + Send {
        let runs = Arc::clone(&self.runs);
        let run_id = run_id.to_owned();
        let agent_id = agent_id.to_owned();
        let event = event.clone();

        async move {
            let mut runs = runs.lock().map_err(|_| EventStoreError::LockPoisoned)?;

            match runs.get_mut(&run_id) {
                Some(stored) => match classify_append(stored, &run_id, &agent_id, &event)? {
                    AppendDecision::Append => {
                        stored.events.push(event);
                        Ok(AppendOutcome::Appended)
                    }
                    AppendDecision::AlreadyPresent => Ok(AppendOutcome::AlreadyPresent),
                },
                None => {
                    validate_first_event(&run_id, &event)?;
                    runs.insert(
                        run_id.clone(),
                        StoredRun {
                            run_id,
                            agent_id,
                            events: vec![event],
                        },
                    );
                    Ok(AppendOutcome::Appended)
                }
            }
        }
    }

    fn load(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<Option<StoredRun>, EventStoreError>> + Send {
        let runs = Arc::clone(&self.runs);
        let run_id = run_id.to_owned();

        async move {
            let runs = runs.lock().map_err(|_| EventStoreError::LockPoisoned)?;
            Ok(runs.get(&run_id).cloned())
        }
    }
}

/// A persistent JSON Lines store for local development and single-process use.
///
/// Clones share one writer lock. Independently constructed stores and separate
/// processes must not write to the same directory concurrently.
/// A trailing newline is the commit marker for each record. Loading repairs an
/// interrupted final write by truncating its unterminated tail; terminated but
/// invalid records are reported as corruption.
#[derive(Debug, Clone)]
pub struct JsonlEventStore {
    directory: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl JsonlEventStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl EventStore for JsonlEventStore {
    fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> impl Future<Output = Result<AppendOutcome, EventStoreError>> + Send {
        let directory = self.directory.clone();
        let lock = Arc::clone(&self.lock);
        let run_id = run_id.to_owned();
        let agent_id = agent_id.to_owned();
        let event = event.clone();

        async move {
            tokio::task::spawn_blocking(move || {
                let _guard = lock.lock().map_err(|_| EventStoreError::LockPoisoned)?;
                append_jsonl_event(&directory, &run_id, &agent_id, &event)
            })
            .await
            .map_err(EventStoreError::Task)?
        }
    }

    fn load(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<Option<StoredRun>, EventStoreError>> + Send {
        let directory = self.directory.clone();
        let lock = Arc::clone(&self.lock);
        let run_id = run_id.to_owned();

        async move {
            tokio::task::spawn_blocking(move || {
                let _guard = lock.lock().map_err(|_| EventStoreError::LockPoisoned)?;
                load_jsonl_run(&directory, &run_id)
            })
            .await
            .map_err(EventStoreError::Task)?
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonlEventRecord {
    schema_version: u32,
    run_id: String,
    agent_id: String,
    envelope: RunEventEnvelope,
}

#[derive(Debug, Deserialize)]
struct JsonlRecordHeader {
    schema_version: u32,
}

fn append_jsonl_event(
    directory: &Path,
    run_id: &str,
    agent_id: &str,
    event: &RunEventEnvelope,
) -> Result<AppendOutcome, EventStoreError> {
    if let Some(stored) = load_jsonl_run(directory, run_id)? {
        if classify_append(&stored, run_id, agent_id, event)? == AppendDecision::AlreadyPresent {
            let path = jsonl_path(directory, run_id);
            File::open(&path)
                .and_then(|file| file.sync_data())
                .map_err(|source| EventStoreError::io("confirm durability of", path, source))?;
            return Ok(AppendOutcome::AlreadyPresent);
        }
    } else {
        validate_first_event(run_id, event)?;
    }

    fs::create_dir_all(directory)
        .map_err(|source| EventStoreError::io("create directory", directory.to_owned(), source))?;

    let path = jsonl_path(directory, run_id);
    let record = JsonlEventRecord {
        schema_version: EVENT_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        agent_id: agent_id.to_owned(),
        envelope: event.clone(),
    };
    let mut encoded = serde_json::to_vec(&record).map_err(EventStoreError::Serialization)?;
    encoded.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| EventStoreError::io("open for append", path.clone(), source))?;

    file.write_all(&encoded)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_data())
        .map_err(|source| EventStoreError::io("append", path, source))?;

    Ok(AppendOutcome::Appended)
}

fn load_jsonl_run(directory: &Path, run_id: &str) -> Result<Option<StoredRun>, EventStoreError> {
    let path = jsonl_path(directory, run_id);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(EventStoreError::io("open", path, source)),
    };

    let mut stored: Option<StoredRun> = None;
    let mut reader = BufReader::new(file);
    let mut line_number = 0;
    let mut committed_length = 0_u64;

    loop {
        let mut line = Vec::new();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| EventStoreError::io("read", path.clone(), source))?;
        if bytes_read == 0 {
            break;
        }

        line_number += 1;
        if !line.ends_with(b"\n") {
            drop(reader);
            truncate_torn_tail(&path, committed_length)?;
            return Ok(stored);
        }

        let header: JsonlRecordHeader =
            serde_json::from_slice(&line).map_err(|source| EventStoreError::CorruptEvent {
                path: path.clone(),
                line: line_number,
                source,
            })?;

        if header.schema_version != EVENT_SCHEMA_VERSION {
            return Err(EventStoreError::UnsupportedSchemaVersion {
                path,
                line: line_number,
                version: header.schema_version,
            });
        }

        let record: JsonlEventRecord =
            serde_json::from_slice(&line).map_err(|source| EventStoreError::CorruptEvent {
                path: path.clone(),
                line: line_number,
                source,
            })?;

        if record.run_id != run_id {
            return Err(EventStoreError::RunIdMismatch {
                expected: run_id.to_owned(),
                actual: record.run_id,
            });
        }

        match &mut stored {
            Some(stored) => {
                match classify_append(stored, run_id, &record.agent_id, &record.envelope)? {
                    AppendDecision::Append => stored.events.push(record.envelope),
                    AppendDecision::AlreadyPresent => {
                        return Err(EventStoreError::SequenceConflict {
                            run_id: run_id.to_owned(),
                            expected: stored.events.len() as u64,
                            actual: record.envelope.sequence,
                        });
                    }
                }
            }
            None => {
                validate_first_event(run_id, &record.envelope)?;
                stored = Some(StoredRun {
                    run_id: record.run_id,
                    agent_id: record.agent_id,
                    events: vec![record.envelope],
                });
            }
        }

        committed_length = committed_length
            .checked_add(bytes_read as u64)
            .ok_or_else(|| EventStoreError::FileSizeOverflow { path: path.clone() })?;
    }

    Ok(stored)
}

fn truncate_torn_tail(path: &Path, committed_length: u64) -> Result<(), EventStoreError> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| {
            EventStoreError::io("open for torn-write recovery", path.to_owned(), source)
        })?;

    file.set_len(committed_length)
        .and_then(|_| file.sync_all())
        .map_err(|source| EventStoreError::io("recover torn write", path.to_owned(), source))
}

fn jsonl_path(directory: &Path, run_id: &str) -> PathBuf {
    let mut encoded = String::with_capacity(run_id.len() * 2);
    for byte in run_id.as_bytes() {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }

    directory.join(format!("run-{encoded}.jsonl"))
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
