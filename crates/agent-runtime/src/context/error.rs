use std::{error::Error, fmt};

#[derive(Debug)]
pub enum CompactingContextError<CompactorError, CheckpointStoreError> {
    Policy(super::ContextPolicyError),
    Compactor(CompactorError),
    CheckpointStore(CheckpointStoreError),
    EventSequenceNotIncreasing {
        previous: u64,
        current: u64,
    },
    CheckpointRunMismatch {
        expected_run_id: String,
        actual_run_id: String,
    },
    CheckpointSequenceMissing {
        checkpoint_id: String,
        through_sequence: u64,
    },
    CheckpointHistoryMismatch {
        checkpoint_id: String,
        expected_fingerprint: String,
        actual_fingerprint: String,
    },
    CheckpointSplitsStep {
        checkpoint_id: String,
        through_sequence: u64,
        step: u32,
    },
    EmptyCheckpointSummary {
        checkpoint_id: String,
    },
    EmptySummary {
        through_sequence: u64,
    },
    NoEventsToCompact,
    PrefixBoundStateRequiresNativeCompaction,
    CompactionInputTokenLimitExceeded {
        sequence: u64,
        estimated_tokens: usize,
        input_token_limit: usize,
    },
    CompactionDidNotReduceTokens {
        before_tokens: usize,
        after_tokens: usize,
    },
    InputTokenLimitExceeded {
        estimated_tokens: usize,
        input_token_limit: usize,
    },
    ProtectedContextTokenLimitExceeded {
        estimated_tokens: usize,
        input_token_limit: usize,
    },
    Fingerprint(serde_json::Error),
}

impl<CompactorError, CheckpointStoreError> fmt::Display
    for CompactingContextError<CompactorError, CheckpointStoreError>
where
    CompactorError: fmt::Display,
    CheckpointStoreError: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy(error) => write!(formatter, "invalid model context policy: {error}"),
            Self::Compactor(error) => write!(formatter, "context compactor failed: {error}"),
            Self::CheckpointStore(error) => write!(formatter, "checkpoint store failed: {error}"),
            Self::EventSequenceNotIncreasing { previous, current } => write!(
                formatter,
                "event sequences are not strictly increasing: {current} follows {previous}"
            ),
            Self::CheckpointRunMismatch {
                expected_run_id,
                actual_run_id,
            } => write!(
                formatter,
                "checkpoint belongs to run {actual_run_id}, not run {expected_run_id}"
            ),
            Self::CheckpointSequenceMissing {
                checkpoint_id,
                through_sequence,
            } => write!(
                formatter,
                "checkpoint {checkpoint_id} references missing sequence {through_sequence}"
            ),
            Self::CheckpointHistoryMismatch {
                checkpoint_id,
                expected_fingerprint,
                actual_fingerprint,
            } => write!(
                formatter,
                "checkpoint {checkpoint_id} was built from a different event prefix (expected {expected_fingerprint}, found {actual_fingerprint})"
            ),
            Self::CheckpointSplitsStep {
                checkpoint_id,
                through_sequence,
                step,
            } => write!(
                formatter,
                "checkpoint {checkpoint_id} at sequence {through_sequence} splits step {step}"
            ),
            Self::EmptyCheckpointSummary { checkpoint_id } => {
                write!(
                    formatter,
                    "checkpoint {checkpoint_id} contains an empty summary"
                )
            }
            Self::EmptySummary { through_sequence } => write!(
                formatter,
                "compactor returned an empty summary through sequence {through_sequence}"
            ),
            Self::PrefixBoundStateRequiresNativeCompaction => formatter.write_str("prefix-bound thinking state requires provider-native compaction; portable summarization would invalidate its prefix"),
            Self::NoEventsToCompact => formatter.write_str("context compaction selected no events"),
            Self::CompactionInputTokenLimitExceeded {
                sequence,
                estimated_tokens,
                input_token_limit,
            } => write!(
                formatter,
                "event at sequence {sequence} needs an estimated {estimated_tokens} tokens in a compaction request, exceeding the compactor input limit of {input_token_limit}"
            ),
            Self::CompactionDidNotReduceTokens {
                before_tokens,
                after_tokens,
            } => write!(
                formatter,
                "compaction did not reduce tokens ({before_tokens} before, {after_tokens} after)"
            ),
            Self::InputTokenLimitExceeded {
                estimated_tokens,
                input_token_limit,
            } => write!(
                formatter,
                "prepared context uses an estimated {estimated_tokens} tokens, exceeding the input limit of {input_token_limit}"
            ),
            Self::ProtectedContextTokenLimitExceeded {
                estimated_tokens,
                input_token_limit,
            } => write!(
                formatter,
                "protected user requirements, instructions, and tools need an estimated {estimated_tokens} tokens, exceeding the input limit of {input_token_limit}; they cannot be discarded by compaction"
            ),
            Self::Fingerprint(error) => {
                write!(formatter, "failed to fingerprint run history: {error}")
            }
        }
    }
}

impl<CompactorError, CheckpointStoreError> Error
    for CompactingContextError<CompactorError, CheckpointStoreError>
where
    CompactorError: Error + 'static,
    CheckpointStoreError: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Policy(error) => Some(error),
            Self::Compactor(error) => Some(error),
            Self::CheckpointStore(error) => Some(error),
            Self::Fingerprint(error) => Some(error),
            _ => None,
        }
    }
}
