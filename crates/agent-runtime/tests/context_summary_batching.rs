use std::{convert::Infallible, sync::Mutex};

use agent_core::agent::{Agent, RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContextCheckpointStore, ContextCompactor,
    ContextInput, ContextManager, ContextPolicy, ContextSummary, InMemoryContextCheckpointStore,
    ModelResponse, TokenCounter, ToolCall,
};

struct CountingCompactor {
    limit: usize,
    non_monotonic: bool,
    counts: Mutex<Vec<(Vec<u64>, usize)>>,
    batches: Mutex<Vec<Vec<u64>>>,
}

impl CountingCompactor {
    fn tokens(&self, previous: Option<&ContextSummary>, events: &[RunEventEnvelope]) -> usize {
        let base = events.len() * 10 + usize::from(previous.is_some()) * 20;
        // A smaller prefix can cost more than its extension. The search must
        // retain an individually checked prefix within the budget.
        if self.non_monotonic && events.len().is_multiple_of(3) {
            base + self.limit
        } else {
            base
        }
    }
}

impl ContextCompactor for CountingCompactor {
    type Error = Infallible;
    fn input_token_limit(&self) -> Option<usize> {
        Some(self.limit)
    }
    fn estimate_input_tokens(
        &self,
        previous: Option<&ContextSummary>,
        events: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        let tokens = self.tokens(previous, events);
        self.counts
            .lock()
            .unwrap()
            .push((events.iter().map(|event| event.sequence).collect(), tokens));
        Ok(tokens)
    }
    async fn compact(
        &self,
        previous: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        let tokens = self.tokens(previous.as_ref(), &events);
        let sequences = events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>();
        assert!(
            tokens <= self.limit,
            "an oversized batch reached the compactor"
        );
        assert!(
            self.counts
                .lock()
                .unwrap()
                .contains(&(sequences.clone(), tokens)),
            "the final batch must have its own successful budget check"
        );
        self.batches.lock().unwrap().push(sequences);
        Ok(ContextSummary {
            objective: "Continue.".into(),
            ..Default::default()
        })
    }
}

fn input() -> ContextInput {
    let agent = Agent::new("agent", "Agent", "", "Read records.");
    let mut run = agent.start_run("summary-batching", "Read records.");
    for index in 0..25 {
        let call_id = index.to_string();
        run.begin_step().unwrap();
        let response = ModelResponse::tool_calls(
            String::new(),
            vec![ToolCall {
                id: call_id.clone(),
                name: "read".into(),
                arguments: "{}".into(),
            }],
        );
        run.record(RunEvent::ModelResponseReceived {
            content: response.text(),
            response: Some(response),
            token_usage: None,
            provider_state: None,
            provider_continuation: None,
        })
        .unwrap();
        run.record(RunEvent::ToolCallRequested {
            call_id: call_id.clone(),
            tool_name: "read".into(),
            arguments: "{}".into(),
        })
        .unwrap();
        run.record(RunEvent::ToolCallSucceeded {
            call_id,
            output: "x".repeat(100),
        })
        .unwrap();
    }
    ContextInput::new(run.id(), "Read records.", run.events().to_vec(), Vec::new())
}

async fn check_batches(limit: usize, non_monotonic: bool) -> (usize, usize) {
    let policy = ContextPolicy::builder()
        .input_token_limit(10_000)
        .compaction_threshold(500)
        .compaction_target(400)
        .recent_tail_tokens(50)
        .build()
        .unwrap();
    let manager = CompactingContextManager::new(
        policy,
        CountingCompactor {
            limit,
            non_monotonic,
            counts: Mutex::default(),
            batches: Mutex::default(),
        },
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    );
    let input = input();
    manager.prepare(input.as_ref()).await.unwrap();
    let checkpoint = manager
        .checkpoint_store()
        .latest(input.run_id.clone())
        .await
        .unwrap()
        .unwrap();
    let expected = input
        .events
        .iter()
        .filter(|event| event.sequence <= checkpoint.through_sequence)
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    let batches = manager.compactor().batches.lock().unwrap();
    assert_eq!(
        batches.concat(),
        expected,
        "each source event must appear exactly once and in order"
    );
    (
        manager.compactor().counts.lock().unwrap().len(),
        batches.len(),
    )
}

#[tokio::test]
async fn a_complete_fitting_summary_batch_requires_one_count() {
    assert_eq!(check_batches(10_000, false).await, (1, 1));
}

#[tokio::test]
async fn large_summary_batches_use_logarithmic_probes_and_preserve_every_event() {
    let (counts, batches) = check_batches(360, false).await;
    assert!(batches > 1);
    assert!(
        counts <= 22,
        "batch selection should avoid re-encoding every successive prefix: {counts}"
    );
}

#[tokio::test]
async fn non_monotonic_counts_still_emit_only_individually_validated_batches() {
    let (_, batches) = check_batches(360, true).await;
    assert!(batches > 1);
}
