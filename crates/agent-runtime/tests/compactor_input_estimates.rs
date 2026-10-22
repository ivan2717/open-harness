use agent_runtime::{
    ContextCompactor, ContextSummary, Model, ModelContextBudget, ModelContextCompactor,
    ModelContextCompactorError, ModelOutput, ModelRequest, ModelResponse, ModelUsage, TokenCounter,
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
struct SummaryModel(Arc<AtomicUsize>);
impl Model for SummaryModel {
    type Error = io::Error;
    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse::final_answer(serde_json::to_string(
            &ContextSummary {
                objective: "preserve constraints".into(),
                ..Default::default()
            },
        )?))
    }
    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        Ok(ModelOutput {
            usage: Some(ModelUsage {
                input_tokens: 50,
                output_tokens: 1,
                cached_input_tokens: None,
            }),
            ..self.respond(request).await?.into()
        })
    }
}

struct Counter(usize);
impl TokenCounter for Counter {
    fn count_tokens(&self, _: &ModelRequest) -> usize {
        self.0
    }
}

#[tokio::test]
async fn summary_budget_uses_a_local_estimate_with_margin() {
    let generations = Arc::new(AtomicUsize::new(0));
    let compactor = ModelContextCompactor::new(SummaryModel(generations.clone()))
        .with_budget(ModelContextBudget::new(500, 100, 0).unwrap())
        .with_token_counter(Counter(50));
    assert_eq!(
        compactor
            .estimate_input_tokens(None, &[], compactor.token_counter())
            .unwrap(),
        58,
    );
    let summary = compactor.compact(None, vec![]).await.unwrap();
    assert_eq!(summary.objective, "preserve constraints");
    assert_eq!(generations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn summary_rejects_an_oversized_local_estimate_before_generation() {
    let generations = Arc::new(AtomicUsize::new(0));
    let compactor = ModelContextCompactor::new(SummaryModel(generations.clone()))
        .with_budget(ModelContextBudget::new(500, 100, 0).unwrap())
        .with_token_counter(Counter(600));
    assert!(matches!(
        compactor.compact(None, vec![]).await,
        Err(ModelContextCompactorError::InputTokenLimitExceeded {
            estimated_tokens: 690,
            ..
        })
    ));
    assert_eq!(generations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn summary_rejects_an_input_that_only_fits_without_uncertainty_margin() {
    let generations = Arc::new(AtomicUsize::new(0));
    let compactor = ModelContextCompactor::new(SummaryModel(generations.clone()))
        .with_budget(ModelContextBudget::new(500, 100, 0).unwrap())
        .with_token_counter(Counter(390));
    assert!(matches!(
        compactor.compact(None, vec![]).await,
        Err(ModelContextCompactorError::InputTokenLimitExceeded {
            estimated_tokens: 449,
            ..
        })
    ));
    assert_eq!(generations.load(Ordering::SeqCst), 0);
}

#[derive(Clone)]
struct ProjectedSummaryModel {
    metadata: agent_runtime::ModelContextMetadata,
    identity: Option<agent_runtime::ProviderContextIdentity>,
    requests: Arc<std::sync::Mutex<Vec<ModelRequest>>>,
}

impl Model for ProjectedSummaryModel {
    type Error = io::Error;
    fn context_metadata(&self) -> Option<&agent_runtime::ModelContextMetadata> {
        Some(&self.metadata)
    }
    fn provider_context_identity(&self) -> Option<&agent_runtime::ProviderContextIdentity> {
        self.identity.as_ref()
    }
    fn validate_replay_state(
        &self,
        state: &agent_runtime::ProviderResponseState,
        content: &[agent_runtime::ContentBlock],
    ) -> Result<(), agent_runtime::ProviderStateError> {
        agent_adapters::validate_replay_state(state, content)
    }
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(ModelResponse::final_answer(serde_json::to_string(
            &ContextSummary {
                objective: "preserve the projected conversation".into(),
                ..Default::default()
            },
        )?))
    }
}

fn projected_model() -> ProjectedSummaryModel {
    ProjectedSummaryModel {
        metadata: agent_runtime::ModelContextMetadata::new("provider", "same-model", 100_000)
            .unwrap()
            .with_accounting_identity("chat-v1"),
        identity: None,
        requests: Arc::default(),
    }
}

fn projected_request() -> ModelRequest {
    ModelRequest {
        instructions: "Original stable instructions".into(),
        messages: vec![
            agent_runtime::ModelMessage::user("Original task".into()),
            agent_runtime::ModelMessage::assistant_blocks(
                vec![agent_runtime::ContentBlock::ToolCall {
                    id: "read-1".into(),
                    name: "read".into(),
                    arguments: "{}".into(),
                }],
                None,
            ),
            agent_runtime::ModelMessage::tool_result(
                "read-1".into(),
                "Original result".into(),
                false,
            ),
        ],
        tools: vec![agent_runtime::ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: "{}".into(),
        }],
        max_output_tokens: Some(8192),
        provider_context: None,
    }
}

#[tokio::test]
async fn same_route_summary_preserves_prefix_and_uses_its_independent_budget() {
    let model = projected_model();
    let requests = model.requests.clone();
    let metadata = model.metadata.clone();
    let compactor = ModelContextCompactor::new(model)
        .with_budget(ModelContextBudget::new(20_000, 1000, 0).unwrap());
    let request = projected_request();
    let estimate = compactor
        .estimate_projected_input_tokens(Some(&metadata), &request)
        .unwrap()
        .unwrap();
    let summary = compactor
        .compact_projected(Some(metadata), request.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.objective, "preserve the projected conversation");
    let sent = requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let sent = &sent[0];
    assert_eq!(sent.instructions, request.instructions);
    assert_eq!(sent.tools, request.tools);
    assert_eq!(sent.messages[..request.messages.len()], request.messages);
    assert_eq!(sent.messages.len(), request.messages.len() + 1);
    assert_eq!(
        sent.messages.last().unwrap().role,
        agent_runtime::ModelRole::User
    );
    let instruction = serde_json::to_string(sent.messages.last().unwrap()).unwrap();
    assert!(instruction.contains("Return only one JSON object"));
    assert!(instruction.contains("do not continue the task or call tools"));
    assert_eq!(sent.max_output_tokens, Some(1000));
    let raw = compactor.token_counter().count_tokens(sent);
    assert_eq!(estimate, raw + (raw * 15).div_ceil(100));
}

#[tokio::test]
async fn projected_summary_declines_unknown_routes_and_unsafe_prefixes() {
    let model = projected_model();
    let requests = model.requests.clone();
    let metadata = model.metadata.clone();
    let compactor = ModelContextCompactor::new(model);
    for change in 0..6 {
        let mut request = projected_request();
        let mut route = Some(metadata.clone());
        match change {
            0 => route = None,
            1 => route.as_mut().unwrap().model = "other-model".into(),
            2 => route.as_mut().unwrap().accounting_identity = Some("responses-v1".into()),
            3 => {
                request.messages.pop();
            }
            4 => {
                request.messages.remove(1);
            }
            5 => {
                request.provider_context = Some(agent_runtime::ProviderContextWindow {
                    prefix_fingerprint: None,
                    identity: agent_runtime::ProviderContextIdentity {
                        provider: "provider".into(),
                        model: "same-model".into(),
                        format: "v1".into(),
                    },
                    items: vec![serde_json::json!({"opaque": "state"})],
                    input_tokens: 200,
                })
            }
            _ => unreachable!(),
        }
        assert!(
            compactor
                .estimate_projected_input_tokens(route.as_ref(), &request)
                .unwrap()
                .is_none()
        );
        assert!(
            compactor
                .compact_projected(route, request)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn projected_summary_checks_margin_before_generation() {
    let model = projected_model();
    let requests = model.requests.clone();
    let metadata = model.metadata.clone();
    let compactor = ModelContextCompactor::new(model)
        .with_budget(ModelContextBudget::new(500, 100, 0).unwrap())
        .with_token_counter(Counter(390));
    assert_eq!(
        compactor
            .estimate_projected_input_tokens(Some(&metadata), &projected_request())
            .unwrap(),
        Some(449)
    );
    assert!(matches!(
        compactor
            .compact_projected(Some(metadata), projected_request())
            .await,
        Err(ModelContextCompactorError::InputTokenLimitExceeded {
            estimated_tokens: 449,
            ..
        })
    ));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn projected_summary_preserves_validated_replay_and_declines_changed_payloads() {
    let mut model = projected_model();
    let identity = agent_runtime::ProviderContextIdentity {
        provider: "provider".into(),
        model: "same-model".into(),
        format: "chat-v1".into(),
    };
    model.identity = Some(identity.clone());
    let metadata = model.metadata.clone();
    let requests = model.requests.clone();
    let compactor = ModelContextCompactor::new(model);
    let mut request = projected_request();
    let state = agent_adapters::WireProtocol::ChatCompletions.capture_state(
        identity,
        agent_runtime::ProviderReplayScope::Response,
        serde_json::json!({"role": "assistant", "content": "", "reasoning_content": "retained reasoning", "tool_calls": [{"id": "read-1", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}),
        200,
    ).unwrap();
    request.messages[1].provider_state = Some(state);
    assert!(
        compactor
            .compact_projected(Some(metadata.clone()), request.clone())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        requests.lock().unwrap()[0].messages[..request.messages.len()],
        request.messages
    );

    let mut changed = request.clone();
    changed.messages[1].provider_state.as_mut().unwrap().payload["tool_calls"][0]["function"]["arguments"] =
        "{\"changed\":true}".into();
    assert!(
        compactor
            .estimate_projected_input_tokens(Some(&metadata), &changed)
            .unwrap()
            .is_none()
    );
    let mut changed = request;
    let state = changed.messages[1].provider_state.as_mut().unwrap();
    state.scope = agent_runtime::ProviderReplayScope::Prefix;
    state.prefix_fingerprint = Some("different-prefix".into());
    assert!(
        compactor
            .compact_projected(Some(metadata), changed)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn projected_summary_uses_the_same_fallback_protocol_identity_as_the_runtime() {
    let mut model = projected_model();
    model.metadata.accounting_identity = None;
    model.identity = Some(agent_runtime::ProviderContextIdentity {
        provider: "provider".into(),
        model: "same-model".into(),
        format: "chat-v1".into(),
    });
    let mut runtime_metadata = model.metadata.clone();
    runtime_metadata.accounting_identity = Some(
        serde_json::json!({
            "provider": "provider", "model": "same-model", "format": "chat-v1"
        })
        .to_string(),
    );
    let compactor = ModelContextCompactor::new(model);
    assert!(
        compactor
            .estimate_projected_input_tokens(Some(&runtime_metadata), &projected_request())
            .unwrap()
            .is_some()
    );
    runtime_metadata.accounting_identity = Some(
        serde_json::json!({
            "provider": "provider", "model": "same-model", "format": "responses-v1"
        })
        .to_string(),
    );
    assert!(
        compactor
            .estimate_projected_input_tokens(Some(&runtime_metadata), &projected_request())
            .unwrap()
            .is_none()
    );
}
