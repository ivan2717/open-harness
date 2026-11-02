//! User-controlled, in-memory command grants. Model requests never create grants.
//! Each grant is one capability pair; independent approvals cannot be combined
//! to obtain a broader pair that the user never approved.
use crate::{ApprovalMode, NetworkAccess, PendingApproval, lock_error};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

pub(crate) const MAX_CONCURRENT_COMMANDS: usize = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAccess {
    ReadOnly,
    #[default]
    WorkspaceWrite,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandApprovalScope {
    #[default]
    Once,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandGrant {
    pub access: CommandAccess,
    pub network_access: NetworkAccess,
}

impl CommandGrant {
    fn covers(self, request: Self) -> bool {
        (self.access == CommandAccess::WorkspaceWrite || request.access == CommandAccess::ReadOnly)
            && (self.network_access == NetworkAccess::Allowed
                || request.network_access == NetworkAccess::Denied)
    }
}

/// Pending requests and grants share a single lock for the whole root
/// conversation. Publishing a grant and releasing covered requests is one
/// operation; a child can never miss a grant while registering its request.
pub(crate) struct CommandPermissions {
    state: Mutex<PermissionState>,
    slots: Arc<Semaphore>,
}

#[derive(Default)]
struct PermissionState {
    mode: ApprovalMode,
    epoch: u64,
    grants: Vec<CommandGrant>,
    pending: BTreeMap<String, PendingRequest>,
}

struct PendingRequest {
    owner: PathBuf,
    info: PendingApproval,
    sender: oneshot::Sender<Option<u64>>,
}

pub(crate) enum Authorization {
    Ready(u64),
    Pending(oneshot::Receiver<Option<u64>>),
}

impl Default for CommandPermissions {
    fn default() -> Self {
        Self {
            state: Mutex::new(PermissionState::default()),
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT_COMMANDS)),
        }
    }
}

impl PermissionState {
    fn allows(&self, request: CommandGrant) -> bool {
        self.grants.iter().any(|grant| grant.covers(request))
    }

    fn grant(&mut self, request: CommandGrant) {
        if !self.allows(request) {
            self.grants.retain(|grant| !request.covers(*grant));
            self.grants.push(request);
        }
        let ready: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| {
                self.allows(CommandGrant {
                    access: pending.info.access,
                    network_access: pending.info.network_access,
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            if let Some(pending) = self.pending.remove(&id) {
                let _ = pending.sender.send(Some(self.epoch));
            }
        }
    }

    fn revoke(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.grants.clear();
        // Dropping senders resolves all outstanding approvals as cancelled.
        self.pending.clear();
    }
}

impl CommandPermissions {
    pub fn mode(&self) -> io::Result<ApprovalMode> {
        Ok(self.state.lock().map_err(lock_error)?.mode)
    }

    pub fn set_mode(&self, mode: ApprovalMode) -> io::Result<()> {
        let mut state = self.state.lock().map_err(lock_error)?;
        if state.mode != mode {
            state.revoke();
            state.mode = mode;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn allows(&self, access: CommandAccess, network_access: NetworkAccess) -> io::Result<bool> {
        Ok(self.state.lock().map_err(lock_error)?.allows(CommandGrant {
            access,
            network_access,
        }))
    }

    #[cfg(test)]
    pub fn request(&self, owner: &Path, info: PendingApproval) -> io::Result<Authorization> {
        self.request_for_task(owner, info, false)
    }

    pub fn request_for_task(
        &self,
        owner: &Path,
        mut info: PendingApproval,
        host_access: bool,
    ) -> io::Result<Authorization> {
        let mut state = self.state.lock().map_err(lock_error)?;
        if host_access && state.mode != ApprovalMode::FullAccess {
            return Err(crate::paths::denied(
                "permission mode changed; retry with the current task policy",
            ));
        }
        // This is permission-boundary evaluation, not a shell-text allowlist.
        // The caller must already have verified the workspace/delegated scope;
        // the OS sandbox will enforce the requested filesystem scope.
        if (host_access && state.mode == ApprovalMode::FullAccess)
            || info.network_access == NetworkAccess::Denied
            || state.allows(CommandGrant {
                access: info.access,
                network_access: info.network_access,
            })
        {
            return Ok(Authorization::Ready(state.epoch));
        }
        info.review_mode = if state.mode == ApprovalMode::FullAccess {
            ApprovalMode::Ask
        } else {
            state.mode
        };
        info.epoch = state.epoch;
        let (sender, receiver) = oneshot::channel();
        state.pending.insert(
            info.id.clone(),
            PendingRequest {
                owner: owner.to_path_buf(),
                info,
                sender,
            },
        );
        Ok(Authorization::Pending(receiver))
    }

    pub fn pending(&self, owner: &Path) -> io::Result<Vec<PendingApproval>> {
        Ok(self
            .state
            .lock()
            .map_err(lock_error)?
            .pending
            .values()
            .filter(|pending| pending.owner == owner)
            .map(|pending| pending.info.clone())
            .collect())
    }

    pub fn remove_pending(&self, id: &str) -> io::Result<()> {
        self.state.lock().map_err(lock_error)?.pending.remove(id);
        Ok(())
    }

    pub fn cancel_pending(&self, owner: &Path) -> io::Result<()> {
        self.state
            .lock()
            .map_err(lock_error)?
            .pending
            .retain(|_, pending| pending.owner != owner);
        Ok(())
    }

    pub fn approve(
        &self,
        owner: &Path,
        id: &str,
        allow: bool,
        scope: CommandApprovalScope,
        expected_epoch: Option<u64>,
    ) -> io::Result<()> {
        self.approve_with(owner, id, allow, scope, expected_epoch, || Ok(()))
    }

    /// The host may atomically persist a user-approved grant while this policy
    /// revision is locked. The callback must not re-enter CommandPermissions.
    pub fn approve_with(
        &self,
        owner: &Path,
        id: &str,
        allow: bool,
        scope: CommandApprovalScope,
        expected_epoch: Option<u64>,
        before_release: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let mut state = self.state.lock().map_err(lock_error)?;
        let pending = state
            .pending
            .get(id)
            .filter(|pending| pending.owner == owner)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "approval is no longer pending")
            })?;
        if expected_epoch
            .is_some_and(|expected| expected != state.epoch || expected != pending.info.epoch)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "approval policy changed; discard the stale review",
            ));
        }
        if pending.sender.is_closed() {
            state.pending.remove(id);
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "command is no longer waiting",
            ));
        }
        let request = CommandGrant {
            access: pending.info.access,
            network_access: pending.info.network_access,
        };
        before_release()?;
        if allow && scope == CommandApprovalScope::Session {
            // Grant first, then release every covered request while retaining
            // the same lock. No command can race registration against this.
            state.grant(request);
        } else if let Some(pending) = state.pending.remove(id) {
            let _ = pending.sender.send(allow.then_some(state.epoch));
        }
        Ok(())
    }

    /// Host/UI use only. This does not remove the OS sandbox.
    pub fn grant(&self, access: CommandAccess, network_access: NetworkAccess) -> io::Result<()> {
        self.state.lock().map_err(lock_error)?.grant(CommandGrant {
            access,
            network_access,
        });
        Ok(())
    }

    pub fn revoke(&self) -> io::Result<()> {
        self.state.lock().map_err(lock_error)?.revoke();
        Ok(())
    }

    pub fn grants(&self) -> io::Result<Vec<CommandGrant>> {
        Ok(self.state.lock().map_err(lock_error)?.grants.clone())
    }

    /// Admission is the authorization commit point. Revocation before this
    /// point prevents execution, including a one-time approval in transit.
    /// Revocation afterward does not stop already-admitted commands; stop does.
    pub fn admit_at_epoch(&self, epoch: u64) -> io::Result<OwnedSemaphorePermit> {
        let state = self.state.lock().map_err(lock_error)?;
        if state.epoch != epoch {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "command authorization was revoked; request it again",
            ));
        }
        self.admit()
    }

    /// Pending approvals should not consume a running slot.
    pub fn admit(&self) -> io::Result<OwnedSemaphorePermit> {
        self.slots.clone().try_acquire_owned().map_err(|_| io::Error::new(
            io::ErrorKind::WouldBlock,
            "this conversation already has 8 active commands; inspect or stop one before starting another",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, access: CommandAccess) -> PendingApproval {
        PendingApproval {
            id: id.into(),
            task_title: None,
            review_mode: ApprovalMode::Ask,
            epoch: 0,
            command: "test command".into(),
            cwd: "/project".into(),
            execution_mode: "sandboxed".into(),
            network_access: NetworkAccess::Allowed,
            access,
        }
    }

    fn waiting(authorization: Authorization) -> oneshot::Receiver<Option<u64>> {
        match authorization {
            Authorization::Pending(receiver) => receiver,
            Authorization::Ready(_) => panic!("network access requires host authorization"),
        }
    }

    #[test]
    fn host_execution_requires_current_full_mode_and_does_not_widen_narrowed_tasks() {
        let permissions = CommandPermissions::default();
        let owner = Path::new("/state/root");
        let info = request("host", CommandAccess::WorkspaceWrite);
        assert!(
            permissions
                .request_for_task(owner, info.clone(), true)
                .is_err()
        );
        permissions.set_mode(ApprovalMode::FullAccess).unwrap();
        let Authorization::Ready(epoch) = permissions
            .request_for_task(owner, info.clone(), true)
            .unwrap()
        else {
            panic!("explicit host authority should not ask again");
        };
        let _waiting = waiting(
            permissions
                .request_for_task(owner, request("narrowed", CommandAccess::ReadOnly), false)
                .unwrap(),
        );
        assert_eq!(
            permissions.pending(owner).unwrap()[0].review_mode,
            ApprovalMode::Ask
        );
        permissions.set_mode(ApprovalMode::Ask).unwrap();
        assert!(permissions.admit_at_epoch(epoch).is_err());
        assert!(permissions.request_for_task(owner, info, true).is_err());
        assert!(permissions.pending(owner).unwrap().is_empty());
    }

    #[tokio::test]
    async fn session_approval_atomically_releases_covered_parent_and_child_requests() {
        let permissions = CommandPermissions::default();
        let parent = Path::new("/state/parent");
        let child = Path::new("/state/child");
        let first = waiting(
            permissions
                .request(parent, request("first", CommandAccess::ReadOnly))
                .unwrap(),
        );
        let second = waiting(
            permissions
                .request(child, request("second", CommandAccess::ReadOnly))
                .unwrap(),
        );
        let mut broader = waiting(
            permissions
                .request(child, request("broader", CommandAccess::WorkspaceWrite))
                .unwrap(),
        );
        permissions
            .approve(parent, "first", true, CommandApprovalScope::Session, None)
            .unwrap();
        assert_eq!(first.await.unwrap(), Some(0));
        assert_eq!(second.await.unwrap(), Some(0));
        assert!(permissions.pending(parent).unwrap().is_empty());
        assert_eq!(permissions.pending(child).unwrap()[0].id, "broader");
        assert!(matches!(
            broader.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            permissions
                .request(child, request("later", CommandAccess::ReadOnly))
                .unwrap(),
            Authorization::Ready(0)
        ));
        permissions.revoke().unwrap();
        assert!(broader.await.is_err());
    }

    #[tokio::test]
    async fn revoke_invalidates_pending_and_already_delivered_unadmitted_approvals() {
        let permissions = CommandPermissions::default();
        let owner = Path::new("/state/root");
        let in_transit = waiting(
            permissions
                .request(owner, request("approved", CommandAccess::ReadOnly))
                .unwrap(),
        );
        permissions
            .approve(owner, "approved", true, CommandApprovalScope::Once, Some(0))
            .unwrap();
        let old_epoch = in_transit.await.unwrap().unwrap();
        let still_waiting = waiting(
            permissions
                .request(owner, request("pending", CommandAccess::ReadOnly))
                .unwrap(),
        );
        permissions.revoke().unwrap();
        assert!(still_waiting.await.is_err());
        assert!(permissions.admit_at_epoch(old_epoch).is_err());
        assert!(
            permissions
                .approve(
                    owner,
                    "pending",
                    true,
                    CommandApprovalScope::Session,
                    Some(0)
                )
                .is_err()
        );
        assert!(permissions.grants().unwrap().is_empty());
        let current = waiting(
            permissions
                .request(owner, request("new", CommandAccess::ReadOnly))
                .unwrap(),
        );
        assert!(
            permissions
                .approve(owner, "new", true, CommandApprovalScope::Once, Some(0))
                .is_err()
        );
        assert_eq!(permissions.pending(owner).unwrap()[0].epoch, 1);
        permissions
            .approve(owner, "new", true, CommandApprovalScope::Once, Some(1))
            .unwrap();
        assert_eq!(current.await.unwrap(), Some(1));
    }

    #[tokio::test]
    async fn reviewer_mode_never_grants_network_and_cannot_authorize_cancelled_requests() {
        let permissions = CommandPermissions::default();
        let owner = Path::new("/state/root");
        for mode in [ApprovalMode::Ask, ApprovalMode::AutoReview] {
            permissions.set_mode(mode).unwrap();
            let mut offline = request("offline", CommandAccess::WorkspaceWrite);
            offline.network_access = NetworkAccess::Denied;
            assert!(matches!(
                permissions.request(owner, offline).unwrap(),
                Authorization::Ready(_)
            ));
            let receiver = waiting(
                permissions
                    .request(owner, request("network", CommandAccess::ReadOnly))
                    .unwrap(),
            );
            let pending = permissions.pending(owner).unwrap().pop().unwrap();
            assert_eq!(pending.review_mode, mode);
            drop(receiver);
            assert!(
                permissions
                    .approve(
                        owner,
                        "network",
                        true,
                        CommandApprovalScope::Session,
                        Some(pending.epoch)
                    )
                    .is_err()
            );
            assert!(permissions.grants().unwrap().is_empty());
        }
        let receiver = waiting(
            permissions
                .request(owner, request("switch", CommandAccess::ReadOnly))
                .unwrap(),
        );
        permissions.set_mode(ApprovalMode::Ask).unwrap();
        assert!(receiver.await.is_err());
        assert!(permissions.pending(owner).unwrap().is_empty());
    }

    #[tokio::test]
    async fn persistence_happens_only_for_current_live_request_and_before_release() {
        let permissions = CommandPermissions::default();
        let owner = Path::new("/state/root");
        let mut receiver = waiting(
            permissions
                .request(owner, request("one", CommandAccess::ReadOnly))
                .unwrap(),
        );
        let mut saved = false;
        assert!(
            permissions
                .approve_with(
                    owner,
                    "one",
                    true,
                    CommandApprovalScope::Session,
                    Some(0),
                    || {
                        assert!(matches!(
                            receiver.try_recv(),
                            Err(oneshot::error::TryRecvError::Empty)
                        ));
                        Err(io::Error::other("storage unavailable"))
                    }
                )
                .is_err()
        );
        assert_eq!(permissions.pending(owner).unwrap().len(), 1);
        assert!(permissions.grants().unwrap().is_empty());
        permissions
            .approve_with(
                owner,
                "one",
                true,
                CommandApprovalScope::Session,
                Some(0),
                || {
                    assert!(matches!(
                        receiver.try_recv(),
                        Err(oneshot::error::TryRecvError::Empty)
                    ));
                    saved = true;
                    Ok(())
                },
            )
            .unwrap();
        assert!(saved);
        assert_eq!(receiver.await.unwrap(), Some(0));
        permissions.revoke().unwrap();
        let cancelled = waiting(
            permissions
                .request(owner, request("cancel", CommandAccess::ReadOnly))
                .unwrap(),
        );
        permissions.cancel_pending(owner).unwrap();
        assert!(cancelled.await.is_err());
        assert!(
            permissions
                .approve_with(
                    owner,
                    "cancel",
                    true,
                    CommandApprovalScope::Session,
                    Some(1),
                    || { panic!("cancelled request must not persist a grant") }
                )
                .is_err()
        );
    }

    #[test]
    fn grants_never_expand_by_combining_independent_capabilities() {
        let permissions = CommandPermissions::default();
        assert!(
            !permissions
                .allows(CommandAccess::ReadOnly, NetworkAccess::Denied)
                .unwrap()
        );
        permissions
            .grant(CommandAccess::WorkspaceWrite, NetworkAccess::Denied)
            .unwrap();
        permissions
            .grant(CommandAccess::ReadOnly, NetworkAccess::Allowed)
            .unwrap();
        assert!(
            permissions
                .allows(CommandAccess::ReadOnly, NetworkAccess::Denied)
                .unwrap()
        );
        assert!(
            permissions
                .allows(CommandAccess::WorkspaceWrite, NetworkAccess::Denied)
                .unwrap()
        );
        assert!(
            permissions
                .allows(CommandAccess::ReadOnly, NetworkAccess::Allowed)
                .unwrap()
        );
        assert!(
            !permissions
                .allows(CommandAccess::WorkspaceWrite, NetworkAccess::Allowed)
                .unwrap()
        );
        assert_eq!(permissions.grants().unwrap().len(), 2);
        permissions
            .grant(CommandAccess::WorkspaceWrite, NetworkAccess::Allowed)
            .unwrap();
        assert!(
            permissions
                .allows(CommandAccess::WorkspaceWrite, NetworkAccess::Allowed)
                .unwrap()
        );
        assert_eq!(permissions.grants().unwrap().len(), 1);
        permissions.revoke().unwrap();
        assert!(permissions.grants().unwrap().is_empty());
        assert!(
            !permissions
                .allows(CommandAccess::ReadOnly, NetworkAccess::Denied)
                .unwrap()
        );
    }

    #[test]
    fn each_conversation_has_separate_grants_and_bounded_running_slots() {
        let first = CommandPermissions::default();
        let second = CommandPermissions::default();
        first
            .grant(CommandAccess::ReadOnly, NetworkAccess::Denied)
            .unwrap();
        assert!(
            !second
                .allows(CommandAccess::ReadOnly, NetworkAccess::Denied)
                .unwrap()
        );
        let mut slots: Vec<_> = (0..MAX_CONCURRENT_COMMANDS)
            .map(|_| first.admit().unwrap())
            .collect();
        assert_eq!(first.admit().unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert!(second.admit().is_ok());
        slots.pop();
        assert!(first.admit().is_ok());
    }
}
