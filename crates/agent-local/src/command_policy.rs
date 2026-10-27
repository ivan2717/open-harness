//! User-controlled, in-memory command grants. Model requests never create grants.
//! Each grant is one capability pair; independent approvals cannot be combined
//! to obtain a broader pair that the user never approved.
use crate::{NetworkAccess, lock_error};
use serde::{Deserialize, Serialize};
use std::{
    io,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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

/// Scoped by the host to one root conversation. Never serialized into a saved
/// run: reopening the application starts with no reusable authorization.
pub(crate) struct CommandPermissions {
    grants: Mutex<Vec<CommandGrant>>,
    slots: Arc<Semaphore>,
}

impl Default for CommandPermissions {
    fn default() -> Self {
        Self {
            grants: Mutex::new(Vec::new()),
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT_COMMANDS)),
        }
    }
}

impl CommandPermissions {
    pub fn allows(&self, access: CommandAccess, network_access: NetworkAccess) -> io::Result<bool> {
        let request = CommandGrant {
            access,
            network_access,
        };
        Ok(self
            .grants
            .lock()
            .map_err(lock_error)?
            .iter()
            .any(|grant| grant.covers(request)))
    }

    /// Called only after a host/UI approval with explicit Session scope.
    pub fn grant(&self, access: CommandAccess, network_access: NetworkAccess) -> io::Result<()> {
        let request = CommandGrant {
            access,
            network_access,
        };
        let mut grants = self.grants.lock().map_err(lock_error)?;
        if !grants.iter().any(|grant| grant.covers(request)) {
            grants.retain(|grant| !request.covers(*grant));
            grants.push(request);
        }
        Ok(())
    }

    pub fn revoke(&self) -> io::Result<()> {
        self.grants.lock().map_err(lock_error)?.clear();
        Ok(())
    }

    pub fn grants(&self) -> io::Result<Vec<CommandGrant>> {
        Ok(self.grants.lock().map_err(lock_error)?.clone())
    }

    /// Pending approvals should not consume a running slot. Keep this permit
    /// alive until process teardown has completed, including after tool yield.
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
