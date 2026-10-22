//! Provider-independent usage and the request to which an observation belongs.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Complete reported input, including cache reads and writes. Adapters
    /// normalize providers with separate cache buckets before constructing this.
    pub input_tokens: usize,
    /// Complete generated output, including reasoning when reported. Reasoning
    /// must not be added again if it is already included in the provider total.
    pub output_tokens: usize,
    /// Cache reads are a subset of input_tokens and still occupy context.
    pub cached_input_tokens: Option<usize>,
}

impl ModelUsage {
    pub fn total_tokens(self) -> usize {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// Versioned, deterministic request identity; no prompt text is duplicated here.
/// Exact-prefix measurements and optional estimates are bound to one model and
/// fixed request. Estimate anchors also identify their local counter configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUsageBaseline {
    pub model_key: String,
    pub fixed_fingerprint: String,
    pub prefix_fingerprint: String,
    pub message_count: usize,
    /// Unmargined estimate of the measured request, used only with an identical
    /// estimator identity when history is rewritten. Legacy baselines omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_estimated_input_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimator_identity: Option<String>,
}

/// Stored atomically with the successful response. A missing baseline retains
/// spending information without treating it as a measurement of future inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsageRecord {
    pub usage: ModelUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<RequestUsageBaseline>,
}
