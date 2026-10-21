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
    ContextInputRef, ContextManager, ContextPolicy, ContextSummary, PreparedContext, TokenCounter,
};
use crate::{ModelOutput, ModelRequest};
use agent_core::agent::RunEventEnvelope;
use std::collections::VecDeque;

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
    ) -> Result<ContextSummary, CompactingContextError<C::Error, K::Error>> {
        let mut remaining = VecDeque::from(events);
        let input_token_limit = self
            .compactor
            .input_token_limit()
            .unwrap_or(policy.input_token_limit());

        while !remaining.is_empty() {
            let mut batch = Vec::new();
            loop {
                let next = remaining
                    .pop_front()
                    .expect("the loop only builds a batch while events remain");
                batch.push(next);
                let estimated_tokens = self
                    .compactor
                    .estimate_input_tokens(previous_summary.as_ref(), &batch, &self.token_counter)
                    .map_err(CompactingContextError::Compactor)?;

                if estimated_tokens > input_token_limit {
                    let overflow = batch
                        .pop()
                        .expect("the event that crossed the limit was just appended");
                    if batch.is_empty() {
                        return Err(CompactingContextError::CompactionInputTokenLimitExceeded {
                            sequence: overflow.sequence,
                            estimated_tokens,
                            input_token_limit,
                        });
                    }
                    remaining.push_front(overflow);
                    break;
                }

                if remaining.is_empty() {
                    break;
                }
            }

            let through_sequence = batch
                .last()
                .expect("a compaction batch always contains an event")
                .sequence;
            let summary = self
                .compactor
                .compact(previous_summary, batch)
                .await
                .map_err(CompactingContextError::Compactor)?;
            if summary.is_empty() {
                return Err(CompactingContextError::EmptySummary { through_sequence });
            }
            previous_summary = Some(summary);
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
    fn count_tokens(&self, input: ContextInputRef<'_>, request: &ModelRequest) -> usize {
        self.usage.estimate(
            input.run_id,
            input.model_metadata,
            request,
            &self.token_counter,
        )
    }

    async fn prepare_context(
        &self,
        input: ContextInputRef<'_>,
        policy: ContextPolicy,
    ) -> Result<PreparedContext, CompactingContextError<C::Error, K::Error>> {
        validate_event_order(input.events)?;
        let protected_tokens = self.token_counter.count_tokens(&protected_request(input));
        let projection = ToolOutputProjection::new(input, policy.max_tool_output_chars());

        let mut previous = match self.checkpoint_store.latest(input.run_id.to_owned()).await {
            Ok(previous) => previous,
            Err(source) => {
                let fallback = uncompacted_context(input, policy.max_tool_output_chars());
                if self.count_tokens(input, &fallback.request) <= policy.input_token_limit() {
                    return Ok(fallback);
                }
                return Err(CompactingContextError::CheckpointStore(source));
            }
        };
        if validate_checkpoint::<C::Error, K::Error>(input.run_id, input.events, previous.as_ref())
            .is_err()
        {
            if let Err(source) = self.checkpoint_store.clear(input.run_id.to_owned()).await {
                let fallback = uncompacted_context(input, policy.max_tool_output_chars());
                if self.count_tokens(input, &fallback.request) <= policy.input_token_limit() {
                    return Ok(fallback);
                }
                return Err(CompactingContextError::CheckpointStore(source));
            }
            previous = None;
        }

        let previous_through = previous
            .as_ref()
            .map(|checkpoint| checkpoint.through_sequence);
        let initial_tail = events_after(input.events, previous_through);
        let initial_omitted =
            projected_omission(input.run_id, previous.as_ref(), initial_tail, &projection);
        let initial_request = build_compacted_request(
            input,
            previous.as_ref(),
            initial_tail,
            &projection,
            initial_omitted.is_some(),
        );
        let initial_tokens = self.count_tokens(input, &initial_request);
        let compaction_is_required = initial_tokens > policy.input_token_limit();
        // The protected subset has no independent provider usage. Its raw
        // estimate must not veto a full request known to fit after calibration.
        // Once rewriting is necessary, that old baseline cannot size a new
        // layout, so preserve the protected-content preflight conservatively.
        if compaction_is_required && protected_tokens > policy.input_token_limit() {
            return Err(CompactingContextError::ProtectedContextTokenLimitExceeded {
                estimated_tokens: protected_tokens,
                input_token_limit: policy.input_token_limit(),
            });
        }

        let trigger = policy.effective_compaction_threshold(
            protected_tokens,
            previous.as_ref().map(|checkpoint| checkpoint.after_tokens),
        );
        if initial_tokens <= trigger {
            return Ok(PreparedContext {
                request: initial_request,
                omitted_history: initial_omitted,
            });
        }

        // Signed thinking may bind the complete prefix. Portable rewriting
        // cannot preserve that binding, even if every retained block is intact.
        if input.events.iter().any(|event| matches!(&event.event,
            agent_core::agent::RunEvent::ModelResponseReceived { provider_continuation: None, provider_state: Some(state), .. }
                if state.scope == crate::ProviderReplayScope::Prefix
        )) {
            if compaction_is_required {
                return Err(CompactingContextError::PrefixBoundStateRequiresNativeCompaction);
            }
            return Ok(PreparedContext { request: initial_request, omitted_history: initial_omitted });
        }

        let Some(mut compact_through_index) = select_compaction_boundary(
            input.events,
            previous_through,
            policy.recent_tail_tokens(),
            &projection,
            &self.token_counter,
            compaction_is_required,
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
        let newly_compacted = input
            .events
            .iter()
            .filter(|event| {
                previous_through.is_none_or(|sequence| event.sequence > sequence)
                    && event.sequence <= through_sequence
            })
            // The durable event remains untouched. The compaction request is
            // itself model context, so bound large tool payloads there too.
            .map(|event| projection.clipped_event(event))
            .collect();
        let mut summary = match self
            .compact_bounded(
                policy,
                previous
                    .as_ref()
                    .map(|checkpoint| checkpoint.summary.clone()),
                newly_compacted,
            )
            .await
        {
            Ok(summary) => summary,
            Err(_) if !compaction_is_required => {
                return Ok(PreparedContext {
                    request: initial_request,
                    omitted_history: initial_omitted,
                });
            }
            Err(error) => return Err(error),
        };

        if summary.is_empty() {
            if !compaction_is_required {
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
            };
            let tail = events_after(input.events, Some(through_sequence));
            let omitted_history =
                projected_omission(input.run_id, Some(&candidate_checkpoint), tail, &projection);
            let request = build_compacted_request(
                input,
                Some(&candidate_checkpoint),
                tail,
                &projection,
                omitted_history.is_some(),
            );
            let after_tokens = self.count_tokens(input, &request);
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
                // fold the newest step. Canonical user inputs stay outside the
                // summary even when that emergency path is necessary.
                let tail_request = build_compacted_request(input, None, tail, &projection, false);
                let tail_tokens = self.token_counter.count_tokens(&tail_request);
                let tail_budget =
                    tail_tokens.saturating_sub(after_tokens - policy.compaction_target());
                if let Some(next_index) = select_compaction_boundary(
                    input.events,
                    Some(through_sequence),
                    tail_budget,
                    &projection,
                    &self.token_counter,
                    after_tokens > policy.input_token_limit(),
                ) {
                    let remaining_events = input.events[compact_through_index + 1..=next_index]
                        .iter()
                        .map(|event| projection.clipped_event(event))
                        .collect();
                    summary = match self
                        .compact_bounded(policy, Some(summary), remaining_events)
                        .await
                    {
                        Ok(summary) => summary,
                        Err(_) if !compaction_is_required => {
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

            if after_tokens >= comparable_before_tokens {
                if !compaction_is_required {
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
                if !compaction_is_required {
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

            let checkpoint = ContextCheckpoint {
                after_tokens,
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
                if !compaction_is_required {
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

    fn estimate_request_tokens(&self, request: &crate::ModelRequest) -> Option<usize> {
        Some(self.token_counter.count_tokens(request))
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
        let mut prepared = self.prepare_context(input, policy).await?;
        prepared.request.max_output_tokens = policy.output_token_limit();
        self.usage
            .record_request(input.run_id, input.model_metadata, &prepared.request);
        Ok(prepared)
    }
}
