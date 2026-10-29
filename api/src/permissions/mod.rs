//! User-owned policy and review records. Skills and model tool arguments cannot
//! mutate this state; all policy changes come through the authenticated host API.
use super::*;
use agent_local::{ApprovalMode, CommandApprovalScope, CommandGrant, PermissionMode};
use serde::{Deserialize, Serialize};
mod records;
mod reviewer;
pub use records::ApprovalReview;
pub(crate) use records::PermissionRecords;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandDecisionScope {
    #[default]
    Once,
    Session,
    Always,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePermissions {
    pub approval_mode: ApprovalMode,
    /// Optional advanced project setting; the primary UI only selects a preset.
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Serialize)]
pub struct RunPermissions {
    pub root_run_id: String,
    pub approval_mode: ApprovalMode,
    pub permission_mode: PermissionMode,
    pub editable: bool,
    pub persistent_command_grants: Vec<CommandGrant>,
    pub reviews: Vec<ApprovalReview>,
}

impl AppState {
    pub(super) fn clear_command_permissions(
        &self,
        root: &str,
        tools: Option<&agent_local::LocalTools>,
    ) -> io::Result<()> {
        let _gate = self
            .inner
            .permission_gate
            .lock()
            .map_err(|_| io::Error::other("permission gate poisoned"))?;
        // Order local permissions before records everywhere. In particular,
        // persistent approval callbacks acquire records under the local lock.
        if let Some(tools) = tools {
            tools.revoke_command_grants()?;
        }
        self.inner.permission_records.change(root, |records| {
            records.grants.clear();
            Ok(())
        })
    }
    pub(super) fn permission_root(&self, id: &str) -> Result<String, ServiceError> {
        let summary = self.get(id)?.summary;
        Ok(summary.delegation.map_or(summary.id, |d| d.root_run_id))
    }

    fn permission_family(&self, root: &str) -> Vec<Arc<RunEntry>> {
        self.inner
            .runs
            .read()
            .unwrap()
            .values()
            .filter(|entry| {
                let summary = entry.snapshot.get().summary;
                summary.id == root || summary.delegation.is_some_and(|d| d.root_run_id == root)
            })
            .cloned()
            .collect()
    }

    fn permissions_busy(entry: &RunEntry) -> bool {
        let summary = entry.snapshot.get().summary;
        entry.background.load(Ordering::Acquire)
            || summary.status == RunStatus::Running
            || summary.compacting
            || entry.runtime.is_executing(&summary.id)
            || entry
                .workspace
                .as_ref()
                .is_some_and(|w| w.commands_running())
    }

    pub fn run_permissions(&self, id: &str) -> Result<RunPermissions, ServiceError> {
        let root = self.permission_root(id)?;
        let summary = self.get(&root)?.summary;
        let records = self.inner.permission_records.get(&root)?;
        Ok(RunPermissions {
            editable: id == root
                && self
                    .permission_family(&root)
                    .iter()
                    .all(|entry| !Self::permissions_busy(entry)),
            approval_mode: summary.execution_policy.approval_mode,
            permission_mode: summary.permission_mode,
            persistent_command_grants: if records.workspace.as_deref()
                == summary.working_directory()
            {
                records.grants
            } else {
                Vec::new()
            },
            reviews: records.reviews,
            root_run_id: root,
        })
    }

    pub async fn update_permissions(
        &self,
        id: &str,
        input: UpdatePermissions,
    ) -> Result<RunPermissions, ServiceError> {
        let _gate = self.inner.model_gate.lock().await;
        self.ensure_accepting_work()?;
        let root = self.permission_root(id)?;
        if id != root {
            return Err(ServiceError::Conflict(
                "请在主任务中修改权限，协作任务继承主任务设置。".into(),
            ));
        }
        let mut entries = self.permission_family(&root);
        // Publish the authoritative root before rebuilding derived children.
        entries.sort_by_key(|entry| entry.snapshot.get().summary.id != root);
        if entries.iter().any(|entry| Self::permissions_busy(entry)) {
            return Err(ServiceError::Conflict(
                "请先停止当前任务、协作任务和命令，再修改权限。".into(),
            ));
        }
        let root_entry = self.entry(&root)?;
        let _operation = root_entry.operations.lock().await;
        let current = root_entry.snapshot.get().summary;
        let permission_mode = if input.approval_mode == ApprovalMode::FullAccess {
            PermissionMode::WorkspaceWrite
        } else {
            input.permission_mode.unwrap_or(current.permission_mode)
        };
        if current.permission_mode == permission_mode
            && current.execution_policy.approval_mode == input.approval_mode
        {
            return self.run_permissions(id);
        }
        if root_entry.snapshot.get().summary.workspace_path.is_none()
            && permission_mode != PermissionMode::WorkspaceWrite
        {
            return Err(ServiceError::BadRequest(
                "会话文件用于创建临时脚本和结果，保持可写；请选择项目以设置只读权限。".into(),
            ));
        }
        // Never leave reusable grants from the former policy active after a
        // boundary change. Revocation is safe even if rebuilding later fails.
        self.clear_command_permissions(&root, root_entry.workspace.as_ref())?;
        self.inner.extensions.revoke_run(&root);
        for entry in entries {
            let mut detail = entry.snapshot.get();
            detail.summary.execution_policy.approval_mode = input.approval_mode;
            let narrowed = detail
                .summary
                .delegation
                .as_ref()
                .and_then(|d| d.write_paths.as_ref())
                .is_some_and(Vec::is_empty);
            detail.summary.permission_mode = if narrowed {
                PermissionMode::ReadOnly
            } else {
                permission_mode
            };
            entry
                .runtime
                .context_manager()
                .stop_memory_background()
                .await;
            let prepared = (|| -> Result<Arc<RunEntry>, ServiceError> {
                let restored = self.make_entry_with_snapshot(
                    detail.clone(),
                    0,
                    Some(entry.snapshot.clone()),
                    None,
                    true,
                )?;
                if input.approval_mode == ApprovalMode::FullAccess {
                    Self::require_workspace_available(&restored)?;
                }
                entry.snapshot.update(|current| {
                    current.summary.permission_mode = detail.summary.permission_mode;
                    current.summary.execution_policy.approval_mode = input.approval_mode;
                })?;
                Ok(restored)
            })();
            let restored = match prepared {
                Ok(restored) => restored,
                Err(error) => {
                    if detail.summary.id == root {
                        // Building tools may update the shared policy. A failed
                        // root commit must never leave Full Access active while
                        // the saved/UI policy still says Ask or AutoReview.
                        if let Some(tools) = &root_entry.workspace {
                            tools.set_approval_mode(current.execution_policy.approval_mode)?;
                        }
                        self.inner
                            .extensions
                            .set_approval_mode(&root, current.execution_policy.approval_mode);
                    }
                    return Err(error);
                }
            };
            self.inner
                .runs
                .write()
                .unwrap()
                .insert(detail.summary.id, restored);
        }
        self.run_permissions(id)
    }

    pub async fn decide_command(
        &self,
        id: &str,
        approval: &str,
        allow: bool,
        scope: CommandDecisionScope,
    ) -> Result<agent_local::WorkspaceReview, ServiceError> {
        if !matches!(scope, CommandDecisionScope::Always) || !allow {
            return self
                .approve_command(
                    id,
                    approval,
                    allow,
                    if matches!(scope, CommandDecisionScope::Session) {
                        CommandApprovalScope::Session
                    } else {
                        CommandApprovalScope::Once
                    },
                )
                .await;
        }
        let root = self.permission_root(id)?;
        let tools = self.workspace_for_command(id, approval, true)?;
        let review = tools.review()?;
        let pending = review
            .approvals
            .iter()
            .find(|p| p.id == approval)
            .ok_or(ServiceError::NotFound)?;
        let grant = CommandGrant {
            access: pending.access,
            network_access: pending.network_access,
        };
        {
            let _gate = self
                .inner
                .permission_gate
                .lock()
                .map_err(|_| io::Error::other("permission gate poisoned"))?;
            tools.approve_if_current_with(
                approval,
                true,
                CommandApprovalScope::Session,
                pending.epoch,
                || {
                    self.inner.permission_records.change(&root, |records| {
                        records.workspace = Some(review.root.clone());
                        if !records.grants.contains(&grant) {
                            records.grants.push(grant);
                        }
                        Ok(())
                    })
                },
            )?;
        }
        self.workspace_review(id).await
    }

    pub async fn revoke_permissions(
        &self,
        id: &str,
        stop: bool,
    ) -> Result<RunPermissions, ServiceError> {
        let root = self.permission_root(id)?;
        if self.get(&root)?.summary.execution_policy.approval_mode == ApprovalMode::FullAccess {
            if !stop {
                return Err(ServiceError::BadRequest(
                    "完全访问权限仍在生效，请先切换审批方式，或选择收回权限并停止任务。".into(),
                ));
            }
            self.cancel(&root).await?;
            return self
                .update_permissions(
                    &root,
                    UpdatePermissions {
                        approval_mode: ApprovalMode::Ask,
                        permission_mode: None,
                    },
                )
                .await;
        }
        let entry = self.entry(&root)?;
        self.clear_command_permissions(&root, entry.workspace.as_ref())?;
        self.inner.extensions.revoke_run(&root);
        if stop {
            self.cancel(&root).await?;
        }
        self.run_permissions(id)
    }
}

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod tests;
