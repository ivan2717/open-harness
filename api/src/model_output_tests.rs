use super::*;
use agent_core::agent::AgentRun;
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContextInput, ContextManager,
    InMemoryContextCheckpointStore,
};
use serde_json::json;

fn input(protocol: ProviderProtocol, mode: OutputLimitMode) -> ConfigureModel {
    ConfigureModel {
        protocol,
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key: "local-fixture-key".into(),
        model: "output-fixture".into(),
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        output_limit_mode: mode,
        reasoning_mode: "off".into(),
        reasoning_effort: "medium".into(),
        reasoning_capability: None,
        reasoning_default: None,
        supports_images: false,
    }
}

#[test]
fn legacy_configuration_preserves_explicit_output_limit_and_identity() {
    let legacy: ConfigureModel = serde_json::from_value(json!({
        "protocol":"responses", "base_url":"http://127.0.0.1:1/v1",
        "api_key":"local-fixture-key", "model":"output-fixture",
        "context_window":1_000_000, "max_output_tokens":128_000,
        "reasoning_mode":"off", "reasoning_effort":"medium"
    }))
    .unwrap();
    assert_eq!(legacy.output_limit_mode, OutputLimitMode::Custom);
    let legacy = configure(legacy).unwrap();
    let custom = configure(input(ProviderProtocol::Responses, OutputLimitMode::Custom)).unwrap();
    let automatic = configure(input(ProviderProtocol::Responses, OutputLimitMode::Auto)).unwrap();
    assert_eq!(legacy.info.id, custom.info.id);
    assert_ne!(legacy.info.id, automatic.info.id);
    assert_eq!(
        legacy.model.provider_context_identity(),
        automatic.model.provider_context_identity()
    );
}

#[tokio::test]
async fn configured_output_mode_survives_context_preparation_for_every_protocol() {
    for protocol in [
        ProviderProtocol::ChatCompletions,
        ProviderProtocol::Responses,
        ProviderProtocol::AnthropicMessages,
    ] {
        for mode in [OutputLimitMode::Auto, OutputLimitMode::Custom] {
            let configured = configure(input(protocol, mode)).unwrap();
            let metadata = configured.model.context_metadata().unwrap().clone();
            let policy = ContextPolicy::builder()
                .model_context_window(metadata.context_window)
                .output_token_reserve(metadata.max_output_tokens.unwrap())
                .safety_margin(2048)
                .build_for_model(&metadata)
                .unwrap();
            let manager = CompactingContextManager::new(
                policy,
                ModelContextCompactor::new(configured.model),
                InMemoryContextCheckpointStore::new(),
                ApproximateTokenCounter::default(),
            );
            let run = AgentRun::new("automatic-output", "agent", "Reply briefly");
            let context = ContextInput::new(run.id(), "", run.events().to_vec(), vec![])
                .with_model_metadata(metadata);
            let prepared = manager.prepare(context.as_ref()).await.unwrap();
            let omit =
                mode == OutputLimitMode::Auto && protocol != ProviderProtocol::AnthropicMessages;
            assert_eq!(
                prepared.request.max_output_tokens,
                if omit { None } else { Some(128_000) }
            );
            assert_eq!(policy.input_token_limit(), 869_952);
            assert_eq!(policy.model_budget().output_token_reserve(), 128_000);
        }
    }
}
