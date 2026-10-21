//! Per-run calibration against the latest provider-reported input usage.
use super::TokenCounter;
use crate::{ModelContextMetadata, ModelMessage, ModelOutput, ModelRequest};
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

const MAX_TRACKED_RUNS: usize = 128;

/// Keeps bounded, disposable token-accounting hints, never conversation text.
///
/// A baseline only applies when the next request contains exactly the previous
/// request as a prefix, with identical model metadata, instructions and tool
/// schemas. Compaction, truncation or changed projection of that input prefix
/// invalidate it.
/// Missing model metadata disables calibration because model identity is unknown.
/// Responses are estimated when replayed: generated output can include reasoning
/// that the provider does not receive in subsequent inputs.
#[derive(Debug, Clone, Default)]
pub(super) struct ContextUsageTracker {
    cache: Arc<Mutex<UsageCache>>,
}

#[derive(Debug, Default)]
struct UsageCache {
    runs: HashMap<String, RunUsage>,
    clock: u64,
}

#[derive(Debug)]
struct RunUsage {
    metadata: Option<ModelContextMetadata>,
    fixed_hash: u64,
    pending: Option<PendingRequest>,
    confirmed: Option<UsageBaseline>,
    last_used: u64,
}

#[derive(Debug)]
struct PendingRequest {
    messages_hash: u64,
    message_count: usize,
    has_input: bool,
}

#[derive(Debug, Clone)]
struct UsageBaseline {
    prefix_hash: u64,
    message_count: usize,
    tokens: usize,
}

impl ContextUsageTracker {
    pub(super) fn record_request(
        &self,
        run_id: &str,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
    ) {
        if metadata.is_none() || request.provider_context.is_some() {
            self.forget(run_id);
            return;
        }
        let fixed_hash = fixed_hash(request);
        let pending = PendingRequest {
            messages_hash: hash_messages(&request.messages).finish(),
            message_count: request.messages.len(),
            has_input: !request.instructions.is_empty()
                || !request.messages.is_empty()
                || !request.tools.is_empty(),
        };
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        cache.clock = cache.clock.saturating_add(1);
        let last_used = cache.clock;
        if !cache.runs.contains_key(run_id) && cache.runs.len() >= MAX_TRACKED_RUNS {
            let oldest = cache
                .runs
                .iter()
                .min_by_key(|(_, usage)| usage.last_used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                cache.runs.remove(&oldest);
            }
        }
        let usage = cache
            .runs
            .entry(run_id.to_owned())
            .or_insert_with(|| RunUsage {
                metadata: metadata.cloned(),
                fixed_hash,
                pending: None,
                confirmed: None,
                last_used,
            });
        let compatible = usage.metadata.as_ref() == metadata
            && usage.fixed_hash == fixed_hash
            && usage
                .confirmed
                .as_ref()
                .is_none_or(|baseline| baseline.matches(request));
        if !compatible {
            usage.confirmed = None;
        }
        usage.metadata = metadata.cloned();
        usage.fixed_hash = fixed_hash;
        usage.pending = Some(pending);
        usage.last_used = last_used;
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
        if metadata.is_none() || output.provider_context.is_some() || output.context_compacted {
            cache.runs.remove(run_id);
            return;
        }
        let Some(usage) = cache.runs.get_mut(run_id) else {
            return;
        };
        if usage.metadata.as_ref() != metadata {
            cache.runs.remove(run_id);
            return;
        }
        let Some(pending) = usage.pending.take() else {
            return;
        };
        let Some(actual) = output.usage else {
            // The earlier baseline can still account for this response as an
            // estimated extension. Never attach usage to an unobserved request.
            return;
        };
        if pending.has_input && actual.input_tokens == 0 {
            usage.confirmed = None;
            return;
        }
        // Cached input is a subset of input_tokens, not a second token cost.
        // Output usage can include discarded reasoning, so only the observed
        // request input is exact. Its replayed response is an estimated extension.
        usage.confirmed = Some(UsageBaseline {
            prefix_hash: pending.messages_hash,
            message_count: pending.message_count,
            tokens: actual.input_tokens,
        });
    }

    pub(super) fn estimate(
        &self,
        run_id: &str,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
        counter: &dyn TokenCounter,
    ) -> usize {
        let estimated = counter.count_tokens(request);
        if metadata.is_none() || request.provider_context.is_some() {
            return estimated;
        }
        let baseline = {
            let Ok(mut cache) = self.cache.lock() else {
                return estimated;
            };
            cache.clock = cache.clock.saturating_add(1);
            let last_used = cache.clock;
            let Some(usage) = cache.runs.get_mut(run_id) else {
                return estimated;
            };
            if usage.metadata.as_ref() != metadata || usage.fixed_hash != fixed_hash(request) {
                return estimated;
            }
            usage.last_used = last_used;
            usage.confirmed.clone()
        };
        let Some(baseline) = baseline else {
            return estimated;
        };
        if !baseline.matches(request) {
            return estimated;
        }
        // Subtract the prefix with the same counter and framing. Counting a
        // standalone extension would double-count request-level overhead.
        let prefix_estimate = counter.count_prefix_tokens(request, baseline.message_count);
        let Some(extension) = estimated.checked_sub(prefix_estimate) else {
            return estimated;
        };
        baseline.tokens.saturating_add(extension)
    }

    pub(super) fn forget(&self, run_id: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.runs.remove(run_id);
        }
    }
}

impl UsageBaseline {
    fn matches(&self, request: &ModelRequest) -> bool {
        request.messages.len() >= self.message_count
            && hash_messages(&request.messages[..self.message_count]).finish() == self.prefix_hash
    }
}

fn fixed_hash(request: &ModelRequest) -> u64 {
    let mut hasher = DefaultHasher::new();
    request.instructions.hash(&mut hasher);
    request.tools.len().hash(&mut hasher);
    for tool in &request.tools {
        tool.name.hash(&mut hasher);
        tool.description.hash(&mut hasher);
        tool.input_schema.hash(&mut hasher);
    }
    // max_output_tokens changes the next response limit, not this input prefix.
    hasher.finish()
}

fn hash_messages(messages: &[ModelMessage]) -> DefaultHasher {
    let mut hasher = DefaultHasher::new();
    serde_json::to_vec(messages)
        .expect("canonical messages are serializable")
        .hash(&mut hasher);
    hasher
}

#[cfg(test)]
mod tests;
