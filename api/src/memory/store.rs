use agent_core::agent::RunEventEnvelope;
use agent_memory::{WikiSourceCursor, WikiState, WikiStore, WikiUpdate};
use std::{io, path::PathBuf, sync::Mutex};

/// An optional plugin's storage failure must not prevent the application from
/// starting. Retry initialization on the next access after the user repairs it.
pub(crate) struct MemoryStore {
    root: PathBuf,
    store: Mutex<Option<WikiStore>>,
}

impl MemoryStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            store: Mutex::new(None),
        }
    }

    fn access<T>(&self, operation: impl FnOnce(&WikiStore) -> io::Result<T>) -> io::Result<T> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| io::Error::other("memory lock poisoned"))?;
        if store.is_none() {
            *store = Some(WikiStore::open(&self.root)?);
        }
        operation(store.as_ref().unwrap())
    }

    #[cfg(test)]
    pub fn load(&self, run: &str) -> io::Result<Option<WikiState>> {
        self.access(|store| store.load(run))
    }

    pub fn validate_source(
        &self,
        run: &str,
        events: &[RunEventEnvelope],
    ) -> io::Result<Option<WikiState>> {
        self.access(|store| store.validate_source(run, events))
    }

    #[cfg(test)]
    pub fn apply(
        &self,
        run: &str,
        events: &[RunEventEnvelope],
        through: u64,
        update: WikiUpdate,
    ) -> io::Result<WikiState> {
        self.access(|store| store.apply(run, events, through, update))
    }

    pub fn apply_progress(
        &self,
        run: &str,
        events: &[RunEventEnvelope],
        through: u64,
        cursor: WikiSourceCursor,
        update: WikiUpdate,
    ) -> io::Result<WikiState> {
        self.access(|store| store.apply_progress(run, events, through, cursor, update))
    }

    pub fn query_validated(
        &self,
        run: &str,
        events: &[RunEventEnvelope],
        arguments: &str,
    ) -> io::Result<String> {
        self.access(|store| store.query_validated(run, events, arguments))
    }
}
