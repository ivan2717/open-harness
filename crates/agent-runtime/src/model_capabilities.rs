//! Execution-relevant capabilities. Wire field names and compatibility switches
//! belong to adapters, not to the harness's model contract.

use std::{collections::HashSet, error::Error, fmt};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub reasoning: ReasoningCapabilities,
    pub context: ContextCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReasoningCapabilities {
    /// Adapter-defined choices. An identically named choice on two models does
    /// not promise an equivalent token budget or level of reasoning.
    pub options: Vec<ReasoningOption>,
    pub default_option: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContextCapabilities {
    pub accepts_native_context: bool,
    pub supports_native_compaction: bool,
    /// Per-response replay bindings decide whether instructions and tool
    /// definitions may change while retaining historical native state.
    pub replay_requirement: ReplayRequirement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayRequirement {
    #[default]
    Optional,
    /// Every assistant response, including restored history, needs compatible
    /// adapter-owned replay state before it can participate in another request.
    Required,
}

impl ModelCapabilities {
    pub fn validate(&self) -> Result<(), ModelCapabilitiesError> {
        let mut identifiers = HashSet::new();
        for option in &self.reasoning.options {
            if option.id.trim().is_empty() || option.label.trim().is_empty() {
                return Err(ModelCapabilitiesError::EmptyReasoningOption);
            }
            if !identifiers.insert(option.id.as_str()) {
                return Err(ModelCapabilitiesError::DuplicateReasoningOption);
            }
        }
        if let Some(default) = &self.reasoning.default_option
            && !identifiers.contains(default.as_str())
        {
            return Err(ModelCapabilitiesError::UnknownDefaultReasoningOption);
        }
        if self.context.supports_native_compaction && !self.context.accepts_native_context {
            return Err(ModelCapabilitiesError::CompactionWithoutNativeContext);
        }
        Ok(())
    }

    /// Validate an explicit choice without translating it to a provider field.
    /// `None` delegates the default to the configured adapter.
    pub fn validate_reasoning_option(
        &self,
        option: Option<&str>,
    ) -> Result<(), ModelCapabilitiesError> {
        self.validate()?;
        if let Some(option) = option
            && !self
                .reasoning
                .options
                .iter()
                .any(|supported| supported.id == option)
        {
            return Err(ModelCapabilitiesError::UnsupportedReasoningOption);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelCapabilitiesError {
    EmptyReasoningOption,
    DuplicateReasoningOption,
    UnknownDefaultReasoningOption,
    UnsupportedReasoningOption,
    CompactionWithoutNativeContext,
}

impl fmt::Display for ModelCapabilitiesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyReasoningOption => {
                "reasoning option identifiers and labels must be nonempty"
            }
            Self::DuplicateReasoningOption => "reasoning option identifiers must be unique",
            Self::UnknownDefaultReasoningOption => {
                "default reasoning option is not advertised by the adapter"
            }
            Self::UnsupportedReasoningOption => {
                "requested reasoning option is not advertised by the adapter"
            }
            Self::CompactionWithoutNativeContext => {
                "native compaction requires native context support"
            }
        })
    }
}

impl Error for ModelCapabilitiesError {}
