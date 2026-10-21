use std::{
    collections::HashMap,
    fmt::Write as _,
    fs::{self, File, Metadata, OpenOptions},
    future::Future,
    io::{BufRead, BufReader, Seek, SeekFrom, Write as _},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use agent_core::agent::{RunEvent, RunEventEnvelope};
use serde::{Deserialize, Serialize};

use super::{
    AppendOutcome, EventStore, EventStoreError, StoredRun, bounded_range, validate_first_event,
};

const EVENT_SCHEMA_VERSION: u32 = 1;

/// A persistent JSON Lines store for local development and single-process use.
///
/// Clones share an offset index and writer lock per run, so unrelated runs do
/// not block one another during disk I/O. Indexes retain no event payloads.
/// Independently constructed stores and separate processes must not write to
/// the same directory concurrently. External changes invalidate the index when
/// the file metadata changes; simultaneous external writes are unsupported.
/// A trailing newline is the commit marker for each record. Loading repairs an
/// interrupted final write by truncating its unterminated tail; terminated but
/// invalid records are reported as corruption.
#[derive(Debug, Clone)]
pub struct JsonlEventStore {
    directory: PathBuf,
    runs: Arc<Mutex<HashMap<String, Arc<Mutex<JsonlRunIndex>>>>>,
}

impl JsonlEventStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn run_index(&self, run_id: &str) -> Result<Arc<Mutex<JsonlRunIndex>>, EventStoreError> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| EventStoreError::LockPoisoned)?;
        Ok(Arc::clone(runs.entry(run_id.to_owned()).or_default()))
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
        let index = self.run_index(run_id);
        let run_id = run_id.to_owned();
        let agent_id = agent_id.to_owned();
        let event = event.clone();

        async move {
            let index = index?;
            tokio::task::spawn_blocking(move || {
                let mut index = index.lock().map_err(|_| EventStoreError::LockPoisoned)?;
                append_jsonl_event(&directory, &run_id, &agent_id, &event, &mut index)
            })
            .await
            .map_err(EventStoreError::Task)?
        }
    }

    fn load(
        &self,
        run_id: &str,
    ) -> impl Future<Output = Result<Option<StoredRun>, EventStoreError>> + Send {
        self.load_range(run_id, 0..u64::MAX)
    }

    fn load_range(
        &self,
        run_id: &str,
        sequences: Range<u64>,
    ) -> impl Future<Output = Result<Option<StoredRun>, EventStoreError>> + Send {
        let directory = self.directory.clone();
        let index = self.run_index(run_id);
        let run_id = run_id.to_owned();

        async move {
            let index = index?;
            tokio::task::spawn_blocking(move || {
                let mut index = index.lock().map_err(|_| EventStoreError::LockPoisoned)?;
                load_jsonl_range(&directory, &run_id, sequences, &mut index)
            })
            .await
            .map_err(EventStoreError::Task)?
        }
    }

    fn load_tool_calls(
        &self,
        run_id: &str,
        call_ids: &[String],
    ) -> impl Future<Output = Result<Vec<RunEventEnvelope>, EventStoreError>> + Send {
        let directory = self.directory.clone();
        let index = self.run_index(run_id);
        let run_id = run_id.to_owned();
        let call_ids = call_ids.to_vec();

        async move {
            if call_ids.is_empty() {
                return Ok(Vec::new());
            }
            let index = index?;
            tokio::task::spawn_blocking(move || {
                let mut index = index.lock().map_err(|_| EventStoreError::LockPoisoned)?;
                load_jsonl_tool_calls(&directory, &run_id, &call_ids, &mut index)
            })
            .await
            .map_err(EventStoreError::Task)?
        }
    }
}

#[derive(Debug, Default)]
struct JsonlRunIndex {
    agent_id: Option<String>,
    offsets: Vec<u64>,
    tool_calls: HashMap<String, Vec<usize>>,
    committed_length: u64,
    stamp: Option<FileStamp>,
    #[cfg(test)]
    scanned_records: usize,
    #[cfg(test)]
    loaded_records: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
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
    index: &mut JsonlRunIndex,
) -> Result<AppendOutcome, EventStoreError> {
    let path = jsonl_path(directory, run_id);
    refresh_jsonl_index(&path, run_id, index)?;
    if let Some(existing_agent) = &index.agent_id {
        if existing_agent != agent_id {
            return Err(EventStoreError::AgentMismatch {
                run_id: run_id.to_owned(),
                expected: existing_agent.clone(),
                actual: agent_id.to_owned(),
            });
        }
        let expected = index.offsets.len() as u64;
        if event.sequence < expected {
            let existing = read_indexed_event(&path, run_id, index, event.sequence as usize)?;
            if existing == *event {
                File::open(&path)
                    .and_then(|file| file.sync_data())
                    .map_err(|source| EventStoreError::io("confirm durability of", path, source))?;
                return Ok(AppendOutcome::AlreadyPresent);
            }
        }
        if event.sequence != expected {
            return Err(EventStoreError::SequenceConflict {
                run_id: run_id.to_owned(),
                expected,
                actual: event.sequence,
            });
        }
    } else {
        validate_first_event(run_id, event)?;
    }

    fs::create_dir_all(directory)
        .map_err(|source| EventStoreError::io("create directory", directory.to_owned(), source))?;

    let record = JsonlEventRecord {
        schema_version: EVENT_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        agent_id: agent_id.to_owned(),
        envelope: event.clone(),
    };
    let mut encoded = serde_json::to_vec(&record).map_err(EventStoreError::Serialization)?;
    encoded.push(b'\n');
    let next_length = index
        .committed_length
        .checked_add(encoded.len() as u64)
        .ok_or_else(|| EventStoreError::FileSizeOverflow { path: path.clone() })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| EventStoreError::io("open for append", path.clone(), source))?;

    // An uncertain or partial write must force revalidation on the next call.
    index.stamp = None;
    file.write_all(&encoded)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_data())
        .map_err(|source| EventStoreError::io("append", path.clone(), source))?;
    let metadata = file
        .metadata()
        .map_err(|source| EventStoreError::io("inspect", path, source))?;
    index.offsets.push(index.committed_length);
    if let RunEvent::ToolCallRequested { call_id, .. } = &event.event {
        index
            .tool_calls
            .entry(call_id.clone())
            .or_default()
            .push(index.offsets.len() - 1);
    }
    index.committed_length = next_length;
    index.agent_id.get_or_insert_with(|| agent_id.to_owned());
    index.stamp = Some(FileStamp::from_metadata(&metadata));
    Ok(AppendOutcome::Appended)
}

fn refresh_jsonl_index(
    path: &Path,
    run_id: &str,
    index: &mut JsonlRunIndex,
) -> Result<(), EventStoreError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            *index = JsonlRunIndex::default();
            return Ok(());
        }
        Err(source) => return Err(EventStoreError::io("open", path.to_owned(), source)),
    };
    let metadata = file
        .metadata()
        .map_err(|source| EventStoreError::io("inspect", path.to_owned(), source))?;
    let stamp = FileStamp::from_metadata(&metadata);
    if index.stamp.as_ref() == Some(&stamp) {
        return Ok(());
    }

    // Build a replacement index so corruption never leaves a partial index
    // usable by subsequent append calls.
    let mut replacement = JsonlRunIndex::default();
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| EventStoreError::io("read", path.to_owned(), source))?;
        if bytes_read == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            truncate_torn_tail(path, replacement.committed_length)?;
            break;
        }
        let sequence = replacement.offsets.len();
        let record = decode_jsonl_record(path, run_id, sequence + 1, &line)?;
        if let Some(existing_agent) = &replacement.agent_id {
            if existing_agent != &record.agent_id {
                return Err(EventStoreError::AgentMismatch {
                    run_id: run_id.to_owned(),
                    expected: existing_agent.clone(),
                    actual: record.agent_id,
                });
            }
            if record.envelope.sequence != sequence as u64 {
                return Err(EventStoreError::SequenceConflict {
                    run_id: run_id.to_owned(),
                    expected: sequence as u64,
                    actual: record.envelope.sequence,
                });
            }
        } else {
            validate_first_event(run_id, &record.envelope)?;
            replacement.agent_id = Some(record.agent_id);
        }
        replacement.offsets.push(replacement.committed_length);
        if let RunEvent::ToolCallRequested { call_id, .. } = record.envelope.event {
            replacement
                .tool_calls
                .entry(call_id)
                .or_default()
                .push(sequence);
        }
        replacement.committed_length = replacement
            .committed_length
            .checked_add(bytes_read as u64)
            .ok_or_else(|| EventStoreError::FileSizeOverflow {
                path: path.to_owned(),
            })?;
    }
    let metadata = reader
        .get_ref()
        .metadata()
        .map_err(|source| EventStoreError::io("inspect", path.to_owned(), source))?;
    replacement.stamp = Some(FileStamp::from_metadata(&metadata));
    #[cfg(test)]
    {
        replacement.scanned_records = index.scanned_records + replacement.offsets.len();
        replacement.loaded_records = index.loaded_records;
    }
    *index = replacement;
    Ok(())
}

fn decode_jsonl_record(
    path: &Path,
    run_id: &str,
    line_number: usize,
    line: &[u8],
) -> Result<JsonlEventRecord, EventStoreError> {
    let header: JsonlRecordHeader =
        serde_json::from_slice(line).map_err(|source| EventStoreError::CorruptEvent {
            path: path.to_owned(),
            line: line_number,
            source,
        })?;
    if header.schema_version != EVENT_SCHEMA_VERSION {
        return Err(EventStoreError::UnsupportedSchemaVersion {
            path: path.to_owned(),
            line: line_number,
            version: header.schema_version,
        });
    }
    let record: JsonlEventRecord =
        serde_json::from_slice(line).map_err(|source| EventStoreError::CorruptEvent {
            path: path.to_owned(),
            line: line_number,
            source,
        })?;
    if record.run_id != run_id {
        return Err(EventStoreError::RunIdMismatch {
            expected: run_id.to_owned(),
            actual: record.run_id,
        });
    }
    Ok(record)
}

fn read_indexed_event(
    path: &Path,
    run_id: &str,
    index: &mut JsonlRunIndex,
    sequence: usize,
) -> Result<RunEventEnvelope, EventStoreError> {
    let mut file =
        File::open(path).map_err(|source| EventStoreError::io("open", path.to_owned(), source))?;
    file.seek(SeekFrom::Start(index.offsets[sequence]))
        .map_err(|source| EventStoreError::io("seek", path.to_owned(), source))?;
    let mut line = Vec::new();
    BufReader::new(file)
        .read_until(b'\n', &mut line)
        .map_err(|source| EventStoreError::io("read", path.to_owned(), source))?;
    #[cfg(test)]
    {
        index.loaded_records += 1;
    }
    Ok(decode_jsonl_record(path, run_id, sequence + 1, &line)?.envelope)
}

fn load_jsonl_range(
    directory: &Path,
    run_id: &str,
    sequences: Range<u64>,
    index: &mut JsonlRunIndex,
) -> Result<Option<StoredRun>, EventStoreError> {
    let path = jsonl_path(directory, run_id);
    refresh_jsonl_index(&path, run_id, index)?;
    let Some(agent_id) = &index.agent_id else {
        return Ok(None);
    };
    let range = bounded_range(sequences, index.offsets.len());
    let mut events = Vec::with_capacity(range.len());
    if !range.is_empty() {
        let mut file = File::open(&path)
            .map_err(|source| EventStoreError::io("open", path.clone(), source))?;
        file.seek(SeekFrom::Start(index.offsets[range.start]))
            .map_err(|source| EventStoreError::io("seek", path.clone(), source))?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        for sequence in range {
            line.clear();
            reader
                .read_until(b'\n', &mut line)
                .map_err(|source| EventStoreError::io("read", path.clone(), source))?;
            events.push(decode_jsonl_record(&path, run_id, sequence + 1, &line)?.envelope);
            #[cfg(test)]
            {
                index.loaded_records += 1;
            }
        }
    }
    Ok(Some(StoredRun {
        run_id: run_id.to_owned(),
        agent_id: agent_id.clone(),
        events,
    }))
}

fn load_jsonl_tool_calls(
    directory: &Path,
    run_id: &str,
    call_ids: &[String],
    index: &mut JsonlRunIndex,
) -> Result<Vec<RunEventEnvelope>, EventStoreError> {
    let path = jsonl_path(directory, run_id);
    refresh_jsonl_index(&path, run_id, index)?;
    let mut sequences: Vec<usize> = call_ids
        .iter()
        .filter_map(|call_id| index.tool_calls.get(call_id))
        .flatten()
        .copied()
        .collect();
    sequences.sort_unstable();
    sequences.dedup();
    sequences
        .into_iter()
        .map(|sequence| read_indexed_event(&path, run_id, index, sequence))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
            loop {
                let counter = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "open-harness-storage-index-{}-{counter}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("failed to create test directory: {error}"),
                }
            }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn append_reuses_offsets_and_range_reads_decode_only_requested_records() {
        let directory = TestDirectory::new();
        let mut index = JsonlRunIndex::default();
        let first = RunEventEnvelope {
            sequence: 0,
            step: 0,
            occurred_at: SystemTime::now(),
            event: RunEvent::RunStarted {
                instruction: "Persist a large history".to_owned(),
            },
        };
        append_jsonl_event(&directory.0, "run", "agent", &first, &mut index).unwrap();
        for sequence in 1..=32 {
            let event = RunEventEnvelope {
                sequence,
                step: 1,
                occurred_at: SystemTime::now(),
                event: match sequence {
                    5 | 25 => RunEvent::ToolCallRequested {
                        call_id: format!("call-{sequence}"),
                        tool_name: "read_file".to_owned(),
                        arguments: "{}".to_owned(),
                    },
                    20 => RunEvent::ToolCallSucceeded {
                        call_id: "call-5".to_owned(),
                        output: "x".repeat(64 * 1024),
                    },
                    _ => RunEvent::ModelResponseReceived {
                        response: None,
                        provider_continuation: None,
                        provider_state: None,
                        content: "x".repeat(64 * 1024),
                    },
                },
            };
            append_jsonl_event(&directory.0, "run", "agent", &event, &mut index).unwrap();
        }
        assert_eq!(index.offsets.len(), 33);
        // Normal append work does not depend on the size of the prior history.
        assert_eq!(index.scanned_records, 0);
        assert_eq!(index.loaded_records, 0);

        let mut reopened = JsonlRunIndex::default();
        let range = load_jsonl_range(&directory.0, "run", 20..22, &mut reopened)
            .unwrap()
            .unwrap();
        assert_eq!(range.events.len(), 2);
        assert_eq!(range.events[0].sequence, 20);
        assert_eq!(reopened.scanned_records, 33);
        assert_eq!(reopened.loaded_records, 2);
        let requests = load_jsonl_tool_calls(
            &directory.0,
            "run",
            &[
                "call-25".to_owned(),
                "call-5".to_owned(),
                "call-5".to_owned(),
            ],
            &mut reopened,
        )
        .unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![5, 25]
        );
        // Resolving tool identities decodes two small request records, never
        // the unrelated large result payloads between them.
        assert_eq!(reopened.scanned_records, 33);
        assert_eq!(reopened.loaded_records, 4);

        let next = RunEventEnvelope {
            sequence: 33,
            step: 1,
            occurred_at: SystemTime::now(),
            event: RunEvent::RunCancelled,
        };
        append_jsonl_event(&directory.0, "run", "agent", &next, &mut reopened).unwrap();
        assert_eq!(reopened.scanned_records, 33);
        assert_eq!(
            append_jsonl_event(&directory.0, "run", "agent", &first, &mut reopened).unwrap(),
            AppendOutcome::AlreadyPresent
        );
        assert_eq!(reopened.scanned_records, 33);
        assert_eq!(reopened.loaded_records, 5);
    }

    #[test]
    fn writer_locks_are_shared_by_clones_but_independent_between_runs() {
        let directory = TestDirectory::new();
        let store = JsonlEventStore::new(&directory.0);
        let clone = store.clone();
        let first = store.run_index("run-a").unwrap();
        let same_run = clone.run_index("run-a").unwrap();
        assert!(Arc::ptr_eq(&first, &same_run));
        let _first_guard = first.lock().unwrap();
        assert!(same_run.try_lock().is_err());
        // Holding a run's lock cannot block resolving and locking another run.
        let second = clone.run_index("run-b").unwrap();
        assert!(second.try_lock().is_ok());
        assert!(store.runs.try_lock().is_ok());
    }
}
