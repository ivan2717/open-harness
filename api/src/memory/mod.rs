//! Optional, derived session memory. Canonical events remain authoritative.
mod compiler;
mod recall;
mod scheduler;
pub use scheduler::CatchUpBudget;
mod sources;
mod store;
pub(crate) use store::MemoryStore;
#[cfg(test)]
mod tests;

use crate::{extensions::ExtensionHost, model::ServiceModel};
use agent_core::agent::RunEventEnvelope;
use agent_memory::{WikiPage, WikiSourceCursor, WikiTaskItem};
use agent_runtime::{ContextInputRef, Model, ModelMessage, ProviderReplayScope};
use serde::Serialize;
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub const PLUGIN_ID: &str = "builtin.llm-wiki";

#[derive(Serialize)]
pub struct RunMemoryView {
    enabled: bool,
    status: &'static str,
    revision: u64,
    through_sequence: Option<u64>,
    source_cursor: Option<WikiSourceCursor>,
    pending: bool,
    overview: String,
    pages: Vec<WikiPage>,
    tasks: Vec<WikiTaskItem>,
    error: Option<String>,
}

impl RunMemoryView {
    pub fn load(host: &ExtensionHost, run: &str, events: &[RunEventEnvelope]) -> Self {
        let enabled = host.memory_enabled();
        let loaded = host.memory.validate_source(run, events);
        let error = if loaded.is_err() {
            Some("记忆读取或来源校验失败，未向模型提供这些记录。".into())
        } else {
            host.memory_errors.read().unwrap().get(run).cloned()
        };
        let state = loaded.ok().flatten();
        Self {
            enabled,
            status: if !enabled {
                "disabled"
            } else if error.is_some() {
                "error"
            } else if state.is_some() {
                "ready"
            } else {
                "empty"
            },
            revision: state.as_ref().map_or(0, |state| state.revision),
            through_sequence: state.as_ref().map(|state| state.through_sequence),
            source_cursor: state.as_ref().and_then(|state| state.source_cursor),
            pending: sources::batch(
                events,
                state.as_ref().and_then(|state| state.source_cursor),
                UpdateReason::Compaction,
            )
            .is_some(),
            overview: state
                .as_ref()
                .map_or_else(String::new, |state| state.overview.clone()),
            pages: state
                .as_ref()
                .map_or_else(Vec::new, |state| state.pages.clone()),
            tasks: state.map_or_else(Vec::new, |state| state.tasks),
            error,
        }
    }
}

#[derive(Clone, Copy)]
pub enum UpdateReason {
    Progress,
    Response,
    Compaction,
}

pub struct WikiMemory<M = ServiceModel> {
    core: Arc<MemoryEngine<M>>,
    background: Arc<scheduler::Background>,
}

struct MemoryEngine<M> {
    host: Arc<ExtensionHost>,
    model: M,
    // One lock serializes foreground catch-up and background compilation.
    // Only actual failures retain a cooldown; dropping an in-flight future
    // leaves the durable cursor unchanged and does not prevent takeover.
    attempted: Mutex<Option<Attempt>>,
}

struct Attempt {
    through: u64,
    generation: u64,
    user_sequence: Option<u64>,
    started: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateOutcome {
    Idle,
    Committed,
    CoolingDown,
    Failed,
    Disabled,
}

impl<M: Model> WikiMemory<M> {
    pub fn new(host: Arc<ExtensionHost>, model: M) -> Self {
        Self {
            core: Arc::new(MemoryEngine {
                host,
                model,
                attempted: Mutex::new(None),
            }),
            background: Arc::default(),
        }
    }

    #[cfg(test)]
    pub async fn update(&self, input: ContextInputRef<'_>, reason: UpdateReason) {
        self.core.update(input, reason, None).await;
    }

    pub fn messages(&self, input: ContextInputRef<'_>) -> Vec<ModelMessage> {
        if !self.core.host.memory_enabled() || input.events.iter().any(|event| matches!(
            &event.event, agent_core::agent::RunEvent::ModelResponseReceived { provider_state: Some(state), .. }
                if state.scope == ProviderReplayScope::Prefix
        )) { return Vec::new() }
        // Small model windows keep the complete budget for the active task;
        // memory is still available through the read-only tool.
        if input
            .model_metadata
            .is_some_and(|metadata| metadata.context_window < 16_384)
        {
            return Vec::new();
        }
        let Ok(Some(state)) = self
            .core
            .host
            .memory
            .validate_source(input.run_id, input.events)
        else {
            return Vec::new();
        };
        let text = recall::render(&state, input.events, recall::MAX_RECALL_CHARS);
        vec![ModelMessage::summary(
            format!("wiki-{}", state.revision),
            text,
        )]
    }
}

impl<M> Drop for WikiMemory<M> {
    fn drop(&mut self) {
        self.background.abort();
    }
}

impl<M: Model> MemoryEngine<M> {
    fn pending(&self, input: ContextInputRef<'_>, through: Option<u64>) -> io::Result<bool> {
        let state = self
            .host
            .memory
            .validate_source(input.run_id, input.events)?;
        let events = source_prefix(input.events, through);
        Ok(sources::batch(
            events,
            state.and_then(|state| state.source_cursor),
            UpdateReason::Compaction,
        )
        .is_some())
    }

    async fn update(
        &self,
        input: ContextInputRef<'_>,
        reason: UpdateReason,
        through: Option<u64>,
    ) -> UpdateOutcome {
        let Some(generation) = self.host.memory_generation() else {
            return UpdateOutcome::Disabled;
        };
        let Some(last) = input.events.last() else {
            return UpdateOutcome::Idle;
        };
        let mut attempted = self.attempted.lock().await;
        let user_sequence = input
            .events
            .iter()
            .rev()
            .find(|event| event.event.user_input().is_some())
            .map(|event| event.sequence);
        if attempted.as_ref().is_some_and(|attempt| {
            attempt.generation == generation
                && attempt.started.elapsed() < Duration::from_secs(30)
                && (attempt.through == last.sequence || attempt.user_sequence == user_sequence)
        }) {
            return UpdateOutcome::CoolingDown;
        }
        let result = self.update_inner(input, reason, through, generation).await;
        match result {
            Ok(committed) => {
                *attempted = None;
                self.host
                    .memory_errors
                    .write()
                    .unwrap()
                    .remove(input.run_id);
                if self.host.memory_generation() != Some(generation) {
                    UpdateOutcome::Disabled
                } else if committed {
                    UpdateOutcome::Committed
                } else {
                    UpdateOutcome::Idle
                }
            }
            Err(_) => {
                *attempted = Some(Attempt {
                    through: last.sequence,
                    generation,
                    user_sequence,
                    started: Instant::now(),
                });
                self.host.memory_errors.write().unwrap().insert(
                    input.run_id.to_owned(),
                    "本次记忆整理未完成，继续使用已有记录；会话仍可正常运行。".into(),
                );
                UpdateOutcome::Failed
            }
        }
    }

    async fn update_inner(
        &self,
        input: ContextInputRef<'_>,
        reason: UpdateReason,
        through: Option<u64>,
        generation: u64,
    ) -> io::Result<bool> {
        // Validate against the complete canonical journal, including already
        // verified events beyond a foreground compaction target.
        let previous = self
            .host
            .memory
            .validate_source(input.run_id, input.events)?;
        let cursor = previous.as_ref().and_then(|state| state.source_cursor);
        let Some(mut batch) = sources::batch(source_prefix(input.events, through), cursor, reason)
        else {
            return Ok(false);
        };
        let update = compiler::compile(&self.model, previous.as_ref(), &mut batch).await?;
        self.host.commit_memory(
            generation,
            input.run_id,
            input.events,
            previous.as_ref().map_or(batch.through, |state| {
                state.through_sequence.max(batch.through)
            }),
            batch.cursor,
            update,
        )?;
        Ok(true)
    }
}

fn source_prefix(events: &[RunEventEnvelope], through: Option<u64>) -> &[RunEventEnvelope] {
    match through {
        Some(through) => &events[..events.partition_point(|event| event.sequence <= through)],
        None => events,
    }
}
