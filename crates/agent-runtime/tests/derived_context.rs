use std::{convert::Infallible, sync::Mutex};

use agent_core::agent::{AgentRun, RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextError, CompactingContextManager,
    ContextCheckpointStore, ContextCompactor, ContextInput, ContextManager, ContextPolicy,
    ContextSummary, InMemoryContextCheckpointStore, MessageContext, ModelMessage, ModelRole,
    TokenCounter,
};

#[derive(Default)]
struct Compactor {
    calls: Mutex<Vec<Vec<RunEventEnvelope>>>,
}

impl ContextCompactor for Compactor {
    type Error = Infallible;

    fn estimate_input_tokens(
        &self,
        _: Option<&ContextSummary>,
        _: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(0)
    }

    async fn compact(
        &self,
        _: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.calls.lock().unwrap().push(events);
        Ok(ContextSummary {
            objective: "Continue the task".into(),
            ..ContextSummary::default()
        })
    }
}

fn manager(
    limit: usize,
    store: InMemoryContextCheckpointStore,
) -> CompactingContextManager<Compactor, InMemoryContextCheckpointStore, ApproximateTokenCounter> {
    CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(limit)
            .compaction_threshold(limit - 1)
            .recent_tail_tokens(1)
            .estimation_margin_percent(0)
            .build()
            .unwrap(),
        Compactor::default(),
        store,
        ApproximateTokenCounter::new(1).unwrap(),
    )
}

fn memory(content: &str) -> ModelMessage {
    ModelMessage::summary("wiki:revision".into(), content.into())
}

fn input(run: &AgentRun) -> ContextInput {
    ContextInput::new(run.id(), "instructions", run.events().to_vec(), vec![])
}

fn history() -> AgentRun {
    let mut run = AgentRun::new("wiki-run", "agent", "Use the corrected specification");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Earlier work. ".repeat(400),
    })
    .unwrap();
    run
}

#[tokio::test]
async fn derived_context_precedes_user_input_and_is_counted_without_changing_history() {
    let run = AgentRun::new("wiki-run", "agent", "Use the corrected specification");
    let manager = manager(10_000, InMemoryContextCheckpointStore::new());
    let plain = input(&run);
    let baseline = manager.prepare(plain.as_ref()).await.unwrap();
    let messages = vec![memory("Historical specification")];
    let with_memory = plain.as_ref().with_derived_messages(&messages).into_owned();
    assert_eq!(with_memory.derived_messages, messages);
    assert_eq!(with_memory.as_ref().derived_messages, messages);

    let prepared = manager.prepare_owned(with_memory.clone()).await.unwrap();

    assert_eq!(prepared.request.messages[0], messages[0]);
    assert_eq!(prepared.request.messages[0].role, ModelRole::User);
    assert!(matches!(
        prepared.request.messages[0].context,
        Some(MessageContext::Summary { .. })
    ));
    assert_eq!(
        prepared.request.messages[1],
        ModelMessage::user("Use the corrected specification".into())
    );
    assert!(
        manager.token_counter().count_tokens(&prepared.request)
            > manager.token_counter().count_tokens(&baseline.request)
    );
    assert_eq!(with_memory.events, run.events());
}

#[tokio::test]
async fn derived_context_survives_compaction_and_can_change_without_invalidating_checkpoints() {
    let run = history();
    let store = InMemoryContextCheckpointStore::new();
    let manager = manager(1_000, store.clone());
    let source = run.events().to_vec();
    let prepared = manager
        .prepare_owned(input(&run).with_derived_messages(vec![memory("Historical notes")]))
        .await
        .unwrap();

    assert!(prepared.omitted_history.is_some());
    assert_eq!(prepared.request.messages[0], memory("Historical notes"));
    assert_eq!(
        prepared.request.messages[1],
        ModelMessage::user("Use the corrected specification".into())
    );
    assert_eq!(manager.compactor().calls.lock().unwrap()[0], source);
    let checkpoint = store.latest(run.id().into()).await.unwrap().unwrap();

    // A new manager simulates restart: the journal checksum does not depend on
    // a mutable memory projection, and its refreshed content is injected once.
    let restored = self::manager(1_000, store.clone());
    let updated = restored
        .prepare_owned(input(&run).with_derived_messages(vec![memory("Revised historical notes")]))
        .await
        .unwrap();
    assert_eq!(
        updated.request.messages[0],
        memory("Revised historical notes")
    );
    assert_eq!(
        updated.request.messages.len(),
        prepared.request.messages.len()
    );
    assert!(restored.compactor().calls.lock().unwrap().is_empty());
    assert_eq!(
        store.latest(run.id().into()).await.unwrap(),
        Some(checkpoint)
    );
    assert_eq!(run.events(), source);
}

#[tokio::test]
async fn derived_context_cannot_bypass_protected_input_limits() {
    let run = history();
    let store = InMemoryContextCheckpointStore::new();
    let manager = manager(100, store.clone());
    let input = input(&run).with_derived_messages(vec![memory(&"x".repeat(200))]);

    assert!(matches!(
        manager.prepare(input.as_ref()).await,
        Err(CompactingContextError::ProtectedContextTokenLimitExceeded { .. })
    ));
    assert!(manager.compactor().calls.lock().unwrap().is_empty());
    assert!(store.latest(run.id().into()).await.unwrap().is_none());
}

#[tokio::test]
async fn manual_compaction_and_overflow_recovery_preserve_derived_context() {
    for manual in [true, false] {
        let run = history();
        let manager = manager(10_000, InMemoryContextCheckpointStore::new());
        let input = input(&run).with_derived_messages(vec![memory("Historical notes")]);
        let initial = manager.prepare(input.as_ref()).await.unwrap();
        assert!(initial.omitted_history.is_none());
        let prepared = if manual {
            manager.compact_now(input.as_ref()).await.unwrap().unwrap()
        } else {
            manager
                .recover_overflow(input.as_ref(), &initial.request)
                .await
                .unwrap()
                .unwrap()
        };
        assert_eq!(prepared.request.messages[0], memory("Historical notes"));
        assert!(prepared.omitted_history.is_some());
        assert!(
            manager.token_counter().count_tokens(&prepared.request)
                < manager.token_counter().count_tokens(&initial.request)
        );
    }
}
