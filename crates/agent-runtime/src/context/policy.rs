use super::{ModelContextBudget, ModelContextBudgetError, budget::percentage};
use crate::{ModelContextMetadata, ModelContextMetadataError};
use std::{error::Error, fmt};

/// Budgets for the active model context. Compactors may supply their own budget.
///
/// A policy retains explicit overrides so `resolve` can derive fresh defaults
/// when the selected model changes, including after a previous resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPolicy {
    model_budget: ModelContextBudget,
    compaction_threshold: usize,
    compaction_target: usize,
    recent_tail_tokens: usize,
    max_tool_output_chars: usize,
    overrides: PolicyOverrides,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PolicyOverrides {
    context_window: Option<usize>,
    output_token_reserve: Option<usize>,
    safety_margin: Option<usize>,
    effective_context_window_percent: Option<u8>,
    compaction_threshold: Option<usize>,
    compaction_target: Option<usize>,
    recent_tail_tokens: Option<usize>,
    recent_tail_percent: Option<u8>,
    preserve_all_user_messages: bool,
    estimation_margin_percent: Option<u8>,
}

impl ContextPolicy {
    pub fn builder() -> ContextPolicyBuilder {
        ContextPolicyBuilder::default()
    }

    /// Resolve model capabilities and caller overrides into usable budgets.
    ///
    /// The advertised default window is used unless explicitly overridden. A
    /// window override is capped by the model's advertised maximum. Automatic
    /// triggers use at most 90% of that window and the hard input limit. Explicit
    /// triggers retain validation rather than being silently rewritten.
    pub fn resolve(
        &self,
        metadata: Option<&ModelContextMetadata>,
    ) -> Result<Self, ContextPolicyError> {
        resolve_policy(self.overrides, self.max_tool_output_chars, metadata)
    }

    pub fn model_budget(self) -> ModelContextBudget {
        self.model_budget
    }

    pub fn input_token_limit(self) -> usize {
        self.model_budget.input_token_limit()
    }

    pub fn output_token_limit(self) -> Option<usize> {
        self.model_budget.output_token_limit()
    }

    pub fn compaction_threshold(self) -> usize {
        self.compaction_threshold
    }

    /// Wait for a useful amount of growth above irreducible inputs or the last
    /// accepted checkpoint. Neither floor can postpone hard-limit recovery.
    pub(crate) fn effective_compaction_threshold(
        self,
        protected_tokens: usize,
        previous_compacted_tokens: Option<usize>,
    ) -> usize {
        let growth_budget = self.compaction_threshold - self.compaction_target;
        self.compaction_threshold
            .max(protected_tokens.saturating_add(growth_budget))
            .max(previous_compacted_tokens.map_or(0, |tokens| tokens.saturating_add(growth_budget)))
            .min(self.input_token_limit())
    }

    /// Desired size after compaction, below the trigger to avoid immediate churn.
    pub fn compaction_target(self) -> usize {
        self.compaction_target
    }

    pub fn recent_tail_tokens(self) -> usize {
        self.recent_tail_tokens
    }

    /// Recent history uses 16% of the input space remaining after instructions
    /// and tool schemas. An explicit token budget takes precedence.
    pub fn recent_tail_tokens_for_message_budget(self, available_tokens: usize) -> usize {
        self.overrides.recent_tail_tokens.unwrap_or_else(|| {
            percentage(
                available_tokens,
                self.overrides.recent_tail_percent.unwrap_or(16),
            )
            .max(1)
            .min(self.compaction_threshold - 1)
        })
    }

    pub fn preserve_all_user_messages(self) -> bool {
        self.overrides.preserve_all_user_messages
    }

    pub fn max_tool_output_chars(self) -> usize {
        self.max_tool_output_chars
    }

    /// Extra headroom applied only to input that has not been measured by the
    /// provider. Independent of the fixed model-window safety reservation.
    pub fn estimation_margin_percent(self) -> u8 {
        self.overrides.estimation_margin_percent.unwrap_or(15)
    }
}

impl Default for ContextPolicy {
    fn default() -> Self {
        ContextPolicyBuilder::default()
            .build()
            .expect("default context policy is valid")
    }
}

/// Named context budgets, validated together when `build` is called.
#[derive(Debug, Clone, Copy)]
pub struct ContextPolicyBuilder {
    overrides: PolicyOverrides,
    max_tool_output_chars: usize,
}

impl Default for ContextPolicyBuilder {
    fn default() -> Self {
        Self {
            overrides: PolicyOverrides::default(),
            max_tool_output_chars: 32_000,
        }
    }
}

impl ContextPolicyBuilder {
    /// Sets a legacy input-only window, clearing output and safety reservations.
    /// Without model metadata this remains an exact input limit. A known model
    /// still applies its effective-window percentage and maximum window.
    /// Prefer `model_budget` when the provider's complete window is known.
    pub fn input_token_limit(mut self, tokens: usize) -> Self {
        self.overrides.context_window = Some(tokens);
        self.overrides.output_token_reserve = Some(0);
        self.overrides.safety_margin = Some(0);
        self.overrides.effective_context_window_percent = Some(100);
        self
    }

    pub fn model_budget(mut self, budget: ModelContextBudget) -> Self {
        self.overrides.context_window = Some(budget.context_window());
        self.overrides.output_token_reserve = Some(budget.output_token_reserve());
        self.overrides.safety_margin = Some(budget.safety_margin());
        self.overrides.effective_context_window_percent =
            Some(budget.effective_context_window_percent());
        self
    }

    pub fn model_context_window(mut self, tokens: usize) -> Self {
        self.overrides.context_window = Some(tokens);
        self
    }

    /// Reserves output capacity and caps generation at this amount. A known
    /// model's smaller output maximum further reduces this value at resolution.
    pub fn output_token_reserve(mut self, tokens: usize) -> Self {
        self.overrides.output_token_reserve = Some(tokens);
        self
    }

    pub fn safety_margin(mut self, tokens: usize) -> Self {
        self.overrides.safety_margin = Some(tokens);
        self
    }

    /// Percentage (0..=100) added to the unmeasured input portion. Defaults to
    /// 15. Zero disables this additional estimation margin.
    pub fn estimation_margin_percent(mut self, percent: u8) -> Self {
        self.overrides.estimation_margin_percent = Some(percent);
        self
    }

    pub fn compaction_threshold(mut self, tokens: usize) -> Self {
        self.overrides.compaction_threshold = Some(tokens);
        self
    }

    pub fn compaction_target(mut self, tokens: usize) -> Self {
        self.overrides.compaction_target = Some(tokens);
        self
    }

    pub fn recent_tail_tokens(mut self, tokens: usize) -> Self {
        self.overrides.recent_tail_tokens = Some(tokens);
        self
    }

    /// Percentage of available message space retained verbatim (1..=99).
    /// Explicit `recent_tail_tokens` overrides this ratio.
    pub fn recent_tail_percent(mut self, percent: u8) -> Self {
        self.overrides.recent_tail_percent = Some(percent);
        self
    }

    /// Keep every historical user message verbatim. By default only the latest
    /// user input is protected; older inputs remain in the canonical journal.
    pub fn preserve_all_user_messages(mut self, preserve: bool) -> Self {
        self.overrides.preserve_all_user_messages = preserve;
        self
    }

    pub fn max_tool_output_chars(mut self, chars: usize) -> Self {
        self.max_tool_output_chars = chars;
        self
    }

    /// Build against the conservative fallback when model metadata is absent.
    /// Context managers resolve this policy again for the selected model.
    pub fn build(self) -> Result<ContextPolicy, ContextPolicyError> {
        resolve_policy(self.overrides, self.max_tool_output_chars, None)
    }

    /// Build with model capabilities, including overrides larger than the
    /// fallback window that cannot be validated until the model is known.
    pub fn build_for_model(
        self,
        metadata: &ModelContextMetadata,
    ) -> Result<ContextPolicy, ContextPolicyError> {
        resolve_policy(self.overrides, self.max_tool_output_chars, Some(metadata))
    }
}

fn resolve_policy(
    overrides: PolicyOverrides,
    max_tool_output_chars: usize,
    metadata: Option<&ModelContextMetadata>,
) -> Result<ContextPolicy, ContextPolicyError> {
    if let Some(percent) = overrides.recent_tail_percent
        && !(1..=99).contains(&percent)
    {
        return Err(ContextPolicyError::InvalidRecentTailPercent(percent));
    }
    if overrides
        .estimation_margin_percent
        .is_some_and(|percent| percent > 100)
    {
        return Err(ContextPolicyError::InvalidEstimationMarginPercent(
            overrides.estimation_margin_percent.unwrap(),
        ));
    }
    if let Some(metadata) = metadata {
        metadata
            .validate()
            .map_err(ContextPolicyError::ModelMetadata)?;
    }
    let fallback = ModelContextBudget::default();
    let context_window = overrides.context_window.unwrap_or_else(|| {
        metadata.map_or(fallback.context_window(), |model| model.context_window)
    });
    let context_window = metadata.map_or(context_window, |model| {
        context_window.min(model.maximum_context_window())
    });
    if context_window == 0 {
        return Err(ContextPolicyError::ZeroInputTokenLimit);
    }
    let output_token_reserve = overrides
        .output_token_reserve
        .unwrap_or(fallback.output_token_reserve());
    let output_token_reserve = metadata
        .and_then(|model| model.max_output_tokens)
        .map_or(output_token_reserve, |maximum| {
            output_token_reserve.min(maximum)
        });
    let safety_margin = overrides.safety_margin.unwrap_or(fallback.safety_margin());
    let effective_percent = metadata.map_or_else(
        || overrides.effective_context_window_percent.unwrap_or(95),
        |model| {
            model
                .effective_context_window_percent
                .min(overrides.effective_context_window_percent.unwrap_or(100))
        },
    );
    let model_budget = ModelContextBudget::new(context_window, output_token_reserve, safety_margin)
        .and_then(|budget| budget.with_effective_context_window_percent(effective_percent))
        .map_err(ContextPolicyError::ModelBudget)?;
    let automatic_threshold = percentage(context_window, 90);
    let compaction_threshold = overrides.compaction_threshold.unwrap_or_else(|| {
        metadata
            .and_then(|model| model.auto_compact_token_limit)
            .map_or(automatic_threshold, |limit| limit.min(automatic_threshold))
            .min(model_budget.input_token_limit())
    });
    if compaction_threshold == 0 {
        return Err(ContextPolicyError::ZeroCompactionThreshold);
    }
    if compaction_threshold > model_budget.input_token_limit() {
        return Err(ContextPolicyError::ThresholdExceedsInputLimit {
            threshold: compaction_threshold,
            input_token_limit: model_budget.input_token_limit(),
        });
    }
    let recent_tail_tokens = overrides.recent_tail_tokens.unwrap_or_else(|| {
        percentage(
            model_budget.input_token_limit(),
            overrides.recent_tail_percent.unwrap_or(16),
        )
        .max(1)
        .min(compaction_threshold.saturating_sub(1))
    });
    if recent_tail_tokens == 0 {
        return Err(ContextPolicyError::ZeroRecentTailTokens);
    }
    if recent_tail_tokens >= compaction_threshold {
        return Err(ContextPolicyError::TailNotBelowThreshold {
            recent_tail_tokens,
            compaction_threshold,
        });
    }
    let compaction_target = overrides.compaction_target.unwrap_or_else(|| {
        (compaction_threshold - compaction_threshold / 4)
            .max(recent_tail_tokens)
            .min(compaction_threshold - 1)
    });
    if compaction_target == 0 || compaction_target >= compaction_threshold {
        return Err(ContextPolicyError::InvalidCompactionTarget {
            target: compaction_target,
            threshold: compaction_threshold,
        });
    }
    if max_tool_output_chars == 0 {
        return Err(ContextPolicyError::ZeroMaxToolOutputChars);
    }
    Ok(ContextPolicy {
        model_budget,
        compaction_threshold,
        compaction_target,
        recent_tail_tokens,
        max_tool_output_chars,
        overrides,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextPolicyError {
    InvalidRecentTailPercent(u8),
    InvalidEstimationMarginPercent(u8),
    ModelMetadata(ModelContextMetadataError),
    ModelBudget(ModelContextBudgetError),
    ZeroInputTokenLimit,
    ZeroCompactionThreshold,
    ThresholdExceedsInputLimit {
        threshold: usize,
        input_token_limit: usize,
    },
    InvalidCompactionTarget {
        target: usize,
        threshold: usize,
    },
    ZeroRecentTailTokens,
    TailNotBelowThreshold {
        recent_tail_tokens: usize,
        compaction_threshold: usize,
    },
    ZeroMaxToolOutputChars,
}

impl fmt::Display for ContextPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRecentTailPercent(percent) => write!(
                formatter,
                "recent-tail percentage ({percent}) must be between 1 and 99"
            ),
            Self::InvalidEstimationMarginPercent(percent) => write!(
                formatter,
                "input estimation margin ({percent}) must be between 0 and 100"
            ),
            Self::ModelMetadata(error) => error.fmt(formatter),
            Self::ModelBudget(error) => error.fmt(formatter),
            Self::ZeroInputTokenLimit => {
                write!(formatter, "input token limit must be greater than zero")
            }
            Self::ZeroCompactionThreshold => {
                write!(formatter, "compaction threshold must be greater than zero")
            }
            Self::ThresholdExceedsInputLimit {
                threshold,
                input_token_limit,
            } => write!(
                formatter,
                "compaction threshold ({threshold}) exceeds input token limit ({input_token_limit})"
            ),
            Self::InvalidCompactionTarget { target, threshold } => write!(
                formatter,
                "compaction target ({target}) must be positive and below threshold ({threshold})"
            ),
            Self::ZeroRecentTailTokens => {
                write!(
                    formatter,
                    "recent-tail token budget must be greater than zero"
                )
            }
            Self::TailNotBelowThreshold {
                recent_tail_tokens,
                compaction_threshold,
            } => write!(
                formatter,
                "recent-tail token budget ({recent_tail_tokens}) must be below compaction threshold ({compaction_threshold})"
            ),
            Self::ZeroMaxToolOutputChars => {
                write!(
                    formatter,
                    "maximum tool-output characters must be greater than zero"
                )
            }
        }
    }
}

impl Error for ContextPolicyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ModelMetadata(error) => Some(error),
            Self::ModelBudget(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ContextPolicy;

    #[test]
    fn effective_trigger_preserves_growth_room_without_bypassing_the_hard_limit() {
        let policy = ContextPolicy::builder()
            .input_token_limit(1_000)
            .compaction_threshold(600)
            .compaction_target(400)
            .recent_tail_tokens(100)
            .build()
            .unwrap();
        assert_eq!(policy.effective_compaction_threshold(100, None), 600);
        assert_eq!(policy.effective_compaction_threshold(700, None), 900);
        assert_eq!(policy.effective_compaction_threshold(100, Some(650)), 850);
        assert_eq!(
            policy.effective_compaction_threshold(usize::MAX, Some(usize::MAX)),
            1_000
        );
    }
}
