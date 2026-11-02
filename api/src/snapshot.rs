use crate::{
    contracts::{ChatMessage, RunDetail, RunStatus, RunSummary},
    projection, time,
};
use agent_core::agent::RunEventEnvelope;
use agent_runtime::{ModelStreamEvent, ModelStreamSink};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;

const STREAM_PUBLISH_INTERVAL: Duration = Duration::from_millis(50);
// Persist the upper bound before using revisions from this block. A restart
// starts beyond every provisional revision, without writing each token to disk.
const REVISION_RESERVATION: u64 = 1 << 20;

#[derive(Debug)]
struct SnapshotState {
    detail: RunDetail,
    full_subscribers: Option<watch::Sender<RunDetail>>,
    reserved_revision: u64,
    published_revision: u64,
    published_at: Instant,
    flush_scheduled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    inner: Arc<Mutex<SnapshotState>>,
    revisions: watch::Sender<u64>,
    path: PathBuf,
    steering: watch::Sender<bool>,
}

impl Snapshot {
    pub fn new(mut detail: RunDetail, path: PathBuf) -> io::Result<Self> {
        detail
            .messages
            .retain(|message| message.status != "streaming");
        let reserved_revision = reserve_revision(detail.summary.revision)?;
        persist(&path, &detail, reserved_revision)?;
        // Delivery is recovered from the event log before opening this snapshot.
        // Remove payloads left behind by a crash after delivery/removal.
        if let Ok(entries) = fs::read_dir(path.with_extension("inputs")) {
            for entry in entries.flatten() {
                let file = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_file())
                    && file
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    && file
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|id| !detail.pending_inputs.iter().any(|input| input.id == id))
                {
                    let _ = fs::remove_file(file);
                }
            }
        }
        let (revisions, _) = watch::channel(detail.summary.revision);
        let (steering, _) = watch::channel(has_steering(&detail));
        Ok(Self {
            inner: Arc::new(Mutex::new(SnapshotState {
                reserved_revision,
                published_revision: detail.summary.revision,
                detail,
                full_subscribers: None,
                published_at: Instant::now(),
                flush_scheduled: false,
            })),
            revisions,
            path,
            steering,
        })
    }
    pub fn get(&self) -> RunDetail {
        self.read(Clone::clone)
    }
    pub fn read<T>(&self, read: impl FnOnce(&RunDetail) -> T) -> T {
        read(&self.inner.lock().expect("snapshot lock poisoned").detail)
    }
    pub fn summary(&self) -> RunSummary {
        self.read(|detail| detail.summary.clone())
    }
    pub fn subscribe(&self) -> watch::Receiver<RunDetail> {
        // Legacy consumers receive the latest state even when there previously
        // were no full-snapshot subscribers. New UI streams use revision notices.
        let mut current = self.inner.lock().expect("snapshot lock poisoned");
        if let Some(sender) = &current.full_subscribers {
            sender.send_replace(current.detail.clone());
            sender.subscribe()
        } else {
            let (sender, receiver) = watch::channel(current.detail.clone());
            current.full_subscribers = Some(sender);
            receiver
        }
    }
    pub fn subscribe_revision(&self) -> watch::Receiver<u64> {
        self.revisions.subscribe()
    }
    fn input_files_path(&self, id: &str) -> PathBuf {
        self.path
            .with_extension("inputs")
            .join(format!("{id}.json"))
    }
    /// Large queued attachments are immutable sidecars, not part of every
    /// streaming UI revision. The visible queue carries names/references only.
    pub fn save_input_files(
        &self,
        id: &str,
        files: &[agent_core::TextFileReference],
    ) -> io::Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(self.path.with_extension("inputs"))?;
        write_json(&self.input_files_path(id), &files)
    }
    pub fn remove_input_files(&self, id: &str) {
        let _ = fs::remove_file(self.input_files_path(id));
    }
    pub fn update(&self, update: impl FnOnce(&mut RunDetail)) -> io::Result<()> {
        self.try_update(|detail| {
            update(detail);
            Ok::<_, io::Error>(())
        })
    }
    pub fn try_update<T, E: From<io::Error>>(
        &self,
        update: impl FnOnce(&mut RunDetail) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut current = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("snapshot lock poisoned"))?;
        let mut detail = current.detail.clone();
        let result = update(&mut detail)?;
        detail.summary.revision = next_revision(detail.summary.revision)?;
        detail.summary.updated_at = time::now();
        let reserved_revision = if detail.summary.revision >= current.reserved_revision {
            reserve_revision(detail.summary.revision)?
        } else {
            current.reserved_revision
        };
        persist(&self.path, &detail, reserved_revision)?;
        self.steering.send_replace(has_steering(&detail));
        current.detail = detail;
        current.reserved_revision = reserved_revision;
        self.publish(&mut current);
        Ok(result)
    }
    pub fn event(&self, event: &RunEventEnvelope) -> io::Result<()> {
        if self
            .inner
            .lock()
            .map_err(|_| io::Error::other("snapshot lock poisoned"))?
            .detail
            .events
            .last()
            .is_some_and(|last| last.sequence >= event.sequence)
        {
            return Ok(());
        }
        self.update(|detail| projection::apply(detail, event))
    }

    fn publish(&self, current: &mut SnapshotState) {
        let revision = current.detail.summary.revision;
        if let Some(sender) = &current.full_subscribers {
            if sender.receiver_count() > 0 {
                sender.send_replace(current.detail.clone());
            } else {
                current.full_subscribers = None;
            }
        }
        self.revisions.send_replace(revision);
        current.published_revision = revision;
        current.published_at = Instant::now();
    }

    fn stream_delta(&self, text: &str, reasoning: bool) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let mut current = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("snapshot lock poisoned"))?;
        if current.detail.summary.status != RunStatus::Running {
            return Ok(());
        }
        let revision = next_revision(current.detail.summary.revision)?;
        if revision >= current.reserved_revision {
            let reserved_revision = reserve_revision(revision)?;
            // Extremely long streams can exhaust a reservation. Renew only its
            // durable high-water mark; provisional content still never persists.
            persist(&self.path, &current.detail, reserved_revision)?;
            current.reserved_revision = reserved_revision;
        }
        let message = streaming_message(&mut current.detail);
        if reasoning {
            message.reasoning.get_or_insert_default().push_str(text);
        } else {
            message.text.push_str(text);
        }
        current.detail.summary.revision = revision;
        current.detail.summary.updated_at = time::now();
        let remaining = STREAM_PUBLISH_INTERVAL.saturating_sub(current.published_at.elapsed());
        if remaining.is_zero() {
            self.publish(&mut current);
        } else if !current.flush_scheduled {
            current.flush_scheduled = true;
            self.schedule_flush(remaining);
        }
        Ok(())
    }

    fn schedule_flush(&self, delay: Duration) {
        let snapshot = self.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let mut delay = delay;
                loop {
                    tokio::time::sleep(delay).await;
                    match snapshot.flush_stream() {
                        Some(remaining) => delay = remaining,
                        None => break,
                    }
                }
            });
        } else {
            // ModelStreamSink is synchronous and can also be used by a provider
            // on a plain worker thread. It must not require an entered runtime.
            std::thread::spawn(move || {
                let mut delay = delay;
                loop {
                    std::thread::sleep(delay);
                    match snapshot.flush_stream() {
                        Some(remaining) => delay = remaining,
                        None => break,
                    }
                }
            });
        }
    }

    fn flush_stream(&self) -> Option<Duration> {
        let mut current = self.inner.lock().expect("snapshot lock poisoned");
        if current.detail.summary.revision != current.published_revision {
            let remaining = STREAM_PUBLISH_INTERVAL.saturating_sub(current.published_at.elapsed());
            if !remaining.is_zero() {
                return Some(remaining);
            }
            // Read the current state, never a captured delta: a durable terminal
            // event may already have replaced or removed the streaming message.
            self.publish(&mut current);
        }
        current.flush_scheduled = false;
        None
    }
}

fn next_revision(revision: u64) -> io::Result<u64> {
    revision
        .checked_add(1)
        .ok_or_else(|| io::Error::other("snapshot revision exhausted"))
}

fn reserve_revision(revision: u64) -> io::Result<u64> {
    revision
        .checked_add(REVISION_RESERVATION)
        .ok_or_else(|| io::Error::other("snapshot revision exhausted"))
}

fn persist(path: &Path, detail: &RunDetail, reserved_revision: u64) -> io::Result<()> {
    let mut durable = detail.clone();
    durable
        .messages
        .retain(|message| message.status != "streaming");
    durable.summary.revision = reserved_revision;
    write_json(path, &durable)
}

fn has_steering(detail: &RunDetail) -> bool {
    !detail.pending_inputs_paused
        && detail
            .pending_inputs
            .iter()
            .any(|input| input.steer && !input.delivering)
}

impl agent_runtime::UserInputQueue for Snapshot {
    fn claim(&self) -> io::Result<Option<agent_runtime::PendingUserInput>> {
        if !self.read(has_steering) {
            return Ok(None);
        }
        let mut claimed = self.try_update(|detail| -> io::Result<_> {
            if detail.pending_inputs_paused {
                return Ok(None);
            }
            let input = detail
                .pending_inputs
                .iter_mut()
                .find(|input| !input.delivering && input.steer);
            Ok(input.map(|input| {
                input.delivering = true;
                input.clone()
            }))
        })?;
        if let Some(input) = &mut claimed {
            match fs::read(self.input_files_path(&input.id)) {
                Ok(bytes) => {
                    input.files = serde_json::from_slice(&bytes).map_err(io::Error::other)?
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound && input.files.is_empty() => {
                }
                Err(error) => return Err(error),
            }
        }
        Ok(claimed)
    }
    fn acknowledge(&self, id: &str) -> io::Result<()> {
        if self.read(|detail| detail.pending_inputs.iter().any(|input| input.id == id)) {
            self.update(|detail| detail.pending_inputs.retain(|input| input.id != id))?;
        }
        self.remove_input_files(id);
        Ok(())
    }
    fn subscribe(&self) -> watch::Receiver<bool> {
        self.steering.subscribe()
    }
}

impl ModelStreamSink for Snapshot {
    fn emit(&mut self, event: ModelStreamEvent) {
        let result = match event {
            ModelStreamEvent::TextDelta { text, .. } => self.stream_delta(&text, false),
            ModelStreamEvent::ReasoningDelta { text, .. } => self.stream_delta(&text, true),
            _ => return,
        };
        if let Err(error) = result {
            tracing::error!(%error, "could not update UI stream snapshot");
        }
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;

fn streaming_message(detail: &mut RunDetail) -> &mut ChatMessage {
    if detail
        .messages
        .last()
        .is_none_or(|message| message.status != "streaming")
    {
        let mut message = ChatMessage::complete(
            format!("stream-{}", detail.events.len()),
            "assistant",
            String::new(),
        );
        message.status = "streaming".into();
        message.started_at = crate::projection::model_request_started_at(detail);
        detail.messages.push(message);
    }
    detail.messages.last_mut().expect("stream message inserted")
}

pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}
