use super::*;
use crate::extensions::{ExtensionConfig, ExtensionSettings, RunExtensions};
use std::path::Path;

impl AppState {
    pub fn extension_settings(&self) -> ExtensionSettings {
        self.inner.extensions.refresh_catalog(None);
        self.inner.extensions.settings()
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
        self.inner.extensions.save(config).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                ServiceError::Conflict(error.to_string())
            } else {
                ServiceError::BadRequest(error.to_string())
            }
        })
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
        self.get(id)?;
        self.inner
            .extensions
            .approvals
            .decide(id, approval, allow)
            .map_err(|error| ServiceError::Conflict(error.to_string()))?;
        self.run_extensions(id)
    }
}
