use agent_runtime::{ContentBlock, MessageContext, ModelRole};
use std::{convert::Infallible, sync::Mutex};

use agent_core::agent::{AgentRun, RunEvent};
use agent_runtime::{
    ApproximateTokenCounter, ContextInput, ContextManager, ContextPolicy,
    InMemoryNativeContextCheckpointStore, Model, ModelMessage, ModelOutput, ModelRequest,
    ModelResponse, NativeContextCheckpointStore, NativeContextCompactor, NativeContextError,
    NativeContextManager, ProviderContextIdentity, ProviderContextWindow, TokenCounter,
};
use serde_json::json;

#[path = "native_context/failures.rs"]
mod failures;

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "test-provider".to_owned(),
        model: "test-model".to_owned(),
        format: "responses-v1".to_owned(),
    }
}

fn window(tokens: usize) -> ProviderContextWindow {
    ProviderContextWindow {
        prefix_fingerprint: None,
        identity: identity(),
        items: vec![
            json!({"type": "message", "role": "user", "content": "Keep API stable."}),
            json!({"type": "compaction", "encrypted_content": "opaque".repeat(10_000)}),
            json!({"type": "message", "role": "assistant", "content": "I inspected the files."}),
        ],
        input_tokens: tokens,
    }
}

struct Compactor {
    identity: ProviderContextIdentity,
    window: ProviderContextWindow,
    input_limit: usize,
    requests: Mutex<Vec<ModelRequest>>,
}

impl NativeContextCompactor for Compactor {
    type Error = Infallible;

    fn identity(&self) -> &ProviderContextIdentity {
        &self.identity
    }

    fn input_token_limit(&self) -> usize {
        self.input_limit
    }

    async fn compact(
        &self,
        request: ModelRequest,
        _target_input_tokens: usize,
    ) -> Result<ProviderContextWindow, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(self.window.clone())
    }
}

type Manager =
    NativeContextManager<Compactor, InMemoryNativeContextCheckpointStore, ApproximateTokenCounter>;

fn manager(
    input_limit: usize,
    compact_input_limit: usize,
    window: ProviderContextWindow,
) -> Manager {
    NativeContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(input_limit)
            .compaction_threshold(800)
            .compaction_target(400)
            .recent_tail_tokens(100)
            .max_tool_output_chars(4_000)
            .build()
            .unwrap(),
        Compactor {
            identity: identity(),
            window,
            input_limit: compact_input_limit,
            requests: Mutex::new(Vec::new()),
        },
        InMemoryNativeContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    )
}

fn input(run: &AgentRun) -> ContextInput {
    ContextInput::new(run.id(), "", run.events().to_vec(), Vec::new())
}

fn history() -> AgentRun {
    let mut run = pending_tool();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "read-call".to_owned(),
        output: "file contents ".repeat(160),
    })
    .unwrap();
    run
}

fn pending_tool() -> AgentRun {
    let mut run = AgentRun::new("native-run", "agent", "Keep API stable.");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
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
    run
}

#[tokio::test]
async fn derived_context_is_rejected_before_native_window_mutation() {
    let manager = manager(3_000, 3_000, window(40));
    let run = history();
    let input = input(&run).with_derived_messages(vec![ModelMessage::summary(
        "wiki:revision".into(),
        "Historical notes".into(),
    )]);
    let rejected = ModelRequest {
        instructions: String::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        max_output_tokens: None,
        provider_context: Some(window(40)),
    };
    assert!(matches!(
        manager.prepare(input.as_ref()).await,
        Err(NativeContextError::DerivedContextUnsupported)
    ));
    assert!(matches!(
        manager.recover_overflow(input.as_ref(), &rejected).await,
        Err(NativeContextError::DerivedContextUnsupported)
    ));
    assert!(matches!(
        manager.compact_now(input.as_ref()).await,
        Err(NativeContextError::DerivedContextUnsupported)
    ));
    let output = ModelOutput {
        provider_context: Some(window(40)),
        ..ModelResponse::final_answer("Done".into()).into()
    };
    assert!(matches!(
        manager.observe_response(input.as_ref(), &output).await,
        Err(NativeContextError::DerivedContextUnsupported)
    ));
    assert!(manager.compactor().requests.lock().unwrap().is_empty());
    assert!(
        manager
            .checkpoint_store()
            .latest(run.id().into())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn native_compaction_preserves_the_entire_ordered_window_and_protects_user_inputs() {
    let native = window(40);
    let manager = manager(3_000, 3_000, native.clone());
    let run = history();
    let source = run.events().to_vec();

    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert_eq!(prepared.request.provider_context.as_ref(), Some(&native));
    assert_eq!(prepared.request.messages.len(), 1);
    assert_eq!(
        prepared.request.messages[0].context,
        Some(MessageContext::Protected)
    );
    assert!(
        prepared.request.messages[0]
            .text()
            .contains("Keep API stable.")
    );
    assert_eq!(
        prepared.omitted_history.unwrap().through_sequence,
        source.last().unwrap().sequence
    );
    assert_eq!(run.events(), source);
    // Ciphertext length is unrelated to the token cost of the native state.
    assert!(manager.token_counter().count_tokens(&prepared.request) < 400);
    assert_eq!(manager.compactor().requests.lock().unwrap().len(), 1);

    let again = manager.prepare_owned(input(&run)).await.unwrap();
    assert_eq!(again.request, prepared.request);
    assert_eq!(manager.compactor().requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn response_window_covers_tool_declarations_but_appends_tool_results_exactly_once() {
    let manager = manager(3_000, 3_000, window(20));
    let mut run = pending_tool();
    let native = window(50);
    let output = ModelOutput {
        provider_state: None,
        response: ModelResponse::tool_calls("Inspecting".to_owned(), Vec::new()),
        usage: None,
        provider_context: Some(native.clone()),
        context_compacted: false,
    };
    manager
        .observe_response(input(&run).as_ref(), &output)
        .await
        .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "read-call".to_owned(),
        output: "contents".to_owned(),
    })
    .unwrap();

    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert_eq!(prepared.request.provider_context, Some(native));
    assert_eq!(
        prepared.request.messages,
        vec![ModelMessage::tool_result(
            "read-call".into(),
            "contents".into(),
            false
        )]
    );
    assert!(manager.compactor().requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn incompatible_provider_model_or_format_checkpoints_rebuild_from_durable_events() {
    for field in ["provider", "model", "format"] {
        let manager = manager(3_000, 3_000, window(20));
        let run = history();
        manager.prepare_owned(input(&run)).await.unwrap();
        let mut checkpoint = manager
            .checkpoint_store()
            .latest(run.id().to_owned())
            .await
            .unwrap()
            .unwrap();
        match field {
            "provider" => checkpoint.window.identity.provider = "different".to_owned(),
            "model" => checkpoint.window.identity.model = "different".to_owned(),
            _ => checkpoint.window.identity.format = "different".to_owned(),
        }
        manager.checkpoint_store().save(checkpoint).await.unwrap();

        let prepared = manager.prepare_owned(input(&run)).await.unwrap();

        assert_eq!(
            prepared.request.provider_context.unwrap().identity,
            identity()
        );
        let requests = manager.compactor().requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].provider_context.is_none());
        assert!(requests[1].messages.iter().any(|message| {
            let candidate = message;
            candidate.role == ModelRole::User && candidate.context.is_none()
        }));
    }
}

#[tokio::test]
async fn changed_event_prefix_invalidates_native_state_even_when_sequences_match() {
    let manager = manager(3_000, 3_000, window(20));
    let run = history();
    manager.prepare_owned(input(&run)).await.unwrap();
    let mut changed = input(&run);
    changed.events[0].event = RunEvent::RunStarted {
        files: Vec::new(),
        instruction: "New constraints".to_owned(),
    };

    let prepared = manager.prepare_owned(changed).await.unwrap();

    assert_eq!(manager.compactor().requests.lock().unwrap().len(), 2);
    assert!({
        let candidate = &prepared.request.messages[0];
        matches!(&candidate.context, Some(MessageContext::Protected)) && {
            let text = candidate.text();
            let content = text.as_str();
            content.contains("New constraints")
        }
    });
}

#[tokio::test]
async fn native_compaction_checks_its_own_input_limit_before_calling_the_provider() {
    let manager = manager(1_000, 900, window(20));

    let error = manager.prepare_owned(input(&history())).await.unwrap_err();

    assert!(matches!(
        error,
        NativeContextError::CompactionInputLimitExceeded { limit: 900, .. }
    ));
    assert!(manager.compactor().requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_or_oversized_native_results_fail_closed_when_compaction_is_required() {
    let invalid = manager(1_000, 3_000, window(0));
    assert!(matches!(
        invalid.prepare_owned(input(&history())).await.unwrap_err(),
        NativeContextError::InvalidWindow
    ));
    assert!(
        invalid
            .checkpoint_store()
            .latest("native-run".to_owned())
            .await
            .unwrap()
            .is_none()
    );

    let oversized = manager(1_000, 3_000, window(1_100));
    assert!(matches!(
        oversized
            .prepare_owned(input(&history()))
            .await
            .unwrap_err(),
        NativeContextError::InputTokenLimitExceeded { .. }
    ));
    assert!(
        oversized
            .checkpoint_store()
            .latest("native-run".to_owned())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn failed_soft_compaction_preserves_the_usable_original_request() {
    let manager = manager(3_000, 3_000, window(0));
    let run = history();

    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert!(prepared.request.provider_context.is_none());
    assert!(prepared.request.messages.iter().any(|message| {
        (message).content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolResult {
                    is_error: false,
                    ..
                }
            )
        })
    }));
    assert!(
        manager
            .checkpoint_store()
            .latest(run.id().to_owned())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn protected_user_content_above_the_soft_threshold_does_not_trigger_futile_compaction() {
    let manager = manager(3_000, 3_000, window(20));
    let run = AgentRun::new("protected-floor", "agent", "p".repeat(1_000));

    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert!(manager.compactor().requests.lock().unwrap().is_empty());
    assert!(prepared.request.provider_context.is_none());
}

#[tokio::test]
async fn a_reduced_native_window_above_the_desired_target_can_still_recover_a_hard_limit() {
    let manager = manager(1_000, 3_000, window(700));

    let prepared = manager.prepare_owned(input(&history())).await.unwrap();

    let tokens = manager.token_counter().count_tokens(&prepared.request);
    assert!(tokens > manager.policy().compaction_target());
    assert!(tokens <= manager.policy().input_token_limit());
    assert!(prepared.request.provider_context.is_some());
}

#[tokio::test]
async fn protected_context_is_reinjected_after_server_compaction_but_not_each_response() {
    let manager = manager(3_000, 3_000, window(20));
    let run = pending_tool();
    let mut output = ModelOutput {
        provider_state: None,
        response: ModelResponse::final_answer("Done".to_owned()),
        usage: None,
        provider_context: Some(window(40)),
        context_compacted: true,
    };
    manager
        .observe_response(input(&run).as_ref(), &output)
        .await
        .unwrap();
    let after_compaction = manager.prepare_owned(input(&run)).await.unwrap();
    assert!(matches!(
        after_compaction.request.messages.as_slice(),
        [ModelMessage {
            context: Some(MessageContext::Protected),
            ..
        }]
    ));

    // A full subsequent response window already carries the restored snapshot.
    output.context_compacted = false;
    manager
        .observe_response(input(&run).as_ref(), &output)
        .await
        .unwrap();
    let continued = manager.prepare_owned(input(&run)).await.unwrap();
    assert!(continued.request.messages.is_empty());
}

#[tokio::test]
async fn server_compaction_does_not_insert_protected_messages_before_pending_tool_results() {
    let manager = manager(3_000, 3_000, window(20));
    let mut run = pending_tool();
    let output = ModelOutput {
        provider_state: None,
        response: ModelResponse::tool_calls("Inspecting".to_owned(), Vec::new()),
        usage: None,
        provider_context: Some(window(40)),
        context_compacted: true,
    };
    manager
        .observe_response(input(&run).as_ref(), &output)
        .await
        .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "read-call".to_owned(),
        output: "contents".to_owned(),
    })
    .unwrap();

    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert!(matches!(
        prepared.request.messages.as_slice(),
        [
            ModelMessage {
                role: ModelRole::Tool,
                ..
            },
            ModelMessage {
                context: Some(MessageContext::Protected),
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn observed_native_state_rejects_invalid_or_incompatible_provider_metadata() {
    let manager = manager(3_000, 3_000, window(20));
    let run = pending_tool();
    for mismatch in [false, true] {
        let mut state = window(40);
        if mismatch {
            state.identity.format = "other-version".to_owned();
        } else {
            state.input_tokens = 0;
        }
        let output = ModelOutput {
            provider_state: None,
            response: ModelResponse::final_answer("Done".to_owned()),
            usage: None,
            provider_context: Some(state),
            context_compacted: false,
        };
        let error = manager
            .observe_response(input(&run).as_ref(), &output)
            .await
            .unwrap_err();
        if mismatch {
            assert!(matches!(error, NativeContextError::IncompatibleWindow));
        } else {
            assert!(matches!(error, NativeContextError::InvalidWindow));
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
}

struct LegacyModel;

impl Model for LegacyModel {
    type Error = Infallible;

    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(ModelResponse::final_answer("Done".to_owned()))
    }
}

#[tokio::test]
async fn existing_model_adapters_get_a_default_metadata_wrapper() {
    let output = LegacyModel
        .respond_with_metadata(ModelRequest {
            instructions: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_output_tokens: Some(50),
            provider_context: None,
        })
        .await
        .unwrap();

    assert_eq!(
        output.response,
        ModelResponse::final_answer("Done".to_owned())
    );
    assert_eq!(output.usage, None);
    assert_eq!(output.provider_context, None);
}
