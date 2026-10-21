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

#[derive(Clone)]
pub(crate) struct Snapshot {
    inner: Arc<Mutex<RunDetail>>,
    sender: watch::Sender<RunDetail>,
    path: PathBuf,
}

impl Snapshot {
    pub fn new(detail: RunDetail, path: PathBuf) -> io::Result<Self> {
        write_json(&path, &detail)?;
        let (sender, _) = watch::channel(detail.clone());
        Ok(Self {
            inner: Arc::new(Mutex::new(detail)),
            sender,
            path,
        })
    }
    pub fn get(&self) -> RunDetail {
        self.inner.lock().expect("snapshot lock poisoned").clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<RunDetail> {
        self.sender.subscribe()
    }
    pub fn update(&self, update: impl FnOnce(&mut RunDetail)) -> io::Result<()> {
        let mut detail = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("snapshot lock poisoned"))?;
        update(&mut detail);
        detail.summary.revision = detail.summary.revision.saturating_add(1);
        detail.summary.updated_at = time::now();
        // The revision survives restart, but provisional stream output is never restored.
        let mut durable = detail.clone();
        durable
            .messages
            .retain(|message| message.status != "streaming");
        write_json(&self.path, &durable)?;
        self.sender.send_replace(detail.clone());
        Ok(())
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
