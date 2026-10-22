//! Per-run calibration against durable provider-reported input usage.
use super::TokenCounter;
use super::measurement::{ContextMeasurement, model_key, request_usage_baseline_with_counter};
use crate::{ModelContextMetadata, ModelOutput, ModelRequest};
use agent_core::{
    agent::{RunEvent, RunEventEnvelope},
    token_usage::RequestUsageBaseline,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const MAX_TRACKED_RUNS: usize = 128;

/// A bounded cache of accounting hints reconstructed from the durable log.
///
/// Exact-prefix measurements retain their measured status. History rewrites can
/// retain a conservative estimate correction when the model, fixed header and
/// versioned local counter are unchanged; their complete input stays estimated.
/// Generated output can contain discarded reasoning, so only retained output is
/// estimated as an extension. No conversation text is retained by this cache.
#[derive(Debug, Clone, Default)]
pub(super) struct ContextUsageTracker {
    cache: Arc<Mutex<UsageCache>>,
}

#[derive(Debug, Default)]
struct UsageCache {
    runs: HashMap<String, RunUsage>,
    clock: u64,
}

#[derive(Debug, Default)]
struct RunUsage {
    pending: Option<PendingRequest>,
    confirmed: Option<UsageBaseline>,
    restored_sequence: Option<u64>,
    last_used: u64,
}

#[derive(Debug)]
struct PendingRequest {
    request: RequestUsageBaseline,
    has_input: bool,
}

#[derive(Debug, Clone)]
struct UsageBaseline {
    request: RequestUsageBaseline,
    tokens: usize,
}

impl UsageCache {
    fn run_mut(&mut self, run_id: &str) -> &mut RunUsage {
        self.clock = self.clock.saturating_add(1);
        if !self.runs.contains_key(run_id) && self.runs.len() >= MAX_TRACKED_RUNS {
            let oldest = self
                .runs
                .iter()
                .min_by_key(|(_, usage)| usage.last_used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.runs.remove(&oldest);
            }
        }
        let usage = self.runs.entry(run_id.to_owned()).or_default();
        usage.last_used = self.clock;
        usage
    }
}

impl ContextUsageTracker {
    pub(super) fn record_request(
        &self,
        run_id: &str,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
        counter: &dyn TokenCounter,
    ) {
        let Some(binding) = request_usage_baseline_with_counter(metadata, request, counter) else {
            self.forget(run_id);
            return;
        };
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        let usage = cache.run_mut(run_id);
        if usage.confirmed.as_ref().is_some_and(|baseline| {
            !baseline.matches(&binding.model_key, request)
                && !baseline.can_recalibrate(&binding.model_key, request, counter)
        }) {
            usage.confirmed = None;
        }
        usage.pending = Some(PendingRequest {
            request: binding,
            has_input: has_input(request, request.messages.len()),
        });
    }

    pub(super) fn observe_response(
        &self,
        run_id: &str,
        metadata: Option<&ModelContextMetadata>,
        output: &ModelOutput,
    ) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        let key = metadata
            .filter(|metadata| metadata.usage_is_input_measurement)
            .and_then(model_key);
        if key.is_none() || output.provider_context.is_some() || output.context_compacted {
            cache.runs.remove(run_id);
            return;
        }
        let Some(usage) = cache.runs.get_mut(run_id) else {
            return;
        };
        let Some(pending) = usage.pending.take() else {
            return;
        };
        if Some(&pending.request.model_key) != key.as_ref() {
            cache.runs.remove(run_id);
            return;
        }
        let Some(actual) = output.usage else {
            // A missing sample leaves a compatible previous anchor intact. The
            // response is counted only if retained in the next request.
            return;
        };
        if pending.has_input && actual.input_tokens == 0 {
            usage.confirmed = None;
            return;
        }
        // Cached input is a subset of input_tokens, never an additional cost.
        usage.confirmed = Some(UsageBaseline {
            request: pending.request,
            tokens: actual.input_tokens,
        });
    }

    /// Rehydrates the latest durable sample once per log revision. Missing
    /// samples in legacy/new responses preserve an earlier compatible sample;
    /// an explicitly unbound sample or native continuation invalidates it.
    /// Does not consume the pending request installed by `record_request`.
    pub(super) fn restore(
        &self,
        run_id: &str,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
        events: &[RunEventEnvelope],
    ) {
        let Some(metadata) = metadata else {
            return;
        };
        if request.provider_context.is_some() || !metadata.usage_is_input_measurement {
            // Recovery and compaction probe alternative projections. Inspecting
            // a native/unmeasurable candidate must not erase a usable anchor for
            // the original request if that candidate is rejected.
            return;
        }
        let Some(latest_sequence) = events.iter().rev().find_map(|event| {
            matches!(event.event, RunEvent::ModelResponseReceived { .. }).then_some(event.sequence)
        }) else {
            return;
        };
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        let usage = cache.run_mut(run_id);
        if usage.restored_sequence == Some(latest_sequence) {
            return;
        }
        usage.restored_sequence = Some(latest_sequence);
        let restored = events.iter().rev().find_map(|event| match &event.event {
            RunEvent::ModelResponseReceived {
                token_usage,
                provider_continuation,
                ..
            } => {
                if provider_continuation.is_some() {
                    return Some(None);
                }
                let record = token_usage.as_ref()?;
                let Some(baseline) = record.baseline.as_ref() else {
                    return Some(None);
                };
                // Once an explicit record exists, do not reach past an invalid
                // sample and resurrect an older anchor from another context.
                if record.usage.input_tokens == 0 && has_input(request, baseline.message_count) {
                    return Some(None);
                }
                Some(Some(UsageBaseline {
                    request: baseline.clone(),
                    tokens: record.usage.input_tokens,
                }))
            }
            _ => None,
        });
        // Legacy logs contain no sample at all. They must not erase an active
        // observation supplied through the ContextManager API. Explicitly
        // unbound records remain distinguishable and do invalidate the anchor.
        if let Some(restored) = restored {
            usage.confirmed = restored;
        }
    }

    pub(super) fn measurement(
        &self,
        run_id: &str,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
        counter: &dyn TokenCounter,
    ) -> ContextMeasurement {
        let fallback = ContextMeasurement::for_request(request, counter);
        let Some(key) = metadata
            .filter(|metadata| metadata.usage_is_input_measurement)
            .and_then(model_key)
        else {
            return fallback;
        };
        if request.provider_context.is_some() {
            return fallback;
        }
        let baseline = {
            let Ok(mut cache) = self.cache.lock() else {
                return fallback;
            };
            if !cache.runs.contains_key(run_id) {
                return fallback;
            }
            cache.run_mut(run_id).confirmed.clone()
        };
        let Some(baseline) = baseline else {
            return fallback;
        };
        if baseline.matches(&key, request) {
            // Subtract a prefix counted with the same framing to avoid charging
            // request-level overhead a second time for the extension.
            let prefix = counter.count_prefix_tokens(request, baseline.request.message_count);
            let Some(extension) = fallback.raw_estimated_input_tokens.checked_sub(prefix) else {
                return fallback;
            };
            return ContextMeasurement::calibrated(
                fallback.raw_estimated_input_tokens,
                baseline.tokens,
                extension,
            );
        }
        if baseline.can_recalibrate(&key, request, counter) {
            return ContextMeasurement::recalibrated(
                fallback.raw_estimated_input_tokens,
                baseline.tokens,
                baseline
                    .request
                    .raw_estimated_input_tokens
                    .expect("compatible estimate anchor"),
            );
        }
        fallback
    }

    #[cfg(test)]
    pub(super) fn estimate(
        &self,
        run_id: &str,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
        counter: &dyn TokenCounter,
    ) -> usize {
        self.measurement(run_id, metadata, request, counter)
            .estimated_input_tokens
    }

    pub(super) fn forget(&self, run_id: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.runs.remove(run_id);
        }
    }
}

impl UsageBaseline {
    fn matches_fixed(&self, model_key: &str, request: &ModelRequest) -> bool {
        self.request.model_key == model_key
            && request.replay_fixed_fingerprint().ok().as_ref()
                == Some(&self.request.fixed_fingerprint)
    }

    fn can_recalibrate(
        &self,
        model_key: &str,
        request: &ModelRequest,
        counter: &dyn TokenCounter,
    ) -> bool {
        self.matches_fixed(model_key, request)
            && self.request.raw_estimated_input_tokens.is_some()
            && self.request.estimator_identity.is_some()
            && self.request.estimator_identity == counter.estimator_identity()
            // Changed opaque replay state cannot be treated as a local rewrite.
            && request.messages.iter().all(|message| message.provider_state.is_none())
    }

    fn matches(&self, model_key: &str, request: &ModelRequest) -> bool {
        self.matches_fixed(model_key, request)
            && request.messages.len() >= self.request.message_count
            && request
                .replay_prefix_fingerprint(self.request.message_count)
                .ok()
                .as_ref()
                == Some(&self.request.prefix_fingerprint)
    }
}

fn has_input(request: &ModelRequest, message_count: usize) -> bool {
    !request.instructions.is_empty() || !request.tools.is_empty() || message_count != 0
}

#[cfg(test)]
mod tests;
