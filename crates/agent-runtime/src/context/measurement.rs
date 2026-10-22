//! A shared snapshot of measured input, projected input, and decision headroom.
use super::TokenCounter;
use crate::{ModelContextMetadata, ModelRequest};
use agent_core::token_usage::RequestUsageBaseline;
use serde::{Deserialize, Serialize};

/// Extra headroom applied only to input whose cost has not been measured.
pub const DEFAULT_ESTIMATION_MARGIN_PERCENT: u8 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMeasurementSource {
    /// No matching provider usage; the complete visible input is estimated.
    Estimated,
    /// Provider usage calibrates an exact prefix or a compatible rewritten
    /// projection. Only the exact-prefix path has measured input in this request.
    ProviderCalibrated,
    /// The provider supplied the size of a native context component. Visible
    /// instructions, tools and appended messages are estimated separately.
    ProviderNative,
    /// Legacy source retained for reading earlier diagnostic records. Current
    /// context managers do not make provider counting requests.
    ProviderCounted,
    /// Legacy source retained for reading earlier diagnostic records. Current
    /// context managers do not load model tokenizers.
    TokenizerEstimated,
}

/// Input accounting for one immutable request projection.
///
/// These are context occupancy values, not cumulative billing. Output capacity
/// is reserved by `ContextPolicy` separately. `measured_input_tokens` measures
/// the matching prefix (or native component), not necessarily the whole request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMeasurement {
    /// The counter's uncalibrated estimate for the complete request.
    pub raw_estimated_input_tokens: usize,
    /// Prediction after applying compatible provider usage to the local estimate.
    pub estimated_input_tokens: usize,
    /// Actual usage for a retained matching prefix, or declared native component.
    pub measured_input_tokens: Option<usize>,
    /// Input outside the measured component that still relies on local estimation.
    pub estimated_delta_tokens: usize,
    pub estimation_margin_tokens: usize,
    /// Predicted input plus estimation margin; used for context budget decisions.
    pub decision_input_tokens: usize,
    pub source: ContextMeasurementSource,
}

impl ContextMeasurement {
    pub fn estimated(tokens: usize) -> Self {
        Self {
            raw_estimated_input_tokens: tokens,
            estimated_input_tokens: tokens,
            measured_input_tokens: None,
            estimated_delta_tokens: tokens,
            estimation_margin_tokens: 0,
            decision_input_tokens: tokens,
            source: ContextMeasurementSource::Estimated,
        }
        .with_margin_percent(DEFAULT_ESTIMATION_MARGIN_PERCENT)
    }

    pub fn provider_native(estimated: usize, native_tokens: usize) -> Self {
        Self {
            raw_estimated_input_tokens: estimated,
            estimated_input_tokens: estimated,
            measured_input_tokens: Some(native_tokens),
            estimated_delta_tokens: estimated.saturating_sub(native_tokens),
            estimation_margin_tokens: 0,
            decision_input_tokens: estimated,
            source: ContextMeasurementSource::ProviderNative,
        }
        .with_margin_percent(DEFAULT_ESTIMATION_MARGIN_PERCENT)
    }

    pub(super) fn calibrated(raw: usize, measured: usize, delta: usize) -> Self {
        let estimated = measured.saturating_add(delta);
        Self {
            raw_estimated_input_tokens: raw,
            estimated_input_tokens: estimated,
            measured_input_tokens: Some(measured),
            estimated_delta_tokens: delta,
            estimation_margin_tokens: 0,
            decision_input_tokens: estimated,
            source: ContextMeasurementSource::ProviderCalibrated,
        }
        .with_margin_percent(DEFAULT_ESTIMATION_MARGIN_PERCENT)
    }

    /// Retains an additive calibration across a history rewrite. A negative
    /// delta may reduce the prediction, but never below the raw local estimate.
    /// No part of the rewritten input is claimed as a measured prefix, so all of
    /// the prediction receives uncertainty headroom.
    pub(super) fn recalibrated(raw: usize, measured: usize, anchor_raw: usize) -> Self {
        let adjusted = if raw >= anchor_raw {
            measured.saturating_add(raw - anchor_raw)
        } else {
            measured.saturating_sub(anchor_raw - raw)
        }
        .max(raw);
        Self {
            raw_estimated_input_tokens: raw,
            estimated_input_tokens: adjusted,
            measured_input_tokens: None,
            estimated_delta_tokens: adjusted,
            estimation_margin_tokens: 0,
            decision_input_tokens: adjusted,
            source: ContextMeasurementSource::ProviderCalibrated,
        }
        .with_margin_percent(DEFAULT_ESTIMATION_MARGIN_PERCENT)
    }

    /// Recomputes headroom without compounding any previously applied margin.
    /// Zero disables the margin. Arithmetic saturates for oversized input.
    pub fn with_margin_percent(mut self, percent: u8) -> Self {
        let percent = usize::from(percent);
        self.estimation_margin_tokens = (self.estimated_delta_tokens / 100)
            .saturating_mul(percent)
            .saturating_add(((self.estimated_delta_tokens % 100) * percent).div_ceil(100));
        self.decision_input_tokens = self
            .estimated_input_tokens
            .saturating_add(self.estimation_margin_tokens);
        self
    }

    pub(super) fn for_request(request: &ModelRequest, counter: &dyn TokenCounter) -> Self {
        let estimated = counter.count_tokens(request);
        match &request.provider_context {
            Some(window) => Self::provider_native(estimated, window.input_tokens),
            None => Self::estimated(estimated),
        }
    }
}

/// Binds usage to a stable request prefix without persisting another prompt copy.
/// Native windows have their own provider-supplied size and are not calibrated.
pub fn request_usage_baseline(
    metadata: Option<&ModelContextMetadata>,
    request: &ModelRequest,
) -> Option<RequestUsageBaseline> {
    let metadata = metadata?;
    if request.provider_context.is_some() || !metadata.usage_is_input_measurement {
        return None;
    }
    Some(RequestUsageBaseline {
        model_key: model_key(metadata)?,
        fixed_fingerprint: request.replay_fixed_fingerprint().ok()?,
        prefix_fingerprint: request
            .replay_prefix_fingerprint(request.messages.len())
            .ok()?,
        message_count: request.messages.len(),
        raw_estimated_input_tokens: None,
        estimator_identity: None,
    })
}

/// Adds a durable estimate anchor only when the counter opts into a stable
/// algorithm/configuration identity. The provider usage is still recorded once.
pub fn request_usage_baseline_with_counter(
    metadata: Option<&ModelContextMetadata>,
    request: &ModelRequest,
    counter: &dyn TokenCounter,
) -> Option<RequestUsageBaseline> {
    let mut baseline = request_usage_baseline(metadata, request)?;
    if let Some(identity) = counter.estimator_identity().filter(|id| !id.is_empty()) {
        baseline.raw_estimated_input_tokens = Some(counter.count_tokens(request));
        baseline.estimator_identity = Some(identity);
    }
    Some(baseline)
}

pub(super) fn model_key(metadata: &ModelContextMetadata) -> Option<String> {
    // Version the tuple so future identity/serialization changes invalidate old
    // baselines instead of silently applying usage from another request format.
    serde_json::to_string(&(
        "context-usage-v1",
        &metadata.provider,
        &metadata.model,
        &metadata.accounting_identity,
        metadata.usage_is_input_measurement,
        metadata.context_window,
        metadata.max_context_window,
        metadata.auto_compact_token_limit,
        metadata.effective_context_window_percent,
        metadata.max_output_tokens,
    ))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_measurement_adds_headroom_only_to_visible_input() {
        let measured = ContextMeasurement::provider_native(1_100, 1_000);
        assert_eq!(measured.measured_input_tokens, Some(1_000));
        assert_eq!(measured.estimated_delta_tokens, 100);
        assert_eq!(measured.decision_input_tokens, 1_115);
    }

    #[test]
    fn matching_usage_has_no_estimation_margin() {
        let measured = ContextMeasurement::calibrated(100, 240, 0).with_margin_percent(30);
        assert_eq!(measured.raw_estimated_input_tokens, 100);
        assert_eq!(measured.measured_input_tokens, Some(240));
        assert_eq!(measured.estimated_delta_tokens, 0);
        assert_eq!(measured.decision_input_tokens, 240);
    }

    #[test]
    fn legacy_count_sources_remain_readable() {
        for (encoded, source) in [
            (
                "\"provider_counted\"",
                ContextMeasurementSource::ProviderCounted,
            ),
            (
                "\"tokenizer_estimated\"",
                ContextMeasurementSource::TokenizerEstimated,
            ),
        ] {
            assert_eq!(
                serde_json::from_str::<ContextMeasurementSource>(encoded).unwrap(),
                source,
            );
        }
    }
}
