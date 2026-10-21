use crate::ProviderContextWindow;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    future::Future,
    sync::{Arc, RwLock},
};

/// A derived provider window anchored to the exact durable event prefix.
///
/// The prefix may end after tool declarations, before their results. Native
/// items preserve those declarations, so subsequent results can append safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeContextCheckpoint {
    pub run_id: String,
    pub through_sequence: u64,
    pub source_fingerprint: String,
    pub window: ProviderContextWindow,
    /// Freshly compacted windows require a protected user snapshot. A full
    /// response window already includes that snapshot, avoiding repeated copies.
    pub needs_protected_context: bool,
}

pub trait NativeContextCheckpointStore: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn latest(
        &self,
        run_id: String,
    ) -> impl Future<Output = Result<Option<NativeContextCheckpoint>, Self::Error>> + Send;

    /// Replaces the derived window at the same boundary (for recompression), or
    /// advances it. Implementations must not replace a newer prefix with an older one.
    fn save(
        &self,
        checkpoint: NativeContextCheckpoint,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn clear(&self, run_id: String) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryNativeContextCheckpointStore {
    inner: Arc<RwLock<HashMap<String, NativeContextCheckpoint>>>,
}

impl InMemoryNativeContextCheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl NativeContextCheckpointStore for InMemoryNativeContextCheckpointStore {
    type Error = InMemoryNativeContextCheckpointStoreError;

    async fn latest(&self, run_id: String) -> Result<Option<NativeContextCheckpoint>, Self::Error> {
        self.inner
            .read()
            .map_err(|_| Self::Error::LockPoisoned)
            .map(|checkpoints| checkpoints.get(&run_id).cloned())
    }

    async fn save(&self, checkpoint: NativeContextCheckpoint) -> Result<(), Self::Error> {
        let mut checkpoints = self.inner.write().map_err(|_| Self::Error::LockPoisoned)?;
        if let Some(previous) = checkpoints.get(&checkpoint.run_id)
            && previous.through_sequence > checkpoint.through_sequence
        {
            return Err(Self::Error::StaleBoundary {
                previous: previous.through_sequence,
                attempted: checkpoint.through_sequence,
            });
        }
        checkpoints.insert(checkpoint.run_id.clone(), checkpoint);
        Ok(())
    }

    async fn clear(&self, run_id: String) -> Result<(), Self::Error> {
        self.inner
            .write()
            .map_err(|_| Self::Error::LockPoisoned)?
            .remove(&run_id);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InMemoryNativeContextCheckpointStoreError {
    LockPoisoned,
    StaleBoundary { previous: u64, attempted: u64 },
}

impl fmt::Display for InMemoryNativeContextCheckpointStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => formatter.write_str("native checkpoint store lock was poisoned"),
            Self::StaleBoundary {
                previous,
                attempted,
            } => write!(
                formatter,
                "cannot replace native checkpoint at sequence {previous} with older sequence {attempted}"
            ),
        }
    }
}

impl Error for InMemoryNativeContextCheckpointStoreError {}
