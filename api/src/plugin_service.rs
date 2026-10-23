use super::*;
use crate::extensions::ExtensionSettings;

impl AppState {
    pub async fn install_plugin(
        &self,
        source_path: String,
        expected_id: Option<String>,
    ) -> Result<ExtensionSettings, ServiceError> {
        self.ensure_accepting_work()?;
        let source = PathBuf::from(source_path);
        if !source.is_absolute() {
            return Err(ServiceError::BadRequest("请选择插件的本地绝对路径".into()));
        }
        let extensions = self.inner.extensions.clone();
        tokio::task::spawn_blocking(move || {
            extensions.install_plugin(&source, expected_id.as_deref())
        })
        .await
        .map_err(|_| ServiceError::BadRequest("插件安装任务未完成".into()))?
        .map_err(|error| ServiceError::BadRequest(error.to_string()))
    }

    pub async fn set_plugin_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> Result<ExtensionSettings, ServiceError> {
        self.ensure_accepting_work()?;
        let stop_memory = id == crate::memory::PLUGIN_ID && !enabled;
        let extensions = self.inner.extensions.clone();
        let settings =
            tokio::task::spawn_blocking(move || extensions.set_plugin_enabled(&id, enabled))
                .await
                .map_err(|_| ServiceError::BadRequest("插件状态更新未完成".into()))?
                .map_err(|error| ServiceError::BadRequest(error.to_string()))?;
        if stop_memory {
            let entries: Vec<_> = self.inner.runs.read().unwrap().values().cloned().collect();
            for entry in entries {
                entry
                    .runtime
                    .context_manager()
                    .stop_memory_background()
                    .await;
            }
        }
        Ok(settings)
    }

    pub async fn uninstall_plugin(&self, id: String) -> Result<ExtensionSettings, ServiceError> {
        self.ensure_accepting_work()?;
        let extensions = self.inner.extensions.clone();
        tokio::task::spawn_blocking(move || extensions.uninstall_plugin(&id))
            .await
            .map_err(|_| ServiceError::BadRequest("插件卸载未完成".into()))?
            .map_err(|error| ServiceError::BadRequest(error.to_string()))
    }
}
