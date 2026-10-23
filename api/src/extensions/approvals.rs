use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionApproval {
    pub id: String,
    pub server_id: String,
    pub server_name: String,
    pub operation: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_scope: Option<super::control::ControlScope>,
}

struct Pending {
    run: String,
    view: ExtensionApproval,
    sender: oneshot::Sender<(bool, bool)>,
}

#[derive(Default)]
pub struct ApprovalQueue {
    pending: Mutex<BTreeMap<String, Pending>>,
    sequence: AtomicU64,
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
        pending
            .remove(id)
            .unwrap()
            .sender
            .send((allow, session))
            .map_err(|_| io::Error::other("审批已失效"))
    }
    pub async fn request(self: &Arc<Self>, run: &str, view: ExtensionApproval) -> io::Result<()> {
        self.request_scoped(run, view).await.map(|_| ())
    }
    pub async fn request_scoped(
        self: &Arc<Self>,
        run: &str,
        mut view: ExtensionApproval,
    ) -> io::Result<bool> {
        let id = format!("mcp-{}", self.sequence.fetch_add(1, Ordering::Relaxed));
        view.id.clone_from(&id);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            id.clone(),
            Pending {
                run: run.into(),
                view,
                sender,
            },
        );
        // Cancellation removes the request immediately and never invokes the external operation.
        let _guard = Guard {
            queue: self.clone(),
            id,
        };
        match receiver.await {
            Ok((true, session)) => Ok(session),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "MCP operation was denied or its approval expired",
            )),
        }
    }
}
