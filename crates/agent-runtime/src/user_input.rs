use std::io;

use agent_core::TextFileReference;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// A user-owned message waiting outside model context. Claiming prevents edits
/// racing delivery; acknowledge only after the corresponding event is durable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingUserInput {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub files: Vec<TextFileReference>,
    #[serde(default)]
    pub steer: bool,
    #[serde(default)]
    pub delivering: bool,
    #[serde(default)]
    pub revision: u64,
}

/// Only inputs explicitly marked for steering can be claimed. Unselected drafts
/// remain outside the model context, including at turn boundaries.
pub trait UserInputQueue: std::fmt::Debug + Send + Sync {
    fn claim(&self) -> io::Result<Option<PendingUserInput>>;
    fn acknowledge(&self, id: &str) -> io::Result<()>;
    fn subscribe(&self) -> watch::Receiver<bool>;
}
