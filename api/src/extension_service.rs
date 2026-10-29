use super::*;
use crate::extensions::{ExtensionConfig, ExtensionSettings, RunExtensions, SkillCatalogView};
use std::path::Path;

impl AppState {
    pub fn control_permissions(
        &self,
        run: Option<&str>,
    ) -> Vec<crate::extensions::ControlPermission> {
        self.inner.extensions.permissions(run)
    }

    pub fn revoke_target_permission(
        &self,
        run: Option<&str>,
        permission: &crate::extensions::ControlPermission,
    ) -> Result<Vec<crate::extensions::ControlPermission>, ServiceError> {
        if let Some(id) = run {
            self.get(id)?;
        }
        self.inner.extensions.revoke_permission(run, permission)?;
        Ok(self.inner.extensions.permissions(run))
    }
    pub async fn run_memory(&self, id: &str) -> Result<crate::memory::RunMemoryView, ServiceError> {
        self.get(id)?;
        let host = &self.inner.extensions;
        let events = host.memory_events(id).await?;
        Ok(crate::memory::RunMemoryView::load(host, id, &events))
    }
    pub fn extension_settings(&self) -> ExtensionSettings {
        self.inner.extensions.refresh_catalog(None);
        self.inner.extensions.settings()
    }
    pub fn skill_catalog(
        &self,
        workspace_path: Option<&str>,
    ) -> Result<SkillCatalogView, ServiceError> {
        let project = workspace_path
            .map(|path| self.validate_workspace_path(path))
            .transpose()?;
        let catalog = self.inner.extensions.refresh_catalog(project.as_deref());
        Ok(SkillCatalogView {
            skills: catalog.skills().to_vec(),
            diagnostics: catalog.diagnostics().to_vec(),
        })
    }
    pub fn refresh_run_skills(&self, id: &str) -> Result<RunExtensions, ServiceError> {
        let detail = self.get(id)?;
        self.inner
            .extensions
            .refresh_catalog(detail.summary.workspace_path.as_deref().map(Path::new));
        self.run_extensions(id)
    }
    pub fn save_extensions(
        &self,
        config: ExtensionConfig,
    ) -> Result<ExtensionSettings, ServiceError> {
        let stop_memory = !config.llm_wiki_enabled;
        let settings = self.inner.extensions.save(config).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                ServiceError::Conflict(error.to_string())
            } else {
                ServiceError::BadRequest(error.to_string())
            }
        })?;
        if stop_memory {
            for entry in self.inner.runs.read().unwrap().values() {
                entry.runtime.context_manager().abort_memory_background();
            }
        }
        Ok(settings)
    }
    pub async fn test_mcp(&self, id: &str) -> Result<agent_mcp::McpCatalog, ServiceError> {
        self.ensure_accepting_work()?;
        self.inner
            .extensions
            .test(id)
            .await
            .map_err(|error| ServiceError::BadRequest(error.to_string()))
    }
    pub fn run_extensions(&self, id: &str) -> Result<RunExtensions, ServiceError> {
        let detail = self.get(id)?;
        let mut state = self
            .inner
            .extensions
            .run_state(id, detail.summary.workspace_path.as_deref().map(Path::new));
        if detail.summary.delegation.is_none() {
            for (child, title) in self.extension_children(id) {
                let child_state = self.inner.extensions.run_state(&child, None);
                state
                    .approvals
                    .extend(child_state.approvals.into_iter().map(|mut approval| {
                        approval.source_run_id = Some(child.clone());
                        approval.task_title = Some(title.clone());
                        approval
                    }));
                state
                    .controls
                    .extend(child_state.controls.into_iter().map(|mut control| {
                        control.source_run_id = Some(child.clone());
                        control.task_title = Some(title.clone());
                        control
                    }));
            }
        }
        Ok(state)
    }

    fn extension_children(&self, root: &str) -> Vec<(String, String)> {
        let mut children: Vec<_> = self
            .inner
            .runs
            .read()
            .unwrap()
            .values()
            .filter_map(|entry| {
                let summary = entry.snapshot.get().summary;
                summary
                    .delegation
                    .as_ref()
                    .filter(|d| d.root_run_id == root)
                    .map(|_| (summary.id.clone(), summary.title.clone()))
            })
            .collect();
        children.sort();
        children
    }
    pub fn revoke_control_permission(
        &self,
        server: &str,
    ) -> Result<ExtensionSettings, ServiceError> {
        self.inner
            .extensions
            .revoke_control_permission(server)
            .map_err(|error| ServiceError::BadRequest(error.to_string()))?;
        Ok(self.extension_settings())
    }
    pub fn decide_extension(
        &self,
        id: &str,
        approval: &str,
        decision: crate::extensions::ApprovalDecision,
    ) -> Result<RunExtensions, ServiceError> {
        self.decide_extension_with_extent(
            id,
            approval,
            decision,
            crate::extensions::ApprovalExtent::Target,
        )
    }
    pub fn decide_extension_with_extent(
        &self,
        id: &str,
        approval: &str,
        decision: crate::extensions::ApprovalDecision,
        extent: crate::extensions::ApprovalExtent,
    ) -> Result<RunExtensions, ServiceError> {
        let state = self.run_extensions(id)?;
        let pending = state
            .approvals
            .iter()
            .find(|request| request.id == approval)
            .ok_or_else(|| ServiceError::Conflict("审批已失效，请刷新会话".into()))?;
        let owner = pending.source_run_id.as_deref().unwrap_or(id);
        self.inner
            .extensions
            .approvals
            .decide_permission_with_extent(owner, approval, decision, extent)
            .map_err(|error| ServiceError::Conflict(error.to_string()))?;
        self.run_extensions(id)
    }
    pub fn approve_extension(
        &self,
        id: &str,
        approval: &str,
        allow: bool,
    ) -> Result<RunExtensions, ServiceError> {
        self.approve_extension_scoped(id, approval, allow, false)
    }
    pub fn approve_extension_scoped(
        &self,
        id: &str,
        approval: &str,
        allow: bool,
        session: bool,
    ) -> Result<RunExtensions, ServiceError> {
        let state = self.run_extensions(id)?;
        let pending = state
            .approvals
            .iter()
            .find(|request| request.id == approval)
            .ok_or_else(|| ServiceError::Conflict("审批已失效，请刷新会话".into()))?;
        let owner = pending.source_run_id.as_deref().unwrap_or(id);
        self.inner
            .extensions
            .approvals
            .decide_scoped(owner, approval, allow, session)
            .map_err(|error| ServiceError::Conflict(error.to_string()))?;
        self.run_extensions(id)
    }
    pub async fn revoke_control(&self, id: &str) -> Result<RunExtensions, ServiceError> {
        let root = self.root_id(id)?;
        let detail = self.get(&root)?;
        self.inner.extensions.revoke_run(&root);
        if detail.summary.status == crate::contracts::RunStatus::Running {
            self.cancel(&root).await?;
        }
        self.run_extensions(id)
    }
}
