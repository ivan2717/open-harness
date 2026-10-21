use std::convert::Infallible;

use agent_core::agent::RunEventEnvelope;
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContextCompactor, ContextPolicy,
    ContextSummary, InMemoryContextCheckpointStore, TokenCounter,
};

#[derive(Debug, Clone, Copy)]
pub struct UnexpectedCompactor;

impl ContextCompactor for UnexpectedCompactor {
    type Error = Infallible;

    fn estimate_input_tokens(
        &self,
        _previous_summary: Option<&ContextSummary>,
        _events: &[RunEventEnvelope],
        _token_counter: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(0)
    }

    async fn compact(
        &self,
        _previous_summary: Option<ContextSummary>,
        _events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        panic!("short runtime fixture unexpectedly crossed the compaction threshold")
    }
}

pub fn test_context_manager() -> CompactingContextManager<
    UnexpectedCompactor,
    InMemoryContextCheckpointStore,
    ApproximateTokenCounter,
> {
    CompactingContextManager::new(
        ContextPolicy::default(),
        UnexpectedCompactor,
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    )
}
