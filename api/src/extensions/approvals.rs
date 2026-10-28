use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    io,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Allow,
    Always,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalGrant {
    Once,
    Scoped,
    Session,
    Always,
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
}

struct Pending {
    run: String,
    view: ExtensionApproval,
    sender: oneshot::Sender<Option<ApprovalGrant>>,
    family: Option<String>,
}

#[derive(Default)]
pub struct ApprovalQueue {
    pending: Mutex<BTreeMap<String, Pending>>,
    sequence: AtomicU64,
    gates: Mutex<HashMap<(String, String), Weak<tokio::sync::Mutex<()>>>>,
    cancellations: Mutex<HashMap<(String, String), u64>>,
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
        let key = (root.to_owned(), server.to_owned());
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
            let key = (root.into(), server.into());
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
                .entry((root.clone(), pending.view.server_id.clone()))
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
        let mut pending = self.pending.lock().unwrap();
        let request = pending
            .get(id)
            .filter(|request| request.run == run)
            .ok_or_else(|| io::Error::other("审批已失效，请刷新会话"))?;
        let grant = match decision {
            ApprovalDecision::Cancel => None,
            ApprovalDecision::Allow => Some(if request.view.session_scope.is_some() {
                ApprovalGrant::Session
            } else {
                ApprovalGrant::Once
            }),
            ApprovalDecision::Always if request.view.session_scope.is_some() => {
                Some(ApprovalGrant::Always)
            }
            ApprovalDecision::Always => return Err(io::Error::other("此操作不支持始终允许")),
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
        let id = format!("mcp-{}", self.sequence.fetch_add(1, Ordering::Relaxed));
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
