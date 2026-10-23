use crate::WikiTaskItem;
use serde::{Deserialize, Serialize};

pub const MAX_OVERVIEW_CHARS: usize = 4_000;
pub const MAX_PAGE_CHARS: usize = 8_000;
pub const MAX_PAGES: usize = 64;
pub const MAX_UPDATE_PAGES: usize = 8;
pub const MAX_SOURCE_SEQUENCES: usize = 64;
pub(crate) const MAX_REVISIONS: usize = 64;
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// The next unprocessed source position. Offsets count Unicode characters in
/// the host's stable, redacted source representation, not bytes or tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiSourceCursor {
    pub sequence: u64,
    pub offset: usize,
}

/// A generated revision. Sources refer to canonical events in this run, never
/// to model reasoning or opaque provider state. Existing pages omitted from an
/// update are retained; a stable ID explicitly replaces the previous page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiUpdate {
    pub overview: String,
    pub pages: Vec<WikiPageUpdate>,
    #[serde(default)]
    pub tasks: Vec<WikiTaskItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiPageUpdate {
    pub id: String,
    pub title: String,
    pub content: String,
    pub source_sequences: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiPage {
    pub id: String,
    pub title: String,
    pub content: String,
    pub source_sequences: Vec<u64>,
    pub updated_through: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiRevision {
    pub revision: u64,
    pub through_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cursor: Option<WikiSourceCursor>,
    pub updated_pages: Vec<String>,
    /// Previous versions of changed pages only, never nested state snapshots.
    pub previous_pages: Vec<WikiPage>,
    #[serde(default)]
    pub updated_tasks: Vec<String>,
    #[serde(default)]
    pub previous_tasks: Vec<WikiTaskItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiState {
    pub schema_version: u32,
    pub run_id: String,
    pub revision: u64,
    pub through_sequence: u64,
    /// Legacy snapshots omitted this field and may have skipped source middles.
    /// Hosts must backfill them from the beginning instead of treating the
    /// fingerprint boundary as complete ingestion coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cursor: Option<WikiSourceCursor>,
    pub source_fingerprint: String,
    pub overview: String,
    pub pages: Vec<WikiPage>,
    #[serde(default)]
    pub tasks: Vec<WikiTaskItem>,
    pub revisions: Vec<WikiRevision>,
    pub(crate) update_fingerprint: String,
}
