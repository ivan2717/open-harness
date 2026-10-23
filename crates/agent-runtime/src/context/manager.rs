use super::checkpoint::{
    checkpoint_id, event_prefix_fingerprint, validate_checkpoint, validate_event_order,
};
use super::preparation::NoCompactionPreparation;
use super::projection::{
    ToolOutputProjection, build_compacted_request, events_after, projected_omission,
    protected_request, select_compaction_boundary, uncompacted_context,
};
use super::usage::ContextUsageTracker;
use super::{
    CompactingContextError, CompactionPreparationHook, ContextCheckpoint, ContextCheckpointStore,
    ContextCompactor, ContextInputRef, ContextManager, ContextPolicy, ContextPruning,
    ContextSummary, PreparedContext, TokenCounter,
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

    /// Recover a provider overflow, preparing optional host context before
    /// each selected summary boundary. Pending preparation cannot block recovery.
    pub async fn recover_overflow_with_hook<H: CompactionPreparationHook>(
        &self,
        input: ContextInputRef<'_>,
        rejected: &ModelRequest,
        hook: &H,
    ) -> Result<Option<PreparedContext>, CompactingContextError<C::Error, K::Error>> {
        let policy = self
            .policy
            .resolve(input.model_metadata)
            .map_err(CompactingContextError::Policy)?;
        let prepared = self
            .prepare_context(input, policy, true, false, hook)
            .await?;
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

    /// Explicit compaction with host preparation. A pending optional memory
    /// update does not prevent this user-requested operation.
    pub async fn compact_now_with_hook<H: CompactionPreparationHook>(
        &self,
        input: ContextInputRef<'_>,
        hook: &H,
    ) -> Result<Option<PreparedContext>, CompactingContextError<C::Error, K::Error>> {
        let policy = self
            .policy
            .resolve(input.model_metadata)
            .map_err(CompactingContextError::Policy)?;
        let previous = self
            .checkpoint_store
            .latest(input.run_id.to_owned())
            .await
            .map_err(CompactingContextError::CheckpointStore)?;
        let prepared = match self.prepare_context(input, policy, false, true, hook).await {
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

    /// Prepare model context, invoking the hook only when summary compaction
    /// is needed. Pending preparation may defer compaction while input fits.
    pub async fn prepare_with_hook<H: CompactionPreparationHook>(
        &self,
        input: ContextInputRef<'_>,
        hook: &H,
    ) -> Result<PreparedContext, CompactingContextError<C::Error, K::Error>> {
        let policy = self
            .policy
            .resolve(input.model_metadata)
            .map_err(CompactingContextError::Policy)?;
        let prepared = self
            .prepare_context(input, policy, false, false, hook)
            .await?;
        self.usage.record_request(
            input.run_id,
            input.model_metadata,
            &prepared.request,
            &self.token_counter,
        );
        Ok(prepared)
    }

    async fn prepare_context<H: CompactionPreparationHook>(
        &self,
        input: ContextInputRef<'_>,
        policy: ContextPolicy,
        force_compaction: bool,
        manual: bool,
        hook: &H,
    ) -> Result<PreparedContext, CompactingContextError<C::Error, K::Error>> {
        validate_event_order(input.events)?;
        // Even below the compaction threshold, inserting mutable derived data
        // could change the prefix covered by signed provider replay state.
        if !input.derived_messages.is_empty()
            && input.events.iter().any(|event| {
                matches!(
                    &event.event,
                    agent_core::agent::RunEvent::ModelResponseReceived {
                        provider_state: Some(state), ..
                    } if state.scope == crate::ProviderReplayScope::Prefix
                )
            })
        {
            return Err(CompactingContextError::DerivedContextWithPrefixBoundState);
        }
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
        fixed.messages = input.derived_messages.to_vec();
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

        // The boundary is now real. Pure pruning and below-threshold requests
        // have already returned, so optional host work cannot delay those paths.
        let original_input = input;
        let mut summary = previous
            .as_ref()
            .filter(|checkpoint| !checkpoint.summary.is_empty())
            .map(|checkpoint| checkpoint.summary.clone());
        let mut summarized_through = previous_through;
        let mut first_segment = true;
        let mut summary_projection = ToolOutputProjection::restored(
            input,
            previous
                .as_ref()
                .and_then(|checkpoint| checkpoint.pruning.as_ref()),
        );
        summary_projection.enable_pruning(policy.max_tool_output_chars());

        loop {
            let through_sequence = input.events[compact_through_index].sequence;
            let preparation = hook
                .before_compaction(original_input, through_sequence)
                .await;
            let input = original_input.with_derived_messages(&preparation.derived_messages);

            // Refresh every request affected by host notes, including fallbacks
            // and fixed input. A newly saved memory must not be invisible to this
            // request or bypass its token budget.
            protected = protected_request(input, policy.preserve_all_user_messages());
            protected.max_output_tokens = policy.output_token_limit();
            protected_tokens = self.token_counter.count_tokens(&protected);
            if protected_tokens > policy.input_token_limit() {
                protected_tokens = self.decision_tokens(input, &protected);
            }
            if protected_tokens > policy.input_token_limit() {
                return Err(CompactingContextError::ProtectedContextTokenLimitExceeded {
                    estimated_tokens: protected_tokens,
                    input_token_limit: policy.input_token_limit(),
                });
            }
            initial_request = build_compacted_request(
                input,
                previous.as_ref(),
                initial_tail,
                &projection,
                initial_omitted.is_some(),
                policy.preserve_all_user_messages(),
            );
            initial_request.max_output_tokens = policy.output_token_limit();
            initial_tokens = self.decision_tokens(input, &initial_request);
            let compaction_is_required =
                force_compaction || initial_tokens > policy.input_token_limit();
            let can_fallback = ((!compaction_is_required && !manual) || pruning_progress)
                && initial_tokens <= policy.input_token_limit();
            if !preparation.ready && !manual && !compaction_is_required {
                return Ok(PreparedContext {
                    request: initial_request,
                    omitted_history: initial_omitted,
                });
            }

            let projected = if first_segment {
                let prefix_tail =
                    events_after(&input.events[..=compact_through_index], previous_through);
                let mut request = build_compacted_request(
                    input,
                    previous.as_ref(),
                    prefix_tail,
                    &projection,
                    initial_omitted.is_some(),
                    policy.preserve_all_user_messages(),
                );
                request.max_output_tokens = policy.output_token_limit();
                Some((input.model_metadata.cloned(), request))
            } else {
                None
            };
            let newly_compacted = input
                .events
                .iter()
                .filter(|event| {
                    summarized_through.is_none_or(|sequence| event.sequence > sequence)
                        && event.sequence <= through_sequence
                })
                // The hook above sees canonical events. Only the independent
                // summary request gets a bounded tool-output projection.
                .map(|event| {
                    if first_segment {
                        summary_projection.clipped_event(event)
                    } else {
                        projection.clipped_event(event)
                    }
                })
                .collect();
            let next_summary = match self
                .compact_bounded(policy, summary.take(), newly_compacted, projected)
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
            summary = Some(next_summary.clone());
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
                summary: next_summary,
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
                let mut tail_request = build_compacted_request(
                    input.with_derived_messages(&[]),
                    None,
                    tail,
                    &projection,
                    false,
                    policy.preserve_all_user_messages(),
                );
                // Boundary selection measures event messages only. Fixed
                // instructions, schemas and refreshed host notes consume
                // budget but cannot be retained as additional event history.
                tail_request.instructions.clear();
                tail_request.tools.clear();
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
                    // The next iteration checks coverage of the expanded
                    // canonical boundary before consuming any additional text.
                    summarized_through = Some(through_sequence);
                    compact_through_index = next_index;
                    first_segment = false;
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
        self.recover_overflow_with_hook(input, rejected, &NoCompactionPreparation)
            .await
    }

    async fn compact_now(
        &self,
        input: ContextInputRef<'_>,
    ) -> Result<Option<PreparedContext>, Self::Error> {
        self.compact_now_with_hook(input, &NoCompactionPreparation)
            .await
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
        self.prepare_with_hook(input, &NoCompactionPreparation)
            .await
    }
}
