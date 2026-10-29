use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    io,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Allow,
    Always,
    Cancel,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalExtent {
    #[default]
    Target,
    Capability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalGrant {
    Once,
    Scoped,
    Session,
    Always,
    SessionCapability,
    AlwaysCapability,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionApproval {
    pub id: String,
    pub server_id: String,
    pub server_name: String,
    pub operation: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_scope: Option<super::control::ControlScope>,
    pub permission_target: Option<super::control_permissions::PermissionTarget>,
    pub can_allow_capability: bool,
}

struct Pending {
    run: String,
    view: ExtensionApproval,
    sender: oneshot::Sender<Option<ApprovalGrant>>,
    family: Option<String>,
}

type ApprovalGates = HashMap<(String, String), Weak<tokio::sync::Mutex<()>>>;

pub struct ApprovalQueue {
    pending: Mutex<BTreeMap<String, Pending>>,
    namespace: String,
    sequence: AtomicU64,
    gates: Mutex<ApprovalGates>,
    cancellations: Mutex<HashMap<(String, String), u64>>,
}

impl Default for ApprovalQueue {
    fn default() -> Self {
        static NEXT_QUEUE: AtomicU64 = AtomicU64::new(0);
        Self {
            pending: Mutex::default(),
            // Audit records survive restarts. Match the host run-ID strategy
            // so fresh requests cannot alias an earlier queue's persisted ID.
            namespace: format!(
                "{:x}-{:x}-{:x}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                std::process::id(),
                NEXT_QUEUE.fetch_add(1, Ordering::Relaxed)
            ),
            sequence: AtomicU64::new(0),
            gates: Mutex::default(),
            cancellations: Mutex::default(),
        }
    }
}

struct Guard {
    queue: Arc<ApprovalQueue>,
    id: String,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.queue.pending.lock().unwrap().remove(&self.id);
    }
}

impl ApprovalQueue {
    pub async fn capability_gate(
        &self,
        root: &str,
        server: &str,
    ) -> io::Result<tokio::sync::OwnedMutexGuard<()>> {
        self.permission_gate(root, server, "capability").await
    }
    pub async fn permission_gate(
        &self,
        root: &str,
        server: &str,
        target: &str,
    ) -> io::Result<tokio::sync::OwnedMutexGuard<()>> {
        let key = (root.to_owned(), format!("{server}:{target}"));
        let generation = self
            .cancellations
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or_default();
        let gate = {
            let mut gates = self.gates.lock().unwrap();
            gates.retain(|_, gate| gate.strong_count() > 0);
            let key = key.clone();
            if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
                gate
            } else {
                let gate = Arc::new(tokio::sync::Mutex::new(()));
                gates.insert(key, Arc::downgrade(&gate));
                gate
            }
        };
        let guard = gate.lock_owned().await;
        if self
            .cancellations
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or_default()
            != generation
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Control permission request was cancelled. Do not repeat it without new user instruction.",
            ));
        }
        Ok(guard)
    }
    fn cancel_waiters(&self, pending: &Pending) {
        if let Some(root) = &pending.family {
            let mut cancellations = self.cancellations.lock().unwrap();
            let generation = cancellations
                .entry((
                    root.clone(),
                    format!(
                        "{}:{}",
                        pending.view.server_id,
                        pending
                            .view
                            .permission_target
                            .as_ref()
                            .map(|t| t.key())
                            .unwrap_or_else(|| "capability".into())
                    ),
                ))
                .or_default();
            *generation = generation.saturating_add(1);
        }
    }
    pub fn decide_permission(
        &self,
        run: &str,
        id: &str,
        decision: ApprovalDecision,
    ) -> io::Result<()> {
        self.decide_permission_with_extent(run, id, decision, ApprovalExtent::Target)
    }
    pub fn decide_permission_with_extent(
        &self,
        run: &str,
        id: &str,
        decision: ApprovalDecision,
        extent: ApprovalExtent,
    ) -> io::Result<()> {
        let mut pending = self.pending.lock().unwrap();
        let request = pending
            .get(id)
            .filter(|request| request.run == run)
            .ok_or_else(|| io::Error::other("审批已失效，请刷新会话"))?;
        if matches!(extent, ApprovalExtent::Capability)
            && !request.view.can_allow_capability
            && !matches!(decision, ApprovalDecision::Cancel)
        {
            return Err(io::Error::other("此操作不支持整个能力授权"));
        }
        let reusable =
            request.view.permission_target.is_some() || request.view.session_scope.is_some();
        let grant = match (decision, extent) {
            (ApprovalDecision::Cancel, _) => None,
            (ApprovalDecision::Allow, ApprovalExtent::Capability) => {
                Some(ApprovalGrant::SessionCapability)
            }
            (ApprovalDecision::Always, ApprovalExtent::Capability) => {
                Some(ApprovalGrant::AlwaysCapability)
            }
            (ApprovalDecision::Allow, _) => Some(if reusable {
                ApprovalGrant::Session
            } else {
                ApprovalGrant::Once
            }),
            (ApprovalDecision::Always, _) if reusable => Some(ApprovalGrant::Always),
            (ApprovalDecision::Always, _) => return Err(io::Error::other("此操作不支持始终允许")),
        };
        if grant.is_none() {
            self.cancel_waiters(request);
        }
        pending
            .remove(id)
            .unwrap()
            .sender
            .send(grant)
            .map_err(|_| io::Error::other("审批已失效"))
    }
    pub fn list(&self, run: &str) -> Vec<ExtensionApproval> {
        self.pending
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.run == run)
            .map(|p| p.view.clone())
            .collect()
    }
    pub fn clear(&self) {
        self.pending.lock().unwrap().clear();
    }
    pub fn clear_run(&self, run: &str) {
        self.pending
            .lock()
            .unwrap()
            .retain(|_, pending| pending.run != run);
    }
    pub fn invalidate_servers(&self, changed: &std::collections::HashSet<String>) {
        self.pending
            .lock()
            .unwrap()
            .retain(|_, request| !changed.contains(&request.view.server_id));
    }
    pub fn invalidate_for_runs(&self, server: &str, runs: &[String]) {
        self.pending
            .lock()
            .unwrap()
            .retain(|_, request| request.view.server_id != server || !runs.contains(&request.run));
    }
    /// Called while the registry lock still protects the newly approved whole
    /// capability. Already-visible target prompts can reuse that same human
    /// decision; later revocation still invalidates each operation's epoch.
    pub(super) fn satisfy_capability(&self, family: &str, server: &str) {
        let mut pending = self.pending.lock().unwrap();
        let ids: Vec<_> = pending
            .iter()
            .filter(|(_, request)| {
                request.family.as_deref() == Some(family)
                    && request.view.server_id == server
                    && request.view.can_allow_capability
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(request) = pending.remove(&id) {
                let _ = request.sender.send(Some(ApprovalGrant::SessionCapability));
            }
        }
    }

    pub fn decide(&self, run: &str, id: &str, allow: bool) -> io::Result<()> {
        self.decide_scoped(run, id, allow, false)
    }
    pub fn decide_scoped(&self, run: &str, id: &str, allow: bool, session: bool) -> io::Result<()> {
        let mut pending = self.pending.lock().unwrap();
        if pending.get(id).is_none_or(|request| request.run != run) {
            return Err(io::Error::other("审批已失效，请刷新会话"));
        }
        if session && (!allow || pending[id].view.session_scope.is_none()) {
            return Err(io::Error::other("此操作不支持会话授权"));
        }
        if !allow {
            self.cancel_waiters(&pending[id]);
        }
        pending
            .remove(id)
            .unwrap()
            .sender
            .send(allow.then_some(if session {
                ApprovalGrant::Scoped
            } else {
                ApprovalGrant::Once
            }))
            .map_err(|_| io::Error::other("审批已失效"))
    }
    pub async fn request(self: &Arc<Self>, run: &str, view: ExtensionApproval) -> io::Result<()> {
        self.request_scoped(run, view).await.map(|_| ())
    }
    pub async fn request_scoped(
        self: &Arc<Self>,
        run: &str,
        view: ExtensionApproval,
    ) -> io::Result<bool> {
        self.request_permission(run, view)
            .await
            .map(|grant| grant != ApprovalGrant::Once)
    }
    pub async fn request_permission(
        self: &Arc<Self>,
        run: &str,
        view: ExtensionApproval,
    ) -> io::Result<ApprovalGrant> {
        self.request_permission_in_family(run, view, None).await
    }
    pub async fn request_permission_in_family(
        self: &Arc<Self>,
        run: &str,
        mut view: ExtensionApproval,
        family: Option<&str>,
    ) -> io::Result<ApprovalGrant> {
        let id = format!(
            "mcp-{}-{}",
            self.namespace,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        );
        view.id.clone_from(&id);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            id.clone(),
            Pending {
                run: run.into(),
                view,
                sender,
                family: family.map(str::to_owned),
            },
        );
        // Cancellation removes the request immediately and never invokes the external operation.
        let _guard = Guard {
            queue: self.clone(),
            id,
        };
        match receiver.await {
            Ok(Some(grant)) => Ok(grant),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The user cancelled this operation or its approval expired. No permission was saved. Continue with another authorized approach when possible; do not repeat the cancelled request without new user instruction.",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(
        queue: &Arc<ApprovalQueue>,
    ) -> (String, tokio::task::JoinHandle<io::Result<ApprovalGrant>>) {
        let task_queue = queue.clone();
        let task = tokio::spawn(async move {
            task_queue
                .request_permission(
                    "same-conversation",
                    ExtensionApproval {
                        id: String::new(),
                        server_id: "documents".into(),
                        server_name: "Documents".into(),
                        operation: "call_mcp_tool: read".into(),
                        arguments: serde_json::json!({"name":"read"}),
                        source_run_id: None,
                        task_title: None,
                        session_scope: None,
                        permission_target: None,
                        can_allow_capability: false,
                    },
                )
                .await
        });
        let id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(pending) = queue.list("same-conversation").first() {
                    break pending.id.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        (id, task)
    }

    #[tokio::test]
    async fn recreated_queue_does_not_reuse_a_persisted_approval_id() {
        let previous = Arc::new(ApprovalQueue::default());
        let (previous_id, previous_request) = request(&previous).await;
        previous.clear();
        assert!(previous_request.await.unwrap().is_err());
        drop(previous);

        let restored = Arc::new(ApprovalQueue::default());
        let (current_id, current_request) = request(&restored).await;
        assert_ne!(current_id, previous_id);
        assert!(
            restored
                .decide("same-conversation", &previous_id, true)
                .is_err()
        );
        assert_eq!(restored.list("same-conversation").len(), 1);
        restored
            .decide("same-conversation", &current_id, true)
            .unwrap();
        assert_eq!(current_request.await.unwrap().unwrap(), ApprovalGrant::Once);
    }
}
