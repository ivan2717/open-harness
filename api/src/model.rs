use crate::{
    context::DemoCompactor,
    contracts::{ConfigureModel, ModelInfo, ModelReasoning},
    demo::{DemoModel, DemoTools},
};
use agent_core::agent::RunEventEnvelope;
use agent_providers::{HttpModel, ProviderConfig, ProviderError, ProviderProtocol, ReasoningMode};
use agent_runtime::{
    ContentBlock, ContextCompactor, ContextPolicy, ContextSummary, Model, ModelCapabilities,
    ModelContextCompactor, ModelContextMetadata, ModelOutput, ModelRequest, ModelResponse,
    ModelStreamSink, ProviderContextIdentity, ProviderResponseState, ProviderStateError,
    TokenCounter, ToolCall, ToolDefinition, ToolExecutor,
};
use std::{io, sync::Arc};

#[derive(Clone)]
pub enum ServiceModel {
    Demo(Arc<DemoModel>),
    Http(Arc<HttpModel>),
    Unavailable,
}
impl ServiceModel {
    pub fn available(&self) -> bool {
        !matches!(self, Self::Unavailable)
    }
}
impl Model for ServiceModel {
    type Error = ProviderError;
    fn is_context_overflow(&self, error: &Self::Error) -> bool {
        matches!(self, Self::Http(model) if model.is_context_overflow(error))
    }
    fn capabilities(&self) -> ModelCapabilities {
        match self {
            Self::Http(model) => model.capabilities(),
            _ => ModelCapabilities::default(),
        }
    }
    fn context_metadata(&self) -> Option<&ModelContextMetadata> {
        match self {
            Self::Http(model) => model.context_metadata(),
            _ => None,
        }
    }
    fn provider_context_identity(&self) -> Option<&ProviderContextIdentity> {
        match self {
            Self::Http(model) => model.provider_context_identity(),
            _ => None,
        }
    }
    fn requires_provider_state(&self) -> bool {
        matches!(self, Self::Http(model) if model.requires_provider_state())
    }
    fn validate_replay_state(
        &self,
        state: &ProviderResponseState,
        content: &[ContentBlock],
    ) -> Result<(), ProviderStateError> {
        match self {
            Self::Http(model) => model.validate_replay_state(state, content),
            _ => Err(ProviderStateError::UnsupportedFormat),
        }
    }
    async fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Self::Error> {
        match self {
            Self::Demo(model) => {
                model.stream(request, sink).await.unwrap();
                Ok(())
            }
            Self::Http(model) => model.stream(request, sink).await,
            Self::Unavailable => Err(ProviderError::InvalidConfig(
                "configure this model before continuing the conversation",
            )),
        }
    }
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(self.respond_with_metadata(request).await?.response)
    }
    async fn respond_with_metadata(
        &self,
        request: ModelRequest,
    ) -> Result<ModelOutput, Self::Error> {
        match self {
            Self::Demo(model) => Ok(model.respond_with_metadata(request).await.unwrap()),
            Self::Http(model) => model.respond_with_metadata(request).await,
            Self::Unavailable => Err(ProviderError::InvalidConfig(
                "configure this model before continuing the conversation",
            )),
        }
    }
}

pub enum ServiceTools {
    Demo(DemoTools),
    Host(Box<crate::service::collaboration::tools::RunTools>),
}
impl ToolExecutor for ServiceTools {
    type Error = io::Error;
    fn definitions(&self) -> &[ToolDefinition] {
        match self {
            Self::Demo(tools) => tools.definitions(),
            Self::Host(tools) => tools.definitions(),
        }
    }
    fn snapshot_definitions(&self) -> Vec<ToolDefinition> {
        match self {
            Self::Demo(tools) => tools.snapshot_definitions(),
            Self::Host(tools) => tools.snapshot_definitions(),
        }
    }
    async fn execute(&self, call: ToolCall) -> Result<String, Self::Error> {
        match self {
            Self::Demo(tools) => tools.execute(call).await,
            Self::Host(tools) => tools.execute(call).await,
        }
    }
    async fn execute_output(&self, call: ToolCall) -> Result<agent_core::ToolOutput, Self::Error> {
        match self {
            Self::Demo(tools) => tools.execute_output(call).await,
            Self::Host(tools) => tools.execute_output(call).await,
        }
    }
    async fn pending_messages(&self) -> Result<Vec<agent_core::AgentMessage>, Self::Error> {
        match self {
            Self::Demo(_) => Ok(Vec::new()),
            Self::Host(tools) => tools.pending_messages().await,
        }
    }
    async fn wait_for_messages(&self) -> Result<Vec<agent_core::AgentMessage>, Self::Error> {
        match self {
            Self::Demo(_) => Ok(Vec::new()),
            Self::Host(tools) => tools.wait_for_messages().await,
        }
    }
    async fn acknowledge_messages(&self, ids: &[String]) -> Result<(), Self::Error> {
        match self {
            Self::Demo(_) => Ok(()),
            Self::Host(tools) => tools.acknowledge_messages(ids).await,
        }
    }
}

pub enum ServiceCompactor {
    Demo(DemoCompactor),
    Http(Box<ModelContextCompactor<ServiceModel>>),
}
impl ContextCompactor for ServiceCompactor {
    type Error = io::Error;
    fn input_token_limit(&self) -> Option<usize> {
        match self {
            Self::Demo(c) => c.input_token_limit(),
            Self::Http(c) => c.input_token_limit(),
        }
    }
    fn estimate_input_tokens(
        &self,
        previous: Option<&ContextSummary>,
        events: &[RunEventEnvelope],
        counter: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        match self {
            Self::Demo(c) => Ok(c.estimate_input_tokens(previous, events, counter).unwrap()),
            Self::Http(c) => c
                .estimate_input_tokens(previous, events, counter)
                .map_err(io::Error::other),
        }
    }
    fn estimate_projected_input_tokens(
        &self,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
    ) -> Result<Option<usize>, Self::Error> {
        match self {
            Self::Demo(c) => Ok(c
                .estimate_projected_input_tokens(metadata, request)
                .unwrap()),
            Self::Http(c) => c
                .estimate_projected_input_tokens(metadata, request)
                .map_err(io::Error::other),
        }
    }
    async fn compact_projected(
        &self,
        metadata: Option<ModelContextMetadata>,
        request: ModelRequest,
    ) -> Result<Option<ContextSummary>, Self::Error> {
        match self {
            Self::Demo(c) => Ok(c.compact_projected(metadata, request).await.unwrap()),
            Self::Http(c) => c
                .compact_projected(metadata, request)
                .await
                .map_err(io::Error::other),
        }
    }
    async fn compact(
        &self,
        previous: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        match self {
            Self::Demo(c) => Ok(c.compact(previous, events).await.unwrap()),
            Self::Http(c) => c.compact(previous, events).await.map_err(io::Error::other),
        }
    }
}

#[derive(Clone)]
pub struct ConfiguredModel {
    pub info: ModelInfo,
    pub model: HttpModel,
}
pub fn configure(input: ConfigureModel) -> Result<ConfiguredModel, ProviderError> {
    if input.reasoning_capability.is_some() && input.reasoning_mode != "off" {
        return Err(ProviderError::InvalidConfig(
            "use reasoning_default with a capability declaration, not legacy reasoning_mode",
        ));
    }
    let reasoning = match input.reasoning_mode.as_str() {
        "off" => ReasoningMode::Off,
        "standard" if input.protocol == ProviderProtocol::AnthropicMessages => {
            ReasoningMode::AnthropicBudget {
                tokens: match input.reasoning_effort.as_str() {
                    "low" => 1024,
                    "medium" => 4096,
                    "high" => 8192,
                    _ => return Err(ProviderError::InvalidConfig("unsupported reasoning effort")),
                }
                .min(input.max_output_tokens.saturating_sub(1)),
            }
        }
        "standard" => ReasoningMode::Effort {
            effort: input.reasoning_effort.clone(),
        },
        "deepseek" => ReasoningMode::DeepSeek {
            effort: input.reasoning_effort.clone(),
        },
        _ => return Err(ProviderError::InvalidConfig("unsupported reasoning mode")),
    };
    ContextPolicy::builder()
        .model_context_window(input.context_window)
        .output_token_reserve(input.max_output_tokens)
        .safety_margin((input.context_window / 100).clamp(64, 2048))
        .build()
        .map_err(|_| {
            ProviderError::InvalidConfig(
                "context window must leave input capacity after output and safety reserves",
            )
        })?;
    // Versioned deterministic identifier includes all wire/accounting settings,
    // never the secret. Credential rotation preserves conversation identity.
    let mut identity = serde_json::to_vec(&(
        input.protocol,
        input.base_url.trim_end_matches('/'),
        &input.model,
        input.context_window,
        input.max_output_tokens,
        &reasoning,
    ))
    .map_err(|_| ProviderError::InvalidConfig("invalid model configuration"))?;
    // Old profiles retain their identifiers. New declarations are connection
    // specific and cannot accidentally reuse a profile with a different mapping.
    if input.reasoning_capability.is_some() || input.reasoning_default.is_some() {
        identity.extend_from_slice(b".reasoning.v2:");
        identity.extend(
            serde_json::to_vec(&(&input.reasoning_capability, &input.reasoning_default))
                .map_err(|_| ProviderError::InvalidConfig("invalid reasoning configuration"))?,
        );
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in identity {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    let mut info = ModelInfo {
        id: format!("model-{hash:016x}"),
        name: input.model.clone(),
        description: format!(
            "{} · {} token context",
            input.protocol.wire().as_str(),
            input.context_window
        ),
        available: true,
        context_window: input.context_window,
        supports_images: input.supports_images,
        reasoning: None,
    };
    let mut model = HttpModel::new(ProviderConfig {
        protocol: input.protocol,
        base_url: input.base_url,
        api_key: input.api_key,
        model: input.model,
        context_window: input.context_window,
        max_output_tokens: input.max_output_tokens,
        supports_images: input.supports_images,
        reasoning,
    })?;
    if let Some(capability) = input.reasoning_capability {
        model = model.with_reasoning_capability(capability)?;
    }
    if let Some(effort) = input.reasoning_default.as_deref() {
        model = model.with_reasoning_effort(effort)?;
    }
    info.reasoning = Some(ModelReasoning {
        default_effort: input
            .reasoning_default
            .or_else(|| (input.reasoning_mode != "off").then_some(input.reasoning_effort)),
        supported_efforts: model
            .supported_reasoning_efforts()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        capability: Some(model.reasoning_capability().clone()),
    });
    Ok(ConfiguredModel { info, model })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(key: &str) -> ConfigureModel {
        ConfigureModel {
            supports_images: false,
            protocol: ProviderProtocol::Responses,
            base_url: "https://api.example.com/v1".into(),
            api_key: key.into(),
            model: "fixture-model".into(),
            context_window: 32000,
            max_output_tokens: 4096,
            reasoning_mode: "off".into(),
            reasoning_effort: "high".into(),
            reasoning_capability: None,
            reasoning_default: None,
        }
    }
    #[test]
    fn credential_rotation_keeps_identity_and_invalid_budget_is_rejected_early() {
        let first = configure(input("first-secret")).unwrap();
        let second = configure(input("second-secret")).unwrap();
        assert_eq!(first.info.id, second.info.id);
        let serialized = serde_json::to_string(&first.info).unwrap();
        assert!(!serialized.contains("first-secret"));
        let mut changed = input("first-secret");
        changed.context_window = 64000;
        assert_ne!(first.info.id, configure(changed).unwrap().info.id);
        let mut invalid = input("first-secret");
        invalid.context_window = 100;
        invalid.max_output_tokens = 80;
        assert!(configure(invalid).is_err());
    }

    fn declared() -> agent_providers::ReasoningCapability {
        serde_json::from_value(serde_json::json!({
            "source":"manual", "adapter":"effort",
            "options":[{"id":"balanced","label":"Balanced","effort":"medium"}, {"id":"maximum","effort":"max"}]
        })).unwrap()
    }

    #[test]
    fn capability_declarations_and_defaults_are_validated_and_part_of_profile_identity() {
        let old = configure(input("key")).unwrap();
        let preset = old.info.reasoning.unwrap();
        assert_eq!(preset.supported_efforts, vec!["low", "medium", "high"]);
        assert_eq!(
            preset.capability.unwrap().source,
            agent_providers::ReasoningSource::Preset
        );
        assert_eq!(preset.default_effort, None);
        let mut declared_input = input("key");
        declared_input.reasoning_capability = Some(declared());
        let declared_model = configure(declared_input).unwrap();
        let info = declared_model.info.reasoning.as_ref().unwrap();
        assert_eq!(info.default_effort, None);
        assert_eq!(info.supported_efforts, vec!["balanced", "maximum"]);
        assert_ne!(old.info.id, declared_model.info.id);
        let mut selected = input("rotated-key");
        selected.reasoning_capability = Some(declared());
        selected.reasoning_default = Some("maximum".into());
        let selected = configure(selected).unwrap();
        assert_eq!(
            selected.info.reasoning.unwrap().default_effort.as_deref(),
            Some("maximum")
        );
        assert_ne!(selected.info.id, declared_model.info.id);
        let mut invalid = input("key");
        invalid.reasoning_capability = Some(declared());
        invalid.reasoning_default = Some("high".into());
        assert!(configure(invalid).is_err());
        let mut conflicting = input("key");
        conflicting.reasoning_mode = "standard".into();
        conflicting.reasoning_capability = Some(declared());
        assert!(configure(conflicting).is_err());
    }

    #[test]
    fn legacy_explicit_modes_keep_defaults_and_budget_default_rejects_insufficient_output() {
        let mut legacy = input("key");
        legacy.protocol = ProviderProtocol::ChatCompletions;
        legacy.reasoning_mode = "deepseek".into();
        let configured = configure(legacy).unwrap();
        let info = configured.info.reasoning.unwrap();
        assert_eq!(info.default_effort.as_deref(), Some("high"));
        assert_eq!(
            info.capability.unwrap().source,
            agent_providers::ReasoningSource::Legacy
        );
        let mut budget = input("key");
        budget.protocol = ProviderProtocol::AnthropicMessages;
        budget.reasoning_capability = Some(serde_json::from_value(serde_json::json!({
            "source":"manual","adapter":"anthropic_budget","options":[{"id":"budget","budget_tokens":4096}]
        })).unwrap());
        budget.reasoning_default = Some("budget".into());
        assert!(configure(budget).is_err());
    }
}
