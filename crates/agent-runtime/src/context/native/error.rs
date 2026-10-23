use std::{error::Error, fmt};

#[derive(Debug)]
pub enum NativeContextError<CompactorError, StoreError> {
    Policy(super::super::ContextPolicyError),
    Compactor(CompactorError),
    CheckpointStore(StoreError),
    Fingerprint(serde_json::Error),
    Replay(crate::ProviderReplayError),
    InvalidEventOrder,
    EmptyHistory,
    InvalidWindow,
    IncompatibleWindow,
    DerivedContextUnsupported,
    CompactionInputLimitExceeded { tokens: usize, limit: usize },
    InputTokenLimitExceeded { tokens: usize, limit: usize },
    ProtectedContextLimitExceeded { tokens: usize, limit: usize },
    CompactionDidNotReduceTokens { before: usize, after: usize },
}

impl<C: fmt::Display, S: fmt::Display> fmt::Display for NativeContextError<C, S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy(error) => write!(formatter, "invalid model context policy: {error}"),
            Self::Compactor(error) => write!(formatter, "native compactor failed: {error}"),
            Self::CheckpointStore(error) => write!(formatter, "native checkpoint store failed: {error}"),
            Self::Fingerprint(error) => write!(formatter, "failed to fingerprint native context: {error}"),
            Self::Replay(error) => write!(formatter, "native provider replay is invalid: {error}"),
            Self::InvalidEventOrder => formatter.write_str("native context requires strictly increasing event sequences"),
            Self::EmptyHistory => formatter.write_str("native context requires a non-empty event history"),
            Self::InvalidWindow => formatter.write_str("native context requires a non-empty ordered window, positive token count, and complete provider identity"),
            Self::IncompatibleWindow => formatter.write_str("native context provider, model, or format does not match the configured compactor"),
            Self::DerivedContextUnsupported => formatter.write_str("derived context requires a portable context strategy; inserting it into a native window could invalidate provider state or split a tool call/result pair"),
            Self::CompactionInputLimitExceeded { tokens, limit } => write!(formatter, "native compaction input uses {tokens} tokens, exceeding its input limit of {limit}"),
            Self::InputTokenLimitExceeded { tokens, limit } => write!(formatter, "native context uses {tokens} tokens, exceeding the input limit of {limit}"),
            Self::ProtectedContextLimitExceeded { tokens, limit } => write!(formatter, "protected user context and fixed instructions/tools require {tokens} tokens, exceeding the input limit of {limit}"),
            Self::CompactionDidNotReduceTokens { before, after } => write!(formatter, "native compaction did not reduce tokens ({before} before, {after} after)"),
        }
    }
}

impl<C: Error + 'static, S: Error + 'static> Error for NativeContextError<C, S> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Policy(error) => Some(error),
            Self::Compactor(error) => Some(error),
            Self::CheckpointStore(error) => Some(error),
            Self::Fingerprint(error) => Some(error),
            Self::Replay(error) => Some(error),
            _ => None,
        }
    }
}
