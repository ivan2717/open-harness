use super::{HttpModel, ProviderError, ProviderProtocol, ReasoningMode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Provenance of a declaration, not proof that a provider honored it at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSource {
    Unknown,
    Preset,
    Catalog,
    Metadata,
    Manual,
    Legacy,
}

/// The provider dialect is independent of its transport protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningAdapter {
    None,
    Effort,
    Deepseek,
    MessagesEffort,
    MessagesEffortOnly,
    AnthropicAdaptive,
    AnthropicBudget,
    ChatToggle,
    ThinkingToggle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningOption {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningCapability {
    pub source: ReasoningSource,
    pub adapter: ReasoningAdapter,
    pub options: Vec<ReasoningOption>,
}

impl Default for ReasoningCapability {
    fn default() -> Self {
        Self {
            source: ReasoningSource::Unknown,
            adapter: ReasoningAdapter::None,
            options: Vec::new(),
        }
    }
}

pub(crate) fn valid_reasoning_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        && value != "default"
}

impl ReasoningCapability {
    pub fn validate(&self, protocol: ProviderProtocol) -> Result<(), ProviderError> {
        let invalid = || ProviderError::InvalidConfig("invalid reasoning capability declaration");
        if self.options.len() > 32
            || (self.adapter == ReasoningAdapter::None) != self.options.is_empty()
            || (self.source == ReasoningSource::Unknown && self.adapter != ReasoningAdapter::None)
        {
            return Err(invalid());
        }
        let compatible = match self.adapter {
            ReasoningAdapter::None => true,
            ReasoningAdapter::Effort => protocol != ProviderProtocol::AnthropicMessages,
            ReasoningAdapter::Deepseek
            | ReasoningAdapter::ChatToggle
            | ReasoningAdapter::ThinkingToggle => protocol == ProviderProtocol::ChatCompletions,
            ReasoningAdapter::MessagesEffort
            | ReasoningAdapter::MessagesEffortOnly
            | ReasoningAdapter::AnthropicAdaptive
            | ReasoningAdapter::AnthropicBudget => protocol == ProviderProtocol::AnthropicMessages,
        };
        if !compatible {
            return Err(invalid());
        }
        let mut ids = HashSet::new();
        for option in &self.options {
            if !valid_reasoning_name(&option.id)
                || !ids.insert(option.id.as_str())
                || option
                    .effort
                    .as_deref()
                    .is_some_and(|value| !valid_reasoning_name(value))
                || option.label.as_ref().is_some_and(|value| {
                    value.trim().is_empty()
                        || value.chars().count() > 128
                        || value.chars().any(char::is_control)
                })
            {
                return Err(invalid());
            }
            let valid = match self.adapter {
                ReasoningAdapter::None => false,
                ReasoningAdapter::Effort | ReasoningAdapter::MessagesEffortOnly => {
                    option.effort.is_some()
                        && option.budget_tokens.is_none()
                        && option.enabled.is_none()
                }
                ReasoningAdapter::Deepseek
                | ReasoningAdapter::MessagesEffort
                | ReasoningAdapter::AnthropicAdaptive => {
                    option.budget_tokens.is_none()
                        && if option.enabled == Some(false) {
                            option.effort.is_none()
                        } else {
                            option.effort.is_some()
                        }
                }
                ReasoningAdapter::AnthropicBudget => {
                    option.effort.is_none()
                        && if option.enabled == Some(false) {
                            option.budget_tokens.is_none()
                        } else {
                            option
                                .budget_tokens
                                .is_some_and(|n| (1024..=8_000_000).contains(&n))
                        }
                }
                ReasoningAdapter::ChatToggle | ReasoningAdapter::ThinkingToggle => {
                    option.enabled.is_some()
                        && option.effort.is_none()
                        && option.budget_tokens.is_none()
                }
            };
            if !valid {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub fn available_options(
        &self,
        max_output_tokens: usize,
    ) -> impl Iterator<Item = &ReasoningOption> {
        self.options.iter().filter(move |option| {
            option
                .budget_tokens
                .is_none_or(|tokens| tokens < max_output_tokens)
        })
    }

    /// Candidate settings for a protocol, not a claim of model support. Hosts
    /// can replace them with provider metadata or a user-selected declaration.
    /// Installing a preset does not send any reasoning fields by default.
    pub fn preset(protocol: ProviderProtocol) -> Self {
        let budget = protocol == ProviderProtocol::AnthropicMessages;
        let mut options: Vec<_> = ["low", "medium", "high"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| ReasoningOption {
                id: if budget {
                    format!("budget_{}", [1024, 4096, 8192][index])
                } else {
                    id.into()
                },
                label: None,
                effort: (!budget).then(|| id.into()),
                budget_tokens: budget.then(|| [1024, 4096, 8192][index]),
                enabled: None,
            })
            .collect();
        if budget {
            options.insert(
                0,
                ReasoningOption {
                    id: "off".into(),
                    label: None,
                    effort: None,
                    budget_tokens: None,
                    enabled: Some(false),
                },
            );
        }
        Self {
            source: ReasoningSource::Preset,
            adapter: if budget {
                ReasoningAdapter::AnthropicBudget
            } else {
                ReasoningAdapter::Effort
            },
            options,
        }
    }

    /// Preserve historical, explicitly selected settings and their wire dialect.
    pub(crate) fn legacy(mode: &ReasoningMode) -> Self {
        let (adapter, efforts): (ReasoningAdapter, &[&str]) = match mode {
            ReasoningMode::Off => return Self::default(),
            ReasoningMode::Effort { .. } => (ReasoningAdapter::Effort, &["low", "medium", "high"]),
            ReasoningMode::DeepSeek { .. } => {
                (ReasoningAdapter::Deepseek, &["low", "medium", "high"])
            }
            ReasoningMode::AnthropicBudget { .. } => (
                ReasoningAdapter::AnthropicBudget,
                &["low", "medium", "high"],
            ),
            ReasoningMode::Configured { .. } => return Self::default(),
        };
        let mut options: Vec<_> = efforts
            .iter()
            .enumerate()
            .map(|(index, id)| ReasoningOption {
                id: (*id).into(),
                label: None,
                effort: (adapter != ReasoningAdapter::AnthropicBudget).then(|| (*id).into()),
                budget_tokens: (adapter == ReasoningAdapter::AnthropicBudget)
                    .then(|| [1024, 4096, 8192][index]),
                enabled: None,
            })
            .collect();
        if let ReasoningMode::Effort { effort } | ReasoningMode::DeepSeek { effort } = mode
            && !options.iter().any(|option| &option.id == effort)
        {
            options.push(ReasoningOption {
                id: effort.clone(),
                label: None,
                effort: Some(effort.clone()),
                budget_tokens: None,
                enabled: None,
            });
        }
        Self {
            source: ReasoningSource::Legacy,
            adapter,
            options,
        }
    }
}

impl HttpModel {
    pub fn reasoning_capability(&self) -> &ReasoningCapability {
        &self.reasoning_capability
    }

    /// Install one connection's capabilities, retaining the provider default until
    /// a selection is made. This does not issue a provider request.
    pub fn with_reasoning_capability(
        mut self,
        capability: ReasoningCapability,
    ) -> Result<Self, ProviderError> {
        capability.validate(self.protocol)?;
        self.reasoning_capability = capability;
        self.legacy_reasoning_profile = false;
        Ok(self)
    }

    pub fn supported_reasoning_efforts(&self) -> Vec<&str> {
        self.reasoning_capability
            .available_options(self.metadata.max_output_tokens.unwrap_or_default())
            .map(|option| option.id.as_str())
            .collect()
    }

    pub fn with_reasoning_effort(&self, effort: &str) -> Result<Self, ProviderError> {
        let option = self
            .reasoning_capability
            .available_options(self.metadata.max_output_tokens.unwrap_or_default())
            .find(|option| option.id == effort)
            .ok_or(ProviderError::InvalidConfig(
                "reasoning choice is undeclared or exceeds the configured output budget",
            ))?;
        let mut model = self.clone();
        // Preserve the previous replay identity when the wire settings are
        // unchanged. Labels and IDs are presentation data, not replay state.
        model.reasoning = match self.reasoning_capability.adapter {
            ReasoningAdapter::Effort => ReasoningMode::Effort {
                effort: option.effort.clone().expect("validated effort"),
            },
            ReasoningAdapter::Deepseek if option.enabled != Some(false) => {
                ReasoningMode::DeepSeek {
                    effort: option.effort.clone().expect("validated effort"),
                }
            }
            ReasoningAdapter::AnthropicBudget if option.enabled != Some(false) => {
                ReasoningMode::AnthropicBudget {
                    tokens: option.budget_tokens.expect("validated budget"),
                }
            }
            adapter => ReasoningMode::Configured {
                adapter,
                option: option.clone(),
            },
        };
        model.update_reasoning_identity();
        Ok(model)
    }

    /// Restore a pre-capability snapshot's already persisted selection. Callers
    /// must restrict this to versioned historical restoration, never new input.
    /// This does not add choices to the connection's advertised capabilities.
    pub fn restore_legacy_reasoning_effort(&self, effort: &str) -> Result<Self, ProviderError> {
        if !self.legacy_reasoning_profile || !matches!(effort, "low" | "medium" | "high") {
            return Err(ProviderError::InvalidConfig(
                "not a historical reasoning selection",
            ));
        }
        let mut model = self.clone();
        model.reasoning = match (&self.reasoning, self.protocol) {
            (_, ProviderProtocol::AnthropicMessages) => {
                let tokens = match effort {
                    "low" => 1024,
                    "medium" => 4096,
                    _ => 8192,
                };
                if tokens >= self.metadata.max_output_tokens.unwrap_or_default() {
                    return Err(ProviderError::InvalidConfig(
                        "historical reasoning budget exceeds output limit",
                    ));
                }
                ReasoningMode::AnthropicBudget { tokens }
            }
            (ReasoningMode::DeepSeek { .. }, _) => ReasoningMode::DeepSeek {
                effort: effort.into(),
            },
            _ => ReasoningMode::Effort {
                effort: effort.into(),
            },
        };
        model.update_reasoning_identity();
        Ok(model)
    }

    fn update_reasoning_identity(&mut self) {
        self.identity.format = format!(
            "{}.http.v1:{:?}",
            self.protocol.wire().as_str(),
            self.reasoning
        );
        self.metadata.accounting_identity = Some(self.identity.format.clone());
    }
}
