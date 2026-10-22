//! Provider-native context continuity, independent of the portable text summary.
mod checkpoint;
mod error;

pub use checkpoint::{
    InMemoryNativeContextCheckpointStore, InMemoryNativeContextCheckpointStoreError,
    NativeContextCheckpoint, NativeContextCheckpointStore,
};
pub use error::NativeContextError;

use super::{
    ContextInputRef, ContextManager, ContextPolicy, OmittedHistory, PreparedContext, TokenCounter,
    checkpoint::event_prefix_fingerprint,
    projection::{
        ToolOutputProjection, build_compacted_request, events_after, projected_omission,
        protected_user_context,
    },
    usage::ContextUsageTracker,
};
use crate::{
    ModelOutput, ModelRequest, ProviderContextIdentity, ProviderContextWindow, ProviderReplayScope,
};
use agent_core::agent::RunEvent;
use std::{error::Error, future::Future};

/// Adapts a provider's native compaction operation. A provider adapter must keep
/// the entire returned window, including retained messages, in the original order.
pub trait NativeContextCompactor: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn identity(&self) -> &ProviderContextIdentity;

    /// Native compaction is another adapter request boundary and must validate
    /// private response payloads before sending restored history.
    fn validate_replay_state(
        &self,
        _state: &crate::ProviderResponseState,
        _content: &[crate::ContentBlock],
    ) -> Result<(), crate::ProviderStateError> {
        Err(crate::ProviderStateError::UnsupportedFormat)
    }

    /// Maximum complete input accepted by the native compaction operation.
    /// This can differ from the main model's input budget.
    fn input_token_limit(&self) -> usize;

    /// `target_input_tokens` is the desired returned-item cost, excluding the
    /// protected context, instructions, and tool schemas added by the manager.
    fn compact(
        &self,
        request: ModelRequest,
        target_input_tokens: usize,
    ) -> impl Future<Output = Result<ProviderContextWindow, Self::Error>> + Send;
}

#[derive(Debug, Clone)]
pub struct NativeContextManager<C, K, N> {
    policy: ContextPolicy,
    compactor: C,
    checkpoint_store: K,
    token_counter: N,
    usage: ContextUsageTracker,
}

impl<C, K, N> NativeContextManager<C, K, N> {
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

impl<C: NativeContextCompactor, K: NativeContextCheckpointStore, N: TokenCounter>
    NativeContextManager<C, K, N>
{
    fn decision_tokens(&self, input: ContextInputRef<'_>, request: &ModelRequest) -> usize {
        self.measure_request(input, request)
            .expect("the built-in context manager always measures requests")
            .decision_input_tokens
    }

    fn validate_window(
        &self,
        window: &ProviderContextWindow,
    ) -> Result<(), NativeContextError<C::Error, K::Error>> {
        if window.items.is_empty()
            || window.input_tokens == 0
            || window.identity.provider.trim().is_empty()
            || window.identity.model.trim().is_empty()
            || window.identity.format.trim().is_empty()
        {
            return Err(NativeContextError::InvalidWindow);
        }
        if window.identity != *self.compactor.identity() {
            return Err(NativeContextError::IncompatibleWindow);
        }
        Ok(())
    }

    fn checkpoint_matches(
        &self,
        input: ContextInputRef<'_>,
        checkpoint: &NativeContextCheckpoint,
    ) -> Result<bool, NativeContextError<C::Error, K::Error>> {
        if checkpoint.run_id != input.run_id || self.validate_window(&checkpoint.window).is_err() {
            return Ok(false);
        }
        let Some(index) = input
            .events
            .iter()
            .position(|event| event.sequence == checkpoint.through_sequence)
        else {
            return Ok(false);
        };
        let fingerprint = event_prefix_fingerprint(&input.events[..=index])
            .map_err(NativeContextError::Fingerprint)?;
        Ok(checkpoint.source_fingerprint == fingerprint)
    }

    /// A response continuation is canonical journal data. Its provider window
    /// already contains the response's calls, but never their later results.
    fn journal_checkpoint(
        &self,
        input: ContextInputRef<'_>,
    ) -> Result<Option<NativeContextCheckpoint>, NativeContextError<C::Error, K::Error>> {
        let Some((response_index, continuation)) =
            input
                .events
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, event)| match &event.event {
                    RunEvent::ModelResponseReceived {
                        provider_continuation: Some(continuation),
                        ..
                    } => Some((index, continuation)),
                    _ => None,
                })
        else {
            return Ok(None);
        };
        self.validate_window(&continuation.window)?;
        let response_step = input.events[response_index].step;
        let mut through_index = response_index;
        for (index, event) in input.events.iter().enumerate().skip(response_index + 1) {
            if event.step != response_step
                || !matches!(event.event, RunEvent::ToolCallRequested { .. })
            {
                break;
            }
            through_index = index;
        }
        Ok(Some(NativeContextCheckpoint {
            run_id: input.run_id.to_owned(),
            through_sequence: input.events[through_index].sequence,
            source_fingerprint: event_prefix_fingerprint(&input.events[..=through_index])
                .map_err(NativeContextError::Fingerprint)?,
            window: continuation.window.clone(),
            needs_protected_context: continuation.compacted,
        }))
    }

    fn validate_request(
        &self,
        input: ContextInputRef<'_>,
        request: &mut ModelRequest,
    ) -> Result<(), NativeContextError<C::Error, K::Error>> {
        let validate = |request: &ModelRequest| {
            if let Some(expected) = request
                .provider_context
                .as_ref()
                .and_then(|window| window.prefix_fingerprint.as_deref())
                && request.replay_fixed_fingerprint()? != expected
            {
                return Err(crate::ProviderReplayError::PrefixChanged);
            }
            request.validate_provider_state_with(
                Some(self.compactor.identity()),
                false,
                |state, content| self.compactor.validate_replay_state(state, content),
            )
        };
        match validate(request) {
            Ok(()) => Ok(()),
            Err(crate::ProviderReplayError::PrefixChanged) if !input.deferred_tools.is_empty() => {
                // Exposing cold-history tools must not change the tool envelope
                // of an existing signed prefix. Business tools remain untouched.
                let mut without_deferred = request.clone();
                without_deferred.tools = input.tools.to_vec();
                validate(&without_deferred).map_err(NativeContextError::Replay)?;
                *request = without_deferred;
                Ok(())
            }
            Err(error) => Err(NativeContextError::Replay(error)),
        }
    }

    fn project(
        &self,
        input: ContextInputRef<'_>,
        policy: ContextPolicy,
        checkpoint: Option<&NativeContextCheckpoint>,
    ) -> Result<PreparedContext, NativeContextError<C::Error, K::Error>> {
        let projection = ToolOutputProjection::restored(input, None);
        let tail = events_after(
            input.events,
            checkpoint.map(|checkpoint| checkpoint.through_sequence),
        );
        let clipped = projected_omission(input.run_id, None, tail, &projection);
        let omitted_history = checkpoint
            .map(|checkpoint| OmittedHistory {
                through_sequence: checkpoint.through_sequence,
                readable_through_sequence: clipped.as_ref().map_or(
                    checkpoint.through_sequence,
                    |clipped| {
                        clipped
                            .readable_through_sequence
                            .max(checkpoint.through_sequence)
                    },
                ),
                checkpoint_id: format!(
                    "native:{}:{}:{}",
                    input.run_id, checkpoint.through_sequence, checkpoint.source_fingerprint
                ),
            })
            .or(clipped);
        let mut request = build_compacted_request(
            input,
            None,
            tail,
            &projection,
            omitted_history.is_some(),
            policy.preserve_all_user_messages(),
        );
        if let Some(checkpoint) = checkpoint
            && checkpoint.needs_protected_context
            && let Some(protected) = protected_user_context(
                input,
                checkpoint.through_sequence,
                policy.preserve_all_user_messages(),
            )
        {
            // Keep pending native tool declarations adjacent to their tail
            // results before appending historical user context.
            request.messages.push(protected);
        }
        request.provider_context = checkpoint.map(|checkpoint| checkpoint.window.clone());
        request.max_output_tokens = policy.output_token_limit();
        self.validate_request(input, &mut request)?;
        Ok(PreparedContext {
            request,
            omitted_history,
        })
    }

    fn checkpoint(
        &self,
        input: ContextInputRef<'_>,
        window: ProviderContextWindow,
        needs_protected_context: bool,
    ) -> Result<NativeContextCheckpoint, NativeContextError<C::Error, K::Error>> {
        self.validate_window(&window)?;
        let through_sequence = input
            .events
            .last()
            .ok_or(NativeContextError::EmptyHistory)?
            .sequence;
        Ok(NativeContextCheckpoint {
            run_id: input.run_id.to_owned(),
            through_sequence,
            source_fingerprint: event_prefix_fingerprint(input.events)
                .map_err(NativeContextError::Fingerprint)?,
            window,
            needs_protected_context,
        })
    }

    fn protected_request(
        &self,
        input: ContextInputRef<'_>,
        initial: &PreparedContext,
    ) -> ModelRequest {
        let mut fixed = initial.request.clone();
        fixed.provider_context = None;
        fixed.messages = input
            .events
            .last()
            .and_then(|last| {
                protected_user_context(
                    input,
                    last.sequence,
                    self.policy.preserve_all_user_messages(),
                )
            })
            .into_iter()
            .collect();
        if initial.omitted_history.is_none() {
            fixed.tools.extend_from_slice(input.deferred_tools);
        }
        fixed
    }

    async fn compact(
        &self,
        input: ContextInputRef<'_>,
        policy: ContextPolicy,
        initial: &PreparedContext,
        force_compaction: bool,
    ) -> Result<PreparedContext, NativeContextError<C::Error, K::Error>> {
        let before = self.decision_tokens(input, &initial.request);
        if before > self.compactor.input_token_limit() {
            return Err(NativeContextError::CompactionInputLimitExceeded {
                tokens: before,
                limit: self.compactor.input_token_limit(),
            });
        }
        let fixed = self.protected_request(input, initial);
        let mut fixed_tokens = self.token_counter.count_tokens(&fixed);
        if fixed_tokens >= policy.input_token_limit() {
            fixed_tokens = self.decision_tokens(input, &fixed);
        }
        if fixed_tokens >= policy.input_token_limit() {
            return Err(NativeContextError::ProtectedContextLimitExceeded {
                tokens: fixed_tokens,
                limit: policy.input_token_limit(),
            });
        }
        let target = policy
            .compaction_target()
            .saturating_sub(fixed_tokens)
            .max(1);
        // prepare() precedes the driver's validation. Never send invalid state
        // to an external compaction adapter, even when a cache fallback fits.
        let mut compaction_request = initial.request.clone();
        self.validate_request(input, &mut compaction_request)?;
        let prefix_bound = compaction_request
            .provider_context
            .as_ref()
            .is_some_and(|window| window.prefix_fingerprint.is_some())
            || compaction_request.messages.iter().any(|message| {
                message
                    .provider_state
                    .as_ref()
                    .is_some_and(|state| state.scope == ProviderReplayScope::Prefix)
            });
        let fixed_fingerprint = compaction_request
            .replay_fixed_fingerprint()
            .map_err(NativeContextError::Replay)?;
        let mut window = self
            .compactor
            .compact(compaction_request, target)
            .await
            .map_err(NativeContextError::Compactor)?;
        if prefix_bound {
            window.prefix_fingerprint = Some(fixed_fingerprint);
        }
        let checkpoint = self.checkpoint(input, window, true)?;
        let candidate = self.project(input, policy, Some(&checkpoint))?;
        let after = self.decision_tokens(input, &candidate.request);
        if after > policy.input_token_limit() {
            return Err(NativeContextError::InputTokenLimitExceeded {
                tokens: after,
                limit: policy.input_token_limit(),
            });
        }
        let mut comparable = initial.request.clone();
        comparable.tools = candidate.request.tools.clone();
        let added_tool_tokens = self
            .token_counter
            .count_tokens(&comparable)
            .saturating_sub(self.token_counter.count_tokens(&initial.request));
        let before = before.saturating_add(added_tool_tokens);
        let did_not_shrink = if force_compaction {
            self.token_counter.count_tokens(&candidate.request)
                >= self.token_counter.count_tokens(&initial.request)
        } else {
            after >= before
        };
        if did_not_shrink {
            return Err(NativeContextError::CompactionDidNotReduceTokens { before, after });
        }
        self.checkpoint_store
            .save(checkpoint)
            .await
            .map_err(NativeContextError::CheckpointStore)?;
        Ok(candidate)
    }
}

impl<C: NativeContextCompactor, K: NativeContextCheckpointStore, N: TokenCounter>
    NativeContextManager<C, K, N>
{
    async fn prepare_context(
        &self,
        input: ContextInputRef<'_>,
        policy: ContextPolicy,
        force_compaction: bool,
    ) -> Result<PreparedContext, NativeContextError<C::Error, K::Error>> {
        if input
            .events
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
        {
            return Err(NativeContextError::InvalidEventOrder);
        }
        let journal = self.journal_checkpoint(input)?;
        let mut previous = match self.checkpoint_store.latest(input.run_id.to_owned()).await {
            Ok(previous) => previous,
            Err(_) if journal.is_some() => None,
            Err(error) => {
                let fallback = self.project(input, policy, None)?;
                if !force_compaction
                    && self.decision_tokens(input, &fallback.request) <= policy.input_token_limit()
                {
                    return Ok(fallback);
                }
                return Err(NativeContextError::CheckpointStore(error));
            }
        };
        if let Some(checkpoint) = &previous
            && !self.checkpoint_matches(input, checkpoint)?
        {
            if let Err(error) = self.checkpoint_store.clear(input.run_id.to_owned()).await
                && journal.is_none()
            {
                let fallback = self.project(input, policy, None)?;
                if !force_compaction
                    && self.decision_tokens(input, &fallback.request) <= policy.input_token_limit()
                {
                    return Ok(fallback);
                }
                return Err(NativeContextError::CheckpointStore(error));
            }
            previous = None;
        }
        if let Some(journal) = journal
            && previous
                .as_ref()
                .is_none_or(|cached| journal.through_sequence > cached.through_sequence)
        {
            previous = Some(journal);
        }
        let initial = self.project(input, policy, previous.as_ref())?;
        let tokens = self.decision_tokens(input, &initial.request);
        let threshold = policy.effective_compaction_threshold(
            self.token_counter
                .count_tokens(&self.protected_request(input, &initial)),
            None,
        );
        if !force_compaction && tokens <= threshold {
            return Ok(initial);
        }
        match self
            .compact(input, policy, &initial, force_compaction)
            .await
        {
            Ok(candidate) => Ok(candidate),
            Err(error @ NativeContextError::Replay(_)) => Err(error),
            Err(_) if !force_compaction && tokens <= policy.input_token_limit() => Ok(initial),
            Err(error) => {
                // A durable native continuation may encode server compaction
                // that cannot be reconstructed from portable response events.
                if force_compaction || self.journal_checkpoint(input)?.is_some() {
                    return Err(error);
                }
                let fallback = self.project(input, policy, None)?;
                if self.decision_tokens(input, &fallback.request) <= policy.input_token_limit() {
                    tracing::warn!(
                        run_id = input.run_id,
                        error = %error,
                        "native context unavailable; using durable response events"
                    );
                    return Ok(fallback);
                }
                Err(error)
            }
        }
    }
}

impl<C: NativeContextCompactor, K: NativeContextCheckpointStore, N: TokenCounter> ContextManager
    for NativeContextManager<C, K, N>
{
    type Error = NativeContextError<C::Error, K::Error>;

    fn max_tool_output_chars(&self) -> Option<usize> {
        Some(self.policy.max_tool_output_chars())
    }

    fn request_usage_baseline(
        &self,
        metadata: Option<&crate::ModelContextMetadata>,
        request: &ModelRequest,
    ) -> Option<crate::RequestUsageBaseline> {
        crate::context::request_usage_baseline_with_counter(metadata, request, &self.token_counter)
    }

    fn estimate_request_tokens(&self, request: &ModelRequest) -> Option<usize> {
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
            .map_err(NativeContextError::Policy)?;
        let prepared = self.prepare_context(input, policy, true).await?;
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

    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        let policy = self
            .policy
            .resolve(input.model_metadata)
            .map_err(NativeContextError::Policy)?;
        let prepared = self.prepare_context(input, policy, false).await?;
        self.usage.record_request(
            input.run_id,
            input.model_metadata,
            &prepared.request,
            &self.token_counter,
        );
        Ok(prepared)
    }

    async fn observe_response(
        &self,
        input: ContextInputRef<'_>,
        output: &ModelOutput,
    ) -> Result<(), Self::Error> {
        self.usage
            .observe_response(input.run_id, input.model_metadata, output);
        let policy = self
            .policy
            .resolve(input.model_metadata)
            .map_err(NativeContextError::Policy)?;
        let Some(window) = &output.provider_context else {
            return Ok(());
        };
        let checkpoint = self.checkpoint(input, window.clone(), output.context_compacted)?;
        if let Err(error) = self.checkpoint_store.save(checkpoint).await {
            let journal = self.journal_checkpoint(input)?;
            let fallback = self.project(input, policy, journal.as_ref())?;
            if self.decision_tokens(input, &fallback.request) <= policy.input_token_limit() {
                // Canonical response events preserve native continuations as
                // well as response-local payloads; the cache is derived only.
                tracing::warn!(
                    run_id = input.run_id,
                    error = %error,
                    "native response checkpoint was not saved; durable event fallback is available"
                );
                return Ok(());
            }
            return Err(NativeContextError::CheckpointStore(error));
        }
        Ok(())
    }
}
