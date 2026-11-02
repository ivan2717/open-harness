use agent_core::token_usage::ModelUsage;
use agent_local::CommandGrant;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalReview {
    pub id: String,
    pub run_id: String,
    pub operation: String,
    pub status: String,
    pub reason: String,
    pub model_id: String,
    pub created_at: String,
    pub usage: Option<ModelUsage>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct ConversationRecords {
    pub workspace: Option<String>,
    pub grants: Vec<CommandGrant>,
    pub reviews: Vec<ApprovalReview>,
}

pub(crate) struct PermissionRecords {
    path: PathBuf,
    state: Mutex<HashMap<String, ConversationRecords>>,
}

impl PermissionRecords {
    pub fn open(directory: &Path) -> io::Result<Self> {
        let path = directory.join("approval-records.json");
        let mut state: HashMap<String, ConversationRecords> = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path)?).map_err(io::Error::other)?
        } else {
            HashMap::new()
        };
        // In-flight reviews never survive a process restart as authorization.
        for records in state.values_mut() {
            for review in &mut records.reviews {
                if review.status == "reviewing" {
                    review.status = "manual".into();
                    review.reason = "审查已中断，请手动确认。".into();
                }
            }
        }
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub(crate) fn get(&self, root: &str) -> io::Result<ConversationRecords> {
        Ok(self
            .state
            .lock()
            .map_err(|_| io::Error::other("permission records poisoned"))?
            .get(root)
            .cloned()
            .unwrap_or_default())
    }

    /// Atomically save before publishing the in-memory state. Callers granting
    /// access hold the permission gate and local epoch lock until this returns.
    pub(crate) fn change(
        &self,
        root: &str,
        update: impl FnOnce(&mut ConversationRecords) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("permission records poisoned"))?;
        let mut next = state.clone();
        update(next.entry(root.into()).or_default())?;
        crate::snapshot::write_json(&self.path, &next)?;
        *state = next;
        Ok(())
    }

    pub(super) fn record(&self, root: &str, review: ApprovalReview) -> io::Result<()> {
        self.change(root, |records| {
            if let Some(existing) = records
                .reviews
                .iter_mut()
                .find(|r| r.id == review.id && r.run_id == review.run_id)
            {
                *existing = review;
            } else {
                records.reviews.push(review);
                if records.reviews.len() > 200 {
                    records.reviews.remove(0);
                }
            }
            Ok(())
        })
    }
}
