use std::future::Future;

use super::ContextInputRef;
use crate::ModelMessage;

/// Host-owned preparation for an actual summary boundary. The runtime does not
/// know which optional memory implementation, if any, supplies this context.
#[derive(Debug, Clone)]
pub struct CompactionPreparation {
    /// Complete replacement for the input's derived messages, not an append.
    pub derived_messages: Vec<ModelMessage>,
    /// False defers optional automatic compaction while the original request
    /// still fits. Manual compaction and hard overflow recovery may continue.
    pub ready: bool,
}

/// Runs after the runtime selects a summary boundary, before summarization.
///
/// `input.events` is the complete canonical snapshot, including unclipped tool
/// output. Only events through `through_sequence` need coverage. The hook may
/// run again if compaction expands that boundary; hosts should make it
/// idempotent and bound total work across those calls. Each call receives the
/// original snapshot, including its original derived messages. Dropping the returned
/// future must stop any blocking preparation. Pure tool-output pruning does
/// not invoke this hook.
pub trait CompactionPreparationHook: Send + Sync {
    fn before_compaction(
        &self,
        input: ContextInputRef<'_>,
        through_sequence: u64,
    ) -> impl Future<Output = CompactionPreparation> + Send;
}

pub(super) struct NoCompactionPreparation;

impl CompactionPreparationHook for NoCompactionPreparation {
    async fn before_compaction(&self, input: ContextInputRef<'_>, _: u64) -> CompactionPreparation {
        CompactionPreparation {
            derived_messages: input.derived_messages.to_vec(),
            ready: true,
        }
    }
}
