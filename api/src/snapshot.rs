use crate::{
    contracts::{ChatMessage, RunDetail},
    projection, time,
};
use agent_core::agent::RunEventEnvelope;
use agent_runtime::{ModelStreamEvent, ModelStreamSink};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    inner: Arc<Mutex<RunDetail>>,
    sender: watch::Sender<RunDetail>,
    path: PathBuf,
    steering: watch::Sender<bool>,
}

impl Snapshot {
    pub fn new(detail: RunDetail, path: PathBuf) -> io::Result<Self> {
        write_json(&path, &detail)?;
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
        let (sender, _) = watch::channel(detail.clone());
        let (steering, _) = watch::channel(has_steering(&detail));
        Ok(Self {
            inner: Arc::new(Mutex::new(detail)),
            sender,
            path,
            steering,
        })
    }
    pub fn get(&self) -> RunDetail {
        self.inner.lock().expect("snapshot lock poisoned").clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<RunDetail> {
        self.sender.subscribe()
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
        let mut detail = current.clone();
        let result = update(&mut detail)?;
        detail.summary.revision = detail.summary.revision.saturating_add(1);
        detail.summary.updated_at = time::now();
        // The revision survives restart, but provisional stream output is never restored.
        let mut durable = detail.clone();
        durable
            .messages
            .retain(|message| message.status != "streaming");
        write_json(&self.path, &durable)?;
        *current = detail.clone();
        self.steering.send_replace(has_steering(&detail));
        self.sender.send_replace(detail);
        Ok(result)
    }
    pub fn event(&self, event: &RunEventEnvelope) -> io::Result<()> {
        if self
            .inner
            .lock()
            .map_err(|_| io::Error::other("snapshot lock poisoned"))?
            .events
            .last()
            .is_some_and(|last| last.sequence >= event.sequence)
        {
            return Ok(());
        }
        self.update(|detail| projection::apply(detail, event))
    }
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
        if self.get().pending_inputs_paused
            || !self
                .get()
                .pending_inputs
                .iter()
                .any(|input| !input.delivering && input.steer)
        {
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
        if self.get().pending_inputs.iter().any(|input| input.id == id) {
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
            ModelStreamEvent::TextDelta { text, .. } => {
                self.update(|detail| streaming_message(detail).text.push_str(&text))
            }
            ModelStreamEvent::ReasoningDelta { text, .. } => self.update(|detail| {
                streaming_message(detail)
                    .reasoning
                    .get_or_insert_default()
                    .push_str(&text)
            }),
            _ => return,
        };
        if let Err(error) = result {
            tracing::error!(%error, "could not persist UI snapshot revision");
        }
    }
}

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
