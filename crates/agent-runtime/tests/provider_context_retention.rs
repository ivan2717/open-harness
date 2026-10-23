use agent_adapters::WireProtocol;
use agent_runtime::ModelRole;
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

use agent_core::agent::{AgentRun, RunEvent, RunEventEnvelope};
use agent_runtime::*;
use serde_json::{Value, json};

const SECRET: &str = "opaque-not-for-summary-or-history";

fn identity() -> ProviderContextIdentity {
    ProviderContextIdentity {
        provider: "openai-test".into(),
        model: "test-model".into(),
        format: "responses-v1".into(),
    }
}

fn append_step(run: &mut AgentRun, index: usize, scope: ProviderReplayScope) {
    let id = format!("call-{index}");
    let state = WireProtocol::Responses.capture_state(identity(), scope, json!([
        {"type":"reasoning", "id":format!("rs-{index}"), "encrypted_content":SECRET, "summary":[]},
        {"type":"message", "role":"assistant", "phase":"commentary", "content":[{"type":"output_text","text":"Inspect"}]},
        {"type":"function_call", "call_id":id, "name":"read", "arguments":"{}"}
    ]), 100).unwrap();
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        content: "Inspect".into(),
        provider_state: Some(state),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: id.clone(),
        tool_name: "read".into(),
        arguments: "{}".into(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: id,
        output: "x".repeat(2000),
        images: Vec::new(),
    })
    .unwrap();
}

#[derive(Clone, Default)]
struct SummarySpy(Arc<Mutex<Vec<RunEventEnvelope>>>);
impl ContextCompactor for SummarySpy {
    type Error = Infallible;
    fn estimate_input_tokens(
        &self,
        _: Option<&ContextSummary>,
        events: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        assert!(!serde_json::to_string(events).unwrap().contains(SECRET));
        Ok(100)
    }
    async fn compact(
        &self,
        _: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.0.lock().unwrap().extend(events);
        Ok(ContextSummary {
            objective: "Continue inspection".into(),
            ..Default::default()
        })
    }
}

fn policy(input_limit: usize) -> ContextPolicy {
    ContextPolicy::builder()
        .input_token_limit(input_limit)
        .compaction_threshold(4500)
        .compaction_target(3500)
        .recent_tail_tokens(2500)
        .max_tool_output_chars(2000)
        .build()
        .unwrap()
}

#[tokio::test]
async fn compaction_preserves_retained_response_items_and_scrubs_summary_input() {
    let mut run = AgentRun::new("state-retention", "agent", "Inspect files");
    for index in 0..3 {
        append_step(&mut run, index, ProviderReplayScope::Response);
    }
    let original = run.events().to_vec();
    let spy = SummarySpy::default();
    let manager = CompactingContextManager::new(
        policy(10000),
        spy.clone(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let prepared = manager
        .prepare_owned(ContextInput::new(
            run.id(),
            "instructions",
            original.clone(),
            vec![],
        ))
        .await
        .unwrap();
    assert!(prepared.omitted_history.is_some());
    prepared
        .request
        .validate_provider_state(Some(&identity()), true)
        .unwrap();
    let retained = prepared
        .request
        .messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage {
                role: ModelRole::Assistant,
                provider_state: Some(state),
                ..
            } => Some(state),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 1);
    let latest = original
        .iter()
        .rev()
        .find_map(|event| match &event.event {
            RunEvent::ModelResponseReceived {
                provider_state: Some(state),
                ..
            } => Some(state),
            _ => None,
        })
        .unwrap();
    assert_eq!(retained[0], latest);
    assert!(
        spy.0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event.event, RunEvent::ModelResponseReceived { .. }))
    );
    assert!(
        !serde_json::to_string(&*spy.0.lock().unwrap())
            .unwrap()
            .contains(SECRET)
    );
    assert_eq!(run.events(), original);
}

#[tokio::test]
async fn prefix_bound_state_skips_soft_compaction_and_rejects_hard_overflow() {
    let mut run = AgentRun::new("prefix", "agent", "Inspect files");
    for index in 0..3 {
        append_step(&mut run, index, ProviderReplayScope::Prefix);
    }
    let spy = SummarySpy::default();
    for limit in [10000, 6000] {
        let manager = CompactingContextManager::new(
            policy(limit),
            spy.clone(),
            InMemoryContextCheckpointStore::new(),
            ApproximateTokenCounter::new(1).unwrap(),
        );
        let result = manager
            .prepare_owned(ContextInput::new(
                run.id(),
                "instructions",
                run.events().to_vec(),
                vec![],
            ))
            .await;
        if limit == 10000 {
            assert!(result.unwrap().omitted_history.is_none());
        } else {
            assert!(matches!(
                result,
                Err(CompactingContextError::PrefixBoundStateRequiresNativeCompaction)
            ));
        }
    }
    assert!(spy.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn derived_prefix_is_rejected_even_below_compaction_threshold_for_bound_state() {
    let mut run = AgentRun::new("prefix", "agent", "Inspect files");
    append_step(&mut run, 0, ProviderReplayScope::Prefix);
    let manager = CompactingContextManager::new(
        policy(10_000),
        SummarySpy::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    );
    let input =
        ContextInput::new(run.id(), "", run.events().to_vec(), vec![]).with_derived_messages(vec![
            ModelMessage::summary("wiki:revision".into(), "Historical notes".into()),
        ]);
    assert!(matches!(
        manager.prepare_owned(input).await,
        Err(CompactingContextError::DerivedContextWithPrefixBoundState)
    ));
    assert!(manager.compactor().0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn history_read_and_search_never_return_opaque_provider_state() {
    let mut run = AgentRun::new("history-state", "agent", "Inspect files");
    append_step(&mut run, 0, ProviderReplayScope::Response);
    let store = InMemoryEventStore::new();
    for event in run.events() {
        store.append(run.id(), run.agent_id(), event).await.unwrap();
    }
    let access = EventStoreHistoryAccess::new();
    let context = HistoryContext {
        run_id: run.id().into(),
        through_sequence: run.events().last().unwrap().sequence,
        checkpoint_id: "checkpoint".into(),
    };
    for (name, arguments) in [
        (SEARCH_RUN_HISTORY_TOOL, json!({"query":SECRET})),
        (SEARCH_RUN_HISTORY_TOOL, json!({"query":"Inspect"})),
        (
            READ_RUN_HISTORY_TOOL,
            json!({"from_sequence":1,"to_sequence":4}),
        ),
    ] {
        let output = access
            .execute(
                &store,
                context.clone(),
                ToolCall {
                    id: "history".into(),
                    name: name.into(),
                    arguments: arguments.to_string(),
                },
            )
            .await
            .unwrap();
        assert!(!output.contains(SECRET));
        assert!(!output.contains("encrypted_content"));
        if name == SEARCH_RUN_HISTORY_TOOL && arguments["query"] == SECRET {
            let parsed: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(parsed["total_matches"], 0);
        }
    }
    assert!(
        serde_json::to_string(&store.load(run.id()).await.unwrap().unwrap().events)
            .unwrap()
            .contains(SECRET)
    );
}

struct SummaryModel;
impl Model for SummaryModel {
    type Error = Infallible;
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        assert!(!serde_json::to_string(&request).unwrap().contains(SECRET));
        Ok(ModelResponse::final_answer(
            serde_json::to_string(&ContextSummary {
                objective: "Continue".into(),
                ..Default::default()
            })
            .unwrap(),
        ))
    }
}

#[tokio::test]
async fn direct_model_compactor_also_scrubs_raw_event_state() {
    let mut run = AgentRun::new("direct-summary", "agent", "Inspect");
    append_step(&mut run, 0, ProviderReplayScope::Response);
    ModelContextCompactor::new(SummaryModel)
        .compact(None, run.events().to_vec())
        .await
        .unwrap();
}
