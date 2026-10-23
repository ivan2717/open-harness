//! Bounded, session-scoped wiki notes derived from a canonical event journal.
//! This crate performs no model requests and never edits the original journal.

mod files;
mod query;
mod task;
mod types;
mod validation;

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use agent_core::agent::RunEventEnvelope;
pub use query::{MAX_QUERY_OUTPUT_CHARS, tool_definition};
pub use task::{
    WikiEvidence, WikiTaskItem, WikiTaskKind, WikiTaskStatus, evidence_text, is_recall_tool,
    merge_items, redact_evidence_text, validate_items,
};
pub use types::*;

/// Local materialized memory. The caller supplies the canonical events for the
/// run; event envelopes do not themselves contain a run ID. `validate_source`
/// must succeed before injecting existing notes into a resumed run.
pub struct WikiStore {
    root: PathBuf,
    mutation: Mutex<()>,
}

impl WikiStore {
    pub fn open(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: files::directory(root)?,
            mutation: Mutex::new(()),
        })
    }

    pub fn load(&self, run_id: &str) -> io::Result<Option<WikiState>> {
        let Some(directory) = self.run_directory(run_id, false)? else {
            return Ok(None);
        };
        let Some(bytes) = files::read(&directory.join("wiki.json"))? else {
            return Ok(None);
        };
        let state: WikiState = serde_json::from_slice(&bytes).map_err(invalid)?;
        validation::state(&state, run_id)?;
        Ok(Some(state))
    }

    /// Verify that the journal prefix used to compile these notes is still the
    /// same. A fork or edited/truncated journal cannot reuse a stale snapshot.
    pub fn validate_source(
        &self,
        run_id: &str,
        events: &[RunEventEnvelope],
    ) -> io::Result<Option<WikiState>> {
        let Some(state) = self.load(run_id)? else {
            return Ok(None);
        };
        validation::source(&state, events)?;
        Ok(Some(state))
    }

    /// Atomically commit one generated update. Unmentioned stable page IDs are
    /// retained. This convenience method marks the complete prefix as consumed;
    /// chunked ingestion must use `apply_progress` instead.
    pub fn apply(
        &self,
        run_id: &str,
        events: &[RunEventEnvelope],
        through_sequence: u64,
        update: WikiUpdate,
    ) -> io::Result<WikiState> {
        self.apply_progress(
            run_id,
            events,
            through_sequence,
            WikiSourceCursor {
                sequence: through_sequence
                    .checked_add(1)
                    .ok_or_else(|| invalid("source cursor overflow"))?,
                offset: 0,
            },
            update,
        )
    }

    /// Commit generated notes and their ingestion cursor in one atomic write.
    /// The fingerprint boundary may remain fixed while multiple chunks of an
    /// event are processed, or while a legacy snapshot is backfilled.
    pub fn apply_progress(
        &self,
        run_id: &str,
        events: &[RunEventEnvelope],
        through_sequence: u64,
        cursor: WikiSourceCursor,
        update: WikiUpdate,
    ) -> io::Result<WikiState> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| invalid("memory write lock poisoned"))?;
        let directory = self
            .run_directory(run_id, true)?
            .expect("created directory");
        let _file_guard = files::lock(&directory)?;
        let previous = self.load(run_id)?;
        let through_sequence = previous.as_ref().map_or(through_sequence, |state| {
            through_sequence.max(state.through_sequence)
        });
        validation::cursor(cursor, through_sequence)?;
        let prefix = validation::prefix(events, through_sequence)?;
        validation::cursor_source(cursor, prefix)?;
        validation::update(&update, prefix)?;
        let update_fingerprint = validation::fingerprint(&update)?;
        if let Some(previous) = &previous {
            validation::source(previous, events)?;
            if let Some(previous_cursor) = previous.source_cursor {
                if cursor < previous_cursor {
                    return Err(invalid("memory ingestion cursor cannot move backwards"));
                }
                if cursor == previous_cursor {
                    if update_fingerprint != previous.update_fingerprint
                        || through_sequence != previous.through_sequence
                    {
                        return Err(invalid(
                            "different memory update already committed at this cursor",
                        ));
                    }
                    let _ = files::export(&directory, previous);
                    return Ok(previous.clone());
                }
            }
        }
        let revision = previous.as_ref().map_or(Ok(1), |state| {
            state
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("memory revision overflow"))
        })?;
        let previous_tasks = previous
            .as_ref()
            .map_or(&[][..], |state| state.tasks.as_slice());
        let tasks = task::merge_items(previous_tasks, &update.tasks, prefix)?;
        let mut updated_tasks: std::collections::BTreeSet<_> =
            update.tasks.iter().map(|item| item.id.clone()).collect();
        // Retired superseded items remain auditable when the bounded active
        // task set makes room for new knowledge.
        updated_tasks.extend(
            previous_tasks
                .iter()
                .filter(|old| !tasks.iter().any(|new| new.id == old.id))
                .map(|old| old.id.clone()),
        );
        let mut pages: BTreeMap<String, WikiPage> = previous
            .as_ref()
            .map(|state| {
                state
                    .pages
                    .iter()
                    .cloned()
                    .map(|page| (page.id.clone(), page))
                    .collect()
            })
            .unwrap_or_default();
        let mut changed = WikiRevision {
            revision,
            through_sequence,
            source_cursor: Some(cursor),
            updated_pages: Vec::new(),
            previous_pages: Vec::new(),
            previous_tasks: previous_tasks
                .iter()
                .filter(|old| updated_tasks.contains(&old.id))
                .cloned()
                .collect(),
            updated_tasks: updated_tasks.into_iter().collect(),
        };
        for page in update.pages {
            changed.updated_pages.push(page.id.clone());
            let page = WikiPage {
                id: page.id,
                title: page.title,
                content: page.content,
                source_sequences: page.source_sequences,
                updated_through: through_sequence,
            };
            if let Some(previous) = pages.insert(page.id.clone(), page) {
                changed.previous_pages.push(previous);
            }
        }
        if pages.len() > MAX_PAGES {
            return Err(invalid(
                "memory has too many pages; consolidate existing pages",
            ));
        }
        let mut revisions = previous.map(|state| state.revisions).unwrap_or_default();
        revisions.push(changed);
        if revisions.len() > types::MAX_REVISIONS {
            revisions.drain(..revisions.len() - types::MAX_REVISIONS);
        }
        let state = WikiState {
            schema_version: types::SCHEMA_VERSION,
            run_id: run_id.to_owned(),
            revision,
            through_sequence,
            source_cursor: Some(cursor),
            source_fingerprint: validation::source_fingerprint(run_id, prefix)?,
            overview: update.overview,
            pages: pages.into_values().collect(),
            tasks,
            revisions,
            update_fingerprint,
        };
        validation::state(&state, run_id)?;
        let bytes = serde_json::to_vec_pretty(&state).map_err(invalid)?;
        files::write(&directory.join("wiki.json"), &bytes)?;
        // Exports are disposable views. An export failure cannot roll back or
        // mask a successful authoritative commit; repair_exports can retry.
        let _ = files::export(&directory, &state);
        Ok(state)
    }

    pub fn overview(&self, run_id: &str, max_chars: usize) -> io::Result<Option<String>> {
        let Some(state) = self.load(run_id)? else {
            return Ok(None);
        };
        if max_chars == 0 {
            return Ok(None);
        }
        Ok(Some(state.overview(max_chars)))
    }

    pub fn query(&self, run_id: &str, arguments: &str) -> io::Result<String> {
        query::execute(self.load(run_id)?.as_ref(), arguments)
    }

    /// Query exactly the snapshot validated against this journal, without a
    /// second load that might observe a concurrent compiler revision.
    pub fn query_validated(
        &self,
        run_id: &str,
        events: &[RunEventEnvelope],
        arguments: &str,
    ) -> io::Result<String> {
        query::execute_validated(
            self.validate_source(run_id, events)?.as_ref(),
            events,
            arguments,
        )
    }

    /// Explicitly regenerate Markdown from the authoritative state. This also
    /// makes export errors observable to diagnostics without losing a revision.
    pub fn repair_exports(&self, run_id: &str) -> io::Result<()> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| invalid("memory write lock poisoned"))?;
        let Some(directory) = self.run_directory(run_id, false)? else {
            return Ok(());
        };
        let _file_guard = files::lock(&directory)?;
        if let Some(state) = self.load(run_id)? {
            files::export(&directory, &state)?;
        }
        Ok(())
    }

    fn run_directory(&self, run_id: &str, create: bool) -> io::Result<Option<PathBuf>> {
        validation::run_id(run_id)?;
        let id = validation::fingerprint(&("open-harness/session-memory/path/v1", run_id))?;
        files::child(&self.root, &id, create)
    }
}

impl WikiState {
    /// Render the already-validated immutable snapshot with a hard character
    /// budget, rather than reloading the store between validation and injection.
    pub fn overview(&self, max_chars: usize) -> String {
        let mut text = format!(
            "Session memory (derived notes, revision {}, through event {}). Newer instructions and current evidence take precedence. Use session_memory to read pages and canonical history to verify sources.\n\n{}\n\nPages:\n",
            self.revision, self.through_sequence, self.overview
        );
        for page in &self.pages {
            text.push_str(&format!("- {}: {}\n", page.id, page.title));
        }
        text.chars().take(max_chars.min(16_000)).collect()
    }
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
