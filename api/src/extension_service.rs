use super::*;
use crate::extensions::{ExtensionConfig, ExtensionSettings, RunExtensions, SkillCatalogView};
use std::path::Path;

impl AppState {
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
        Ok(self
            .inner
            .extensions
            .run_state(id, detail.summary.workspace_path.as_deref().map(Path::new)))
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
        self.get(id)?;
        self.inner
            .extensions
            .approvals
            .decide_scoped(id, approval, allow, session)
            .map_err(|error| ServiceError::Conflict(error.to_string()))?;
        self.run_extensions(id)
    }
    pub async fn revoke_control(&self, id: &str) -> Result<RunExtensions, ServiceError> {
        let detail = self.get(id)?;
        self.inner.extensions.revoke_run(id);
        if detail.summary.status == crate::contracts::RunStatus::Running {
            self.cancel(id).await?;
        }
        self.run_extensions(id)
    }
}
