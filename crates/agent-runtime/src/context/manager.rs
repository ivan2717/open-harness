use super::checkpoint::{
    checkpoint_id, event_prefix_fingerprint, validate_checkpoint, validate_event_order,
};
use super::projection::{
    ToolOutputProjection, build_compacted_request, events_after, projected_omission,
    protected_request, select_compaction_boundary, uncompacted_context,
};
use super::usage::ContextUsageTracker;
use super::{
    CompactingContextError, ContextCheckpoint, ContextCheckpointStore, ContextCompactor,
    ContextInputRef, ContextManager, ContextPolicy, ContextPruning, ContextSummary,
    PreparedContext, TokenCounter,
};
use crate::{ModelContextMetadata, ModelOutput, ModelRequest};
use agent_core::agent::RunEventEnvelope;

#[derive(Debug, Clone)]
pub struct CompactingContextManager<C, K, N> {
    policy: ContextPolicy,
    compactor: C,
    checkpoint_store: K,
    token_counter: N,
    usage: ContextUsageTracker,
}

impl<C, K, N> CompactingContextManager<C, K, N> {
    pub fn new(policy: ContextPolicy, compactor: C, checkpoint_store: K, token_counter: N) -> Self {
        Self {
            policy,
            compactor,
            checkpoint_store,
            token_counter,
            usage: ContextUsageTracker::default(),
        }
    }

    pub fn policy(&self) -> ContextPolicy {
        self.policy
    }

    pub fn compactor(&self) -> &C {
        &self.compactor
    }

    pub fn checkpoint_store(&self) -> &K {
        &self.checkpoint_store
    }

    pub fn token_counter(&self) -> &N {
        &self.token_counter
    }
}

impl<C, K, N> CompactingContextManager<C, K, N>
where
    C: ContextCompactor,
    K: ContextCheckpointStore,
    N: TokenCounter,
{
    async fn compact_bounded(
        &self,
        policy: ContextPolicy,
        mut previous_summary: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
        projected: Option<(Option<ModelContextMetadata>, ModelRequest)>,
    ) -> Result<ContextSummary, CompactingContextError<C::Error, K::Error>> {
        let mut remaining = events.as_slice();
        let input_token_limit = self
            .compactor
            .input_token_limit()
            .unwrap_or(policy.input_token_limit());

        if let Some((metadata, request)) = projected
            && self
                .compactor
                .estimate_projected_input_tokens(metadata.as_ref(), &request)
                .map_err(CompactingContextError::Compactor)?
                .is_some_and(|tokens| tokens <= input_token_limit)
            && let Some(summary) = self
                .compactor
                .compact_projected(metadata, request)
                .await
                .map_err(CompactingContextError::Compactor)?
        {
            if summary.is_empty() {
                return Err(CompactingContextError::EmptySummary {
                    through_sequence: events.last().map_or(0, |event| event.sequence),
                });
            }
            return Ok(summary);
        }

        while !remaining.is_empty() {
            // Most summaries fit in one request. Estimate the complete batch
            // first so large histories do not need one probe per event.
            let full_tokens = self
                .compactor
                .estimate_input_tokens(previous_summary.as_ref(), remaining, &self.token_counter)
                .map_err(CompactingContextError::Compactor)?;
            let batch_len = if full_tokens <= input_token_limit {
                remaining.len()
            } else {
                // Keep a prefix observed to fit and an observed failing bound.
                // Adapter estimates need not be strictly monotonic: this search
                // may miss a larger fitting prefix, but never accepts an
                // unchecked one. `compact` also checks its final model input.
                let mut fitting = 0;
                let mut overflowing = remaining.len();
                let mut overflow_tokens = full_tokens;
                while overflowing - fitting > 1 {
                    let probe = fitting + (overflowing - fitting) / 2;
                    let tokens = self
                        .compactor
                        .estimate_input_tokens(
                            previous_summary.as_ref(),
                            &remaining[..probe],
                            &self.token_counter,
                        )
                        .map_err(CompactingContextError::Compactor)?;
                    if tokens <= input_token_limit {
                        fitting = probe;
                    } else {
                        overflowing = probe;
                        overflow_tokens = tokens;
                    }
                }
                if fitting == 0 {
                    return Err(CompactingContextError::CompactionInputTokenLimitExceeded {
                        sequence: remaining[0].sequence,
                        estimated_tokens: overflow_tokens,
                        input_token_limit,
                    });
                }
                fitting
            };
            let (batch, tail) = remaining.split_at(batch_len);
            let through_sequence = batch
                .last()
                .expect("a compaction batch always contains an event")
                .sequence;
            let summary = self
                .compactor
                .compact(previous_summary, batch.to_vec())
                .await
                .map_err(CompactingContextError::Compactor)?;
            if summary.is_empty() {
                return Err(CompactingContextError::EmptySummary { through_sequence });
            }
            previous_summary = Some(summary);
            remaining = tail;
        }

        previous_summary.ok_or(CompactingContextError::NoEventsToCompact)
    }
}

impl<C, K, N> CompactingContextManager<C, K, N>
where
    C: ContextCompactor,
    K: ContextCheckpointStore,
    N: TokenCounter,
{
    fn decision_tokens(&self, input: ContextInputRef<'_>, request: &ModelRequest) -> usize {
        self.measure_request(input, request)
            .expect("the built-in context manager always measures requests")
            .decision_input_tokens
    }

    async fn prepare_context(
        &self,
        input: ContextInputRef<'_>,
        policy: ContextPolicy,
        force_compaction: bool,
        manual: bool,
    ) -> Result<PreparedContext, CompactingContextError<C::Error, K::Error>> {
        validate_event_order(input.events)?;
        let mut protected = protected_request(input, policy.preserve_all_user_messages());
        protected.max_output_tokens = policy.output_token_limit();
        let mut protected_tokens = self.token_counter.count_tokens(&protected);

        let mut previous = match self.checkpoint_store.latest(input.run_id.to_owned()).await {
            Ok(previous) => previous,
            Err(source) => {
                let mut fallback = uncompacted_context(input);
                fallback.request.max_output_tokens = policy.output_token_limit();
                if !force_compaction
                    && self.decision_tokens(input, &fallback.request) <= policy.input_token_limit()
                {
                    return Ok(fallback);
                }
                return Err(CompactingContextError::CheckpointStore(source));
            }
        };
        if validate_checkpoint::<C::Error, K::Error>(input.run_id, input.events, previous.as_ref())
            .is_err()
        {
            if let Err(source) = self.checkpoint_store.clear(input.run_id.to_owned()).await {
                let mut fallback = uncompacted_context(input);
                fallback.request.max_output_tokens = policy.output_token_limit();
                if !force_compaction
                    && self.decision_tokens(input, &fallback.request) <= policy.input_token_limit()
                {
                    return Ok(fallback);
                }
                return Err(CompactingContextError::CheckpointStore(source));
            }
            previous = None;
        }

        let mut projection = ToolOutputProjection::restored(
            input,
            previous
                .as_ref()
                .and_then(|checkpoint| checkpoint.pruning.as_ref()),
        );
        let previous_through = previous
            .as_ref()
            .and_then(ContextCheckpoint::summary_through_sequence);
        let initial_tail = events_after(input.events, previous_through);
        let mut initial_omitted =
            projected_omission(input.run_id, previous.as_ref(), initial_tail, &projection);
        let mut initial_request = build_compacted_request(
            input,
            previous.as_ref(),
            initial_tail,
            &projection,
            initial_omitted.is_some(),
            policy.preserve_all_user_messages(),
        );
        initial_request.max_output_tokens = policy.output_token_limit();
        let mut initial_tokens = self.decision_tokens(input, &initial_request);
        let trigger = policy.effective_compaction_threshold(
            protected_tokens,
            previous.as_ref().map(|checkpoint| checkpoint.after_tokens),
        );
        if !force_compaction && !manual && initial_tokens < trigger {
            return Ok(PreparedContext {
                request: initial_request,
                omitted_history: initial_omitted,
            });
        }

        // Signed thinking binds the entire prefix. Even pruning an old result
        // would invalidate it, so portable maintenance never rewrites it.
        if input.events.iter().any(|event| matches!(&event.event,
            agent_core::agent::RunEvent::ModelResponseReceived { provider_continuation: None, provider_state: Some(state), .. }
                if state.scope == crate::ProviderReplayScope::Prefix
        )) {
            if force_compaction || initial_tokens > policy.input_token_limit() {
                return Err(CompactingContextError::PrefixBoundStateRequiresNativeCompaction);
            }
            return Ok(PreparedContext { request: initial_request, omitted_history: initial_omitted });
        }

        // Pressure first removes oversized tool details, then measures again.
        // Persist exact replacements before model calls so cancellation and a
        // failing summarizer cannot discard useful progress.
        projection.enable_pruning(policy.max_tool_output_chars());
        let pruned_omitted =
            projected_omission(input.run_id, previous.as_ref(), initial_tail, &projection);
        let mut pruned_request = build_compacted_request(
            input,
            previous.as_ref(),
            initial_tail,
            &projection,
            pruned_omitted.is_some(),
            policy.preserve_all_user_messages(),
        );
        pruned_request.max_output_tokens = policy.output_token_limit();
        let mut pruning_progress = false;
        if self.token_counter.count_tokens(&pruned_request)
            < self.token_counter.count_tokens(&initial_request)
        {
            let last = input.events.last().expect("pruning requires an event");
            let fingerprint = event_prefix_fingerprint(input.events)
                .map_err(CompactingContextError::Fingerprint)?;
            let replacements = projection.replacements(initial_tail);
            let projected_events = input
                .events
                .iter()
                .map(|event| projection.clipped_event(event))
                .collect::<Vec<_>>();
            let replacement_fingerprint = event_prefix_fingerprint(&projected_events)
                .map_err(CompactingContextError::Fingerprint)?;
            let pruning = ContextPruning {
                through_sequence: last.sequence,
                source_fingerprint: fingerprint.clone(),
                replacements,
            };
            let after_tokens = self.decision_tokens(input, &pruned_request);
            let checkpoint = ContextCheckpoint {
                run_id: input.run_id.to_owned(),
                id: format!(
                    "context-pruned:{}:{}:{replacement_fingerprint}",
                    input.run_id, last.sequence
                ),
                through_sequence: previous_through.unwrap_or(last.sequence),
                source_fingerprint: previous
                    .as_ref()
                    .filter(|checkpoint| !checkpoint.summary.is_empty())
                    .map_or(fingerprint, |checkpoint| {
                        checkpoint.source_fingerprint.clone()
                    }),
                before_tokens: initial_tokens,
                after_tokens,
                summary: previous
                    .as_ref()
                    .map_or_else(ContextSummary::default, |checkpoint| {
                        checkpoint.summary.clone()
                    }),
                pruning: Some(pruning),
            };
            if let Err(source) = self.checkpoint_store.save(checkpoint.clone()).await {
                if !force_compaction && !manual && initial_tokens <= policy.input_token_limit() {
                    return Ok(PreparedContext {
                        request: initial_request,
                        omitted_history: initial_omitted,
                    });
                }
                return Err(CompactingContextError::CheckpointStore(source));
            }
            previous = Some(checkpoint);
            initial_request = pruned_request;
            initial_tokens = after_tokens;
            initial_omitted =
                projected_omission(input.run_id, previous.as_ref(), initial_tail, &projection);
            pruning_progress = true;
            if initial_tokens < trigger && (!manual || force_compaction) {
                return Ok(PreparedContext {
                    request: initial_request,
                    omitted_history: initial_omitted,
                });
            }
        } else {
            // Exposing history tools can outweigh pruning. Do not silently use
            // an uncommitted or larger projection for the eventual request.
            projection = ToolOutputProjection::restored(
                input,
                previous
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.pruning.as_ref()),
            );
        }
        let compaction_is_required =
            force_compaction || initial_tokens > policy.input_token_limit();
        let can_fallback = ((!compaction_is_required && !manual) || pruning_progress)
            && initial_tokens <= policy.input_token_limit();
        if compaction_is_required && protected_tokens > policy.input_token_limit() {
            protected_tokens = self.decision_tokens(input, &protected);
        }
        if compaction_is_required && protected_tokens > policy.input_token_limit() {
            return Err(CompactingContextError::ProtectedContextTokenLimitExceeded {
                estimated_tokens: protected_tokens,
                input_token_limit: policy.input_token_limit(),
            });
        }
        let mut fixed = protected.clone();
        fixed.messages.clear();
        fixed.tools = initial_request.tools.clone();
        let recent_tail_tokens = policy.recent_tail_tokens_for_message_budget(
            policy
                .input_token_limit()
                .saturating_sub(self.token_counter.count_tokens(&fixed)),
        );

        let Some(mut compact_through_index) = select_compaction_boundary(
            input.events,
            previous_through,
            recent_tail_tokens,
            &projection,
            &self.token_counter,
            compaction_is_required || manual,
        ) else {
            if initial_tokens > policy.input_token_limit() {
                return Err(CompactingContextError::InputTokenLimitExceeded {
                    estimated_tokens: initial_tokens,
                    input_token_limit: policy.input_token_limit(),
                });
            }
            return Ok(PreparedContext {
                request: initial_request,
                omitted_history: initial_omitted,
            });
        };

        let through_sequence = input.events[compact_through_index].sequence;
        let mut summary_projection = ToolOutputProjection::restored(
            input,
            previous
                .as_ref()
                .and_then(|checkpoint| checkpoint.pruning.as_ref()),
        );
        summary_projection.enable_pruning(policy.max_tool_output_chars());
        let prefix_tail = events_after(&input.events[..=compact_through_index], previous_through);
        let mut projected_prefix = build_compacted_request(
            input,
            previous.as_ref(),
            prefix_tail,
            &projection,
            initial_omitted.is_some(),
            policy.preserve_all_user_messages(),
        );
        projected_prefix.max_output_tokens = policy.output_token_limit();
        let newly_compacted = input
            .events
            .iter()
            .filter(|event| {
                previous_through.is_none_or(|sequence| event.sequence > sequence)
                    && event.sequence <= through_sequence
            })
            // The durable event remains untouched. The compaction request is
            // itself model context, so bound large tool payloads there too.
            .map(|event| summary_projection.clipped_event(event))
            .collect();
        let mut summary = match self
            .compact_bounded(
                policy,
                previous
                    .as_ref()
                    .filter(|checkpoint| !checkpoint.summary.is_empty())
                    .map(|checkpoint| checkpoint.summary.clone()),
                newly_compacted,
                Some((input.model_metadata.cloned(), projected_prefix)),
            )
            .await
        {
            Ok(summary) => summary,
            Err(_) if can_fallback => {
                return Ok(PreparedContext {
                    request: initial_request,
                    omitted_history: initial_omitted,
                });
            }
            Err(error) => return Err(error),
        };

        if summary.is_empty() {
            if can_fallback {
                return Ok(PreparedContext {
                    request: initial_request,
                    omitted_history: initial_omitted,
                });
            }
            return Err(CompactingContextError::EmptySummary { through_sequence });
        }

        loop {
            let through_sequence = input.events[compact_through_index].sequence;
            let source_fingerprint =
                event_prefix_fingerprint(&input.events[..=compact_through_index])
                    .map_err(CompactingContextError::Fingerprint)?;
            let checkpoint_id = checkpoint_id(input.run_id, through_sequence, &source_fingerprint);
            let mut candidate_checkpoint = ContextCheckpoint {
                run_id: input.run_id.to_owned(),
                id: checkpoint_id,
                through_sequence,
                source_fingerprint,
                before_tokens: initial_tokens,
                after_tokens: 0,
                summary: summary.clone(),
                pruning: previous
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.pruning.clone())
                    .and_then(|mut pruning| {
                        pruning
                            .replacements
                            .retain(|replacement| replacement.sequence > through_sequence);
                        (!pruning.replacements.is_empty()).then_some(pruning)
                    }),
            };
            let tail = events_after(input.events, Some(through_sequence));
            let omitted_history =
                projected_omission(input.run_id, Some(&candidate_checkpoint), tail, &projection);
            let mut request = build_compacted_request(
                input,
                Some(&candidate_checkpoint),
                tail,
                &projection,
                omitted_history.is_some(),
                policy.preserve_all_user_messages(),
            );
            request.max_output_tokens = policy.output_token_limit();
            let after_tokens = self.decision_tokens(input, &request);
            let mut comparable_request = initial_request.clone();
            comparable_request.tools = request.tools.clone();
            // Compaction may expose history tools. Preserve the observed cost
            // of the original input and add only their estimated schema cost;
            // replacing the tool set invalidates the general usage baseline.
            let added_tool_tokens = self
                .token_counter
                .count_tokens(&comparable_request)
                .saturating_sub(self.token_counter.count_tokens(&initial_request));
            let comparable_before_tokens = initial_tokens.saturating_add(added_tool_tokens);
            candidate_checkpoint.before_tokens = comparable_before_tokens;

            if after_tokens > policy.compaction_target()
                && compact_through_index + 1 < input.events.len()
            {
                // Leave room for subsequent work, while retaining complete
                // recent steps where possible. Only hard-limit recovery may
                // fold the newest step. The latest user input is reinserted
                // verbatim even when that emergency path is necessary.
                let tail_request = build_compacted_request(
                    input,
                    None,
                    tail,
                    &projection,
                    false,
                    policy.preserve_all_user_messages(),
                );
                let tail_tokens = self.token_counter.count_tokens(&tail_request);
                let tail_budget =
                    tail_tokens.saturating_sub(after_tokens - policy.compaction_target());
                if let Some(next_index) = select_compaction_boundary(
                    input.events,
                    Some(through_sequence),
                    tail_budget,
                    &projection,
                    &self.token_counter,
                    after_tokens > policy.input_token_limit() || manual,
                ) {
                    let remaining_events = input.events[compact_through_index + 1..=next_index]
                        .iter()
                        .map(|event| projection.clipped_event(event))
                        .collect();
                    summary = match self
                        .compact_bounded(policy, Some(summary), remaining_events, None)
                        .await
                    {
                        Ok(summary) => summary,
                        Err(_) if can_fallback => {
                            return Ok(PreparedContext {
                                request: initial_request,
                                omitted_history: initial_omitted,
                            });
                        }
                        Err(error) => return Err(error),
                    };
                    compact_through_index = next_index;
                    continue;
                }
            }

            // A provider rejection overrides the old calibrated baseline.
            // Compare two local estimates when the prefix is rewritten; a
            // raw-after value is not comparable to a calibrated-before value.
            let raw_before = self.token_counter.count_tokens(&initial_request);
            let raw_after = self.token_counter.count_tokens(&request);
            let did_not_shrink = if force_compaction {
                raw_after >= raw_before
            } else {
                after_tokens >= comparable_before_tokens
            };
            if did_not_shrink {
                if can_fallback || manual {
                    return Ok(PreparedContext {
                        request: initial_request,
                        omitted_history: initial_omitted,
                    });
                }
                return Err(CompactingContextError::CompactionDidNotReduceTokens {
                    before_tokens: comparable_before_tokens,
                    after_tokens,
                });
            }
            if after_tokens > policy.input_token_limit() {
                if can_fallback {
                    return Ok(PreparedContext {
                        request: initial_request,
                        omitted_history: initial_omitted,
                    });
                }
                return Err(CompactingContextError::InputTokenLimitExceeded {
                    estimated_tokens: after_tokens,
                    input_token_limit: policy.input_token_limit(),
                });
            }

            if force_compaction {
                candidate_checkpoint.before_tokens =
                    self.token_counter.count_tokens(&comparable_request);
            }
            let checkpoint = ContextCheckpoint {
                // Forced recovery records comparable local estimates. The
                // returned measurement still includes decision headroom.
                after_tokens: if force_compaction {
                    raw_after
                } else {
                    after_tokens
                },
                ..candidate_checkpoint
            };
            tracing::debug!(
                run_id = input.run_id,
                before_tokens = comparable_before_tokens,
                after_tokens,
                target_tokens = policy.compaction_target(),
                through_sequence,
                "prepared compacted context"
            );
            if let Err(source) = self.checkpoint_store.save(checkpoint).await {
                if can_fallback {
                    return Ok(PreparedContext {
                        request: initial_request,
                        omitted_history: initial_omitted,
                    });
                }
                return Err(CompactingContextError::CheckpointStore(source));
            }

            return Ok(PreparedContext {
                request,
                omitted_history,
            });
        }
    }
}

impl<C, K, N> ContextManager for CompactingContextManager<C, K, N>
where
    C: ContextCompactor,
    K: ContextCheckpointStore,
    N: TokenCounter,
{
    type Error = CompactingContextError<C::Error, K::Error>;

    fn max_tool_output_chars(&self) -> Option<usize> {
        Some(self.policy.max_tool_output_chars())
    }

    fn request_usage_baseline(
        &self,
        metadata: Option<&ModelContextMetadata>,
        request: &ModelRequest,
    ) -> Option<crate::RequestUsageBaseline> {
        super::request_usage_baseline_with_counter(metadata, request, &self.token_counter)
    }

    fn estimate_request_tokens(&self, request: &crate::ModelRequest) -> Option<usize> {
        Some(self.token_counter.count_tokens(request))
    }

    fn measure_request(
        &self,
        input: ContextInputRef<'_>,
        request: &ModelRequest,
    ) -> Option<super::ContextMeasurement> {
        self.usage
            .restore(input.run_id, input.model_metadata, request, input.events);
        let measurement = self.usage.measurement(
            input.run_id,
            input.model_metadata,
            request,
            &self.token_counter,
        );
        Some(measurement.with_margin_percent(self.policy.estimation_margin_percent()))
    }

    async fn recover_overflow(
        &self,
        input: ContextInputRef<'_>,
        rejected: &ModelRequest,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        let policy = self
            .policy
            .resolve(input.model_metadata)
            .map_err(CompactingContextError::Policy)?;
        let prepared = self.prepare_context(input, policy, true, false).await?;
        // Compare with the same local counter: the provider baseline cannot
        // measure a rewritten prefix. Never retry merely because it changed.
        if self.token_counter.count_tokens(&prepared.request)
            >= self.token_counter.count_tokens(rejected)
        {
            return Ok(None);
        }
        self.usage.record_request(
            input.run_id,
            input.model_metadata,
            &prepared.request,
            &self.token_counter,
        );
        Ok(Some(prepared))
    }

    async fn compact_now(
        &self,
        input: ContextInputRef<'_>,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        let policy = self
            .policy
            .resolve(input.model_metadata)
            .map_err(CompactingContextError::Policy)?;
        let previous = self
            .checkpoint_store
            .latest(input.run_id.to_owned())
            .await
            .map_err(CompactingContextError::CheckpointStore)?;
        let prepared = match self.prepare_context(input, policy, false, true).await {
            Err(CompactingContextError::CompactionDidNotReduceTokens { .. }) => return Ok(None),
            result => result?,
        };
        let current = self
            .checkpoint_store
            .latest(input.run_id.to_owned())
            .await
            .map_err(CompactingContextError::CheckpointStore)?;
        if previous.as_ref().map(|checkpoint| &checkpoint.id)
            == current.as_ref().map(|checkpoint| &checkpoint.id)
        {
            return Ok(None);
        }
        self.usage.record_request(
            input.run_id,
            input.model_metadata,
            &prepared.request,
            &self.token_counter,
        );
        Ok(Some(prepared))
    }

    async fn observe_response(
        &self,
        input: ContextInputRef<'_>,
        output: &ModelOutput,
    ) -> Result<(), Self::Error> {
        self.usage
            .observe_response(input.run_id, input.model_metadata, output);
        Ok(())
    }

    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        let policy = self
            .policy
            .resolve(input.model_metadata)
            .map_err(CompactingContextError::Policy)?;
        let prepared = self.prepare_context(input, policy, false, false).await?;
        self.usage.record_request(
            input.run_id,
            input.model_metadata,
            &prepared.request,
            &self.token_counter,
        );
        Ok(prepared)
    }
}
