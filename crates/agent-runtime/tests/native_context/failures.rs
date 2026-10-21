use super::*;
use agent_runtime::ModelRole;
use agent_runtime::{ModelContextMetadata, ModelUsage, NativeContextCheckpoint, ToolCall};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
struct FailingStore {
    inner: InMemoryNativeContextCheckpointStore,
    fail_writes: AtomicBool,
}

impl NativeContextCheckpointStore for FailingStore {
    type Error = io::Error;

    async fn latest(&self, run_id: String) -> Result<Option<NativeContextCheckpoint>, Self::Error> {
        self.inner.latest(run_id).await.map_err(io::Error::other)
    }

    async fn save(&self, checkpoint: NativeContextCheckpoint) -> Result<(), Self::Error> {
        if self.fail_writes.load(Ordering::Relaxed) {
            return Err(io::Error::other("checkpoint writes unavailable"));
        }
        self.inner.save(checkpoint).await.map_err(io::Error::other)
    }

    async fn clear(&self, run_id: String) -> Result<(), Self::Error> {
        self.inner.clear(run_id).await.map_err(io::Error::other)
    }
}

fn failure_manager(
    input_limit: usize,
) -> NativeContextManager<Compactor, FailingStore, ApproximateTokenCounter> {
    NativeContextManager::new(
        manager(input_limit, 3_000, window(20)).policy(),
        Compactor {
            identity: identity(),
            window: window(20),
            input_limit: 3_000,
            requests: Mutex::new(Vec::new()),
        },
        FailingStore::default(),
        ApproximateTokenCounter::new(1).unwrap(),
    )
}

fn output(tokens: usize) -> ModelOutput {
    ModelOutput {
        provider_state: None,
        response: ModelResponse::final_answer("Done".to_owned()),
        usage: None,
        provider_context: Some(window(tokens)),
        context_compacted: false,
    }
}

#[tokio::test]
async fn failed_response_cache_write_preserves_a_successful_answer_when_events_fit() {
    let manager = failure_manager(3_000);
    manager
        .checkpoint_store()
        .fail_writes
        .store(true, Ordering::Relaxed);
    let mut run = AgentRun::new("answer", "agent", "Read");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        provider_state: None,
        content: "Done".to_owned(),
    })
    .unwrap();

    manager
        .observe_response(input(&run).as_ref(), &output(40))
        .await
        .unwrap();
    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert!(prepared.request.provider_context.is_none());
    assert!(prepared.request.messages.iter().any(|message| {
        let candidate = message;
        candidate.role == ModelRole::Assistant && {
            let text = candidate.text();
            let content = text.as_str();
            content == "Done"
        }
    }));
}

#[tokio::test]
async fn failed_cache_write_keeps_a_prior_usable_provider_window() {
    let manager = failure_manager(3_000);
    let run = pending_tool();
    manager
        .observe_response(input(&run).as_ref(), &output(40))
        .await
        .unwrap();
    manager
        .checkpoint_store()
        .fail_writes
        .store(true, Ordering::Relaxed);

    manager
        .observe_response(input(&run).as_ref(), &output(60))
        .await
        .unwrap();
    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert_eq!(prepared.request.provider_context.unwrap().input_tokens, 40);
}

#[tokio::test]
async fn failed_cache_write_can_fall_back_to_events_even_if_an_older_window_is_too_large() {
    let manager = failure_manager(3_000);
    let run = history();
    manager
        .observe_response(input(&run).as_ref(), &output(3_100))
        .await
        .unwrap();
    manager
        .checkpoint_store()
        .fail_writes
        .store(true, Ordering::Relaxed);

    manager
        .observe_response(input(&run).as_ref(), &output(40))
        .await
        .unwrap();
    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert!(prepared.request.provider_context.is_none());
    assert!(manager.token_counter().count_tokens(&prepared.request) <= 3_000);
    assert!(manager.compactor().requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_cache_write_remains_an_error_when_durable_events_cannot_fit() {
    let manager = failure_manager(1_000);
    manager
        .checkpoint_store()
        .fail_writes
        .store(true, Ordering::Relaxed);

    let error = manager
        .observe_response(input(&history()).as_ref(), &output(40))
        .await
        .unwrap_err();

    assert!(matches!(error, NativeContextError::CheckpointStore(_)));
}

#[tokio::test]
async fn invalid_provider_metadata_remains_fatal_even_when_the_cache_is_unavailable() {
    let manager = failure_manager(3_000);
    manager
        .checkpoint_store()
        .fail_writes
        .store(true, Ordering::Relaxed);

    let error = manager
        .observe_response(input(&pending_tool()).as_ref(), &output(0))
        .await
        .unwrap_err();

    assert!(matches!(error, NativeContextError::InvalidWindow));
}

#[tokio::test]
async fn failed_candidate_keeps_actual_usage_when_the_raw_fallback_underestimates_context() {
    let manager = manager(1_000, 3_000, window(1_100));
    let mut run = AgentRun::new("usage-hard-fallback", "agent", "Keep API stable.");
    let mut metadata = ModelContextMetadata::new("test-provider", "test-model", 1_000).unwrap();
    metadata.effective_context_window_percent = 100;
    let context = |run: &AgentRun| input(run).with_model_metadata(metadata.clone());
    manager.prepare_owned(context(&run)).await.unwrap();

    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        provider_state: None,
        content: "Inspecting".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: "read-call".to_owned(),
        tool_name: "read".to_owned(),
        arguments: "{}".to_owned(),
    })
    .unwrap();
    manager
        .observe_response(
            context(&run).as_ref(),
            &ModelOutput {
                provider_state: None,
                response: ModelResponse::tool_calls(
                    "Inspecting".to_owned(),
                    vec![ToolCall {
                        id: "read-call".to_owned(),
                        name: "read".to_owned(),
                        arguments: "{}".to_owned(),
                    }],
                ),
                usage: Some(ModelUsage {
                    input_tokens: 900,
                    output_tokens: 50,
                    cached_input_tokens: Some(800),
                }),
                provider_context: None,
                context_compacted: false,
            },
        )
        .await
        .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "read-call".to_owned(),
        output: "r".repeat(100),
    })
    .unwrap();

    // The provider's last response plus the new tool result exceed the hard
    // limit. Trying an oversized native candidate must not discard that known
    // usage and then accept the same conversation using its much lower estimate.
    let error = manager.prepare_owned(context(&run)).await.unwrap_err();
    assert!(matches!(
        error,
        NativeContextError::InputTokenLimitExceeded { limit: 1_000, .. }
    ));
    {
        let requests = manager.compactor().requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(manager.token_counter().count_tokens(&requests[0]) < 1_000);
    }
    assert!(
        manager
            .checkpoint_store()
            .latest(run.id().to_owned())
            .await
            .unwrap()
            .is_none()
    );
}
