//! Host-owned extensions. Model-visible tools never accept executable server configuration.
mod approvals;
mod control;
#[cfg(test)]
mod control_integration_tests;
#[cfg(test)]
mod plugin_tests;
mod plugins;
#[cfg(test)]
mod tests;
mod tools;

use agent_mcp::{McpCatalog, McpClient, McpServerConfig};
use agent_plugins::{PluginInfo, PluginStore};
use agent_skills::{
    SkillCatalog, SkillCatalogConfig, SkillDiagnostic, SkillInfo, SkillRoot, SkillScope,
};
pub use approvals::ExtensionApproval;
pub use control::{ControlAccess, ControlScope};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
pub use tools::ExtensionTools;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionConfig {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub disabled_skill_ids: Vec<String>,
    #[serde(default = "enabled_by_default")]
    pub llm_wiki_enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

impl Default for ExtensionConfig {
    fn default() -> Self {
        Self {
            revision: 0,
            mcp_servers: Vec::new(),
            disabled_skill_ids: Vec::new(),
            llm_wiki_enabled: true,
        }
    }
}

#[derive(Serialize)]
pub struct BuiltinPluginInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub enabled: bool,
}

#[derive(Serialize)]
pub struct ExtensionSettings {
    pub config: ExtensionConfig,
    pub skills_directory: String,
    pub skills: Vec<SkillInfo>,
    pub diagnostics: Vec<SkillDiagnostic>,
    pub plugins: Vec<PluginInfo>,
    pub builtin_plugins: Vec<BuiltinPluginInfo>,
}

#[derive(Serialize)]
pub struct SkillCatalogView {
    pub skills: Vec<SkillInfo>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

#[derive(Serialize)]
pub struct RunExtensions {
    pub skills: Vec<SkillInfo>,
    pub diagnostics: Vec<SkillDiagnostic>,
    pub approvals: Vec<ExtensionApproval>,
    pub controls: Vec<ControlAccess>,
}

pub struct ExtensionHost {
    closed: AtomicBool,
    directory: PathBuf,
    bundled_plugins: Option<PathBuf>,
    config: RwLock<ExtensionConfig>,
    plugins: RwLock<PluginStore>,
    catalogs: RwLock<HashMap<Option<PathBuf>, Arc<SkillCatalog>>>,
    clients: RwLock<HashMap<(String, String), CachedClient>>,
    pub approvals: Arc<approvals::ApprovalQueue>,
    controls: Mutex<control::ControlRegistry>,
    pub(crate) memory: crate::memory::MemoryStore,
    pub(crate) memory_errors: RwLock<HashMap<String, String>>,
}
struct CachedClient {
    client: McpClient,
    last_used: Instant,
}

impl ExtensionHost {
    pub fn open(directory: &Path) -> io::Result<Arc<Self>> {
        Self::open_with_bundled(
            directory,
            std::env::var_os("OPENHARNESS_BUNDLED_PLUGINS_DIR").map(PathBuf::from),
        )
    }

    fn open_with_bundled(directory: &Path, bundled: Option<PathBuf>) -> io::Result<Arc<Self>> {
        fs::create_dir_all(directory.join("skills"))?;
        let directory = fs::canonicalize(directory)?;
        let path = directory.join("extensions.json");
        let config: ExtensionConfig = if path.exists() {
            if fs::metadata(&path)?.len() > 256 * 1024 {
                return Err(io::Error::other("extension configuration exceeds 256 KiB"));
            }
            serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?
        } else {
            ExtensionConfig::default()
        };
        validate(&config)?;
        let plugins = PluginStore::open(&directory.join("plugins"))?;
        let memory = crate::memory::MemoryStore::new(directory.join("memories"));
        Ok(Arc::new(Self {
            closed: AtomicBool::new(false),
            directory,
            bundled_plugins: bundled.and_then(|root| fs::canonicalize(root).ok()),
            config: RwLock::new(config),
            plugins: RwLock::new(plugins),
            catalogs: RwLock::default(),
            clients: RwLock::default(),
            approvals: Arc::default(),
            controls: Mutex::default(),
            memory,
            memory_errors: RwLock::default(),
        }))
    }

    pub fn catalog(&self, project: Option<&Path>) -> Arc<SkillCatalog> {
        let cached = self
            .catalogs
            .read()
            .unwrap()
            .get(&project.map(Path::to_path_buf))
            .cloned();
        cached.unwrap_or_else(|| self.refresh_catalog(project))
    }

    pub fn refresh_catalog(&self, project: Option<&Path>) -> Arc<SkillCatalog> {
        let settings = self.config.read().unwrap();
        let mut config = SkillCatalogConfig::for_workspace(self.directory.join("skills"), project);
        config.disabled_skill_ids = settings.disabled_skill_ids.clone();
        config
            .roots
            .extend(self.plugins.read().unwrap().skill_roots().into_iter().map(
                |(namespace, path)| SkillRoot {
                    path,
                    scope: SkillScope::Additional,
                    namespace: Some(namespace),
                },
            ));
        let catalog = Arc::new(SkillCatalog::discover(config));
        let mut catalogs = self.catalogs.write().unwrap();
        // Bound memory retained for inactive project catalogs.
        if catalogs.len() >= 32 {
            catalogs.clear();
        }
        catalogs.insert(project.map(Path::to_path_buf), catalog.clone());
        catalog
    }

    pub fn settings(&self) -> ExtensionSettings {
        let catalog = self.catalog(None);
        let config = self.config.read().unwrap().clone();
        let memory_enabled = config.llm_wiki_enabled;
        ExtensionSettings {
            config,
            skills_directory: self.directory.join("skills").to_string_lossy().into_owned(),
            skills: catalog.skills().to_vec(),
            diagnostics: catalog.diagnostics().to_vec(),
            plugins: self.plugins.read().unwrap().list(),
            builtin_plugins: vec![BuiltinPluginInfo {
                id: crate::memory::PLUGIN_ID,
                name: "LLM Wiki",
                description: "为长会话保存决策原因、失败经验和结果依据，按需回查。仅用于当前会话；自动整理使用当前模型，会产生额外用量。",
                enabled: memory_enabled,
            }],
        }
    }

    pub(crate) fn memory_enabled(&self) -> bool {
        !self.closed.load(Ordering::Acquire) && self.config.read().unwrap().llm_wiki_enabled
    }

    pub(crate) fn memory_generation(&self) -> Option<u64> {
        let config = self.config.read().unwrap();
        (config.llm_wiki_enabled && !self.closed.load(Ordering::Acquire)).then_some(config.revision)
    }

    // Serialize with disabling the plugin, so an in-flight compilation cannot
    // publish a new revision after the user has turned memory off.
    pub(crate) fn commit_memory(
        &self,
        generation: u64,
        run: &str,
        events: &[agent_core::agent::RunEventEnvelope],
        through: u64,
        cursor: agent_memory::WikiSourceCursor,
        update: agent_memory::WikiUpdate,
    ) -> io::Result<()> {
        let config = self.config.read().unwrap();
        if config.llm_wiki_enabled
            && config.revision == generation
            && !self.closed.load(Ordering::Acquire)
        {
            self.memory
                .apply_progress(run, events, through, cursor, update)?;
        }
        Ok(())
    }

    pub(crate) async fn memory_events(
        &self,
        run: &str,
    ) -> io::Result<Vec<agent_core::agent::RunEventEnvelope>> {
        use agent_runtime::EventStore;
        let stored = agent_runtime::JsonlEventStore::new(self.directory.join("events"))
            .load_for_runtime(run)
            .await
            .map_err(io::Error::other)?;
        Ok(stored.map_or_else(Vec::new, |stored| stored.events))
    }

    pub fn save(&self, mut config: ExtensionConfig) -> io::Result<ExtensionSettings> {
        validate(&config)?;
        let mut current = self.config.write().unwrap();
        if config.revision != current.revision {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "扩展配置已更新，请刷新后重试",
            ));
        }
        let plugin_ids: HashSet<_> = self
            .plugins
            .read()
            .unwrap()
            .list()
            .into_iter()
            .flat_map(|plugin| plugin.mcp_servers.into_iter().map(|server| server.id))
            .collect();
        if config
            .mcp_servers
            .iter()
            .any(|server| plugin_ids.contains(&server.id))
        {
            return Err(io::Error::other(
                "MCP 服务标识与已安装插件冲突，请选择其他标识",
            ));
        }
        config.revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("extension revision limit reached"))?;
        validate(&config)?;
        let changed: HashSet<_> = current
            .mcp_servers
            .iter()
            .filter(|old| {
                !config.mcp_servers.iter().any(|new| {
                    new.id == old.id
                        && serde_json::to_value(new).ok() == serde_json::to_value(old).ok()
                })
            })
            .map(|server| server.id.clone())
            .collect();
        crate::snapshot::write_json(&self.directory.join("extensions.json"), &config)?;
        *current = config;
        self.catalogs.write().unwrap().clear();
        self.clients.write().unwrap().retain(|(_, id), cached| {
            if changed.contains(id) {
                cached.client.revoke();
                false
            } else {
                true
            }
        });
        // An approval refers to one concrete configuration. Changing configuration
        // invalidates affected requests instead of silently authorizing another server.
        self.approvals.invalidate_servers(&changed);
        self.controls.lock().unwrap().invalidate_servers(&changed);
        drop(current);
        Ok(self.settings())
    }

    pub fn servers(&self) -> Vec<McpServerConfig> {
        let config = self.config.read().unwrap();
        self.effective_servers(&config)
    }

    // Lock ordering: config -> plugins -> catalogs/clients. Holding config while
    // resolving a client prevents a plugin mutation from racing client creation.
    fn effective_servers(&self, config: &ExtensionConfig) -> Vec<McpServerConfig> {
        config
            .mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .cloned()
            .chain(self.plugins.read().unwrap().servers())
            .collect()
    }

    pub fn server(&self, id: &str) -> io::Result<McpServerConfig> {
        self.servers()
            .into_iter()
            .find(|server| server.id == id)
            .ok_or_else(|| {
                io::Error::other("MCP server is missing or disabled; configure it in Settings")
            })
    }

    pub async fn test(&self, id: &str) -> io::Result<McpCatalog> {
        // Connection tests never share state with a conversation.
        let client = self.client("__connection-test", &self.server(id)?)?;
        client.discover().await.map_err(io::Error::other)
    }

    pub fn client(&self, run: &str, expected: &McpServerConfig) -> io::Result<McpClient> {
        self.client_with_epoch(run, expected, None)
    }

    fn client_with_epoch(
        &self,
        run: &str,
        expected: &McpServerConfig,
        epoch: Option<u64>,
    ) -> io::Result<McpClient> {
        let config = self.config.read().unwrap();
        let servers = self.effective_servers(&config);
        let server = servers
            .iter()
            .find(|server| server.id == expected.id && server.enabled)
            .ok_or_else(|| io::Error::other("MCP server is disabled"))?;
        if serde_json::to_value(server).map_err(io::Error::other)?
            != serde_json::to_value(expected).map_err(io::Error::other)?
        {
            return Err(io::Error::other(
                "MCP configuration changed; request approval again",
            ));
        }
        let controls = self.controls.lock().unwrap();
        if let Some(epoch) = epoch {
            controls.check_epoch(run, epoch)?;
        }
        let mut clients = self.clients.write().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::other("extension host is shutting down"));
        }
        let key = (run.to_owned(), server.id.clone());
        if let Some(cached) = clients.get_mut(&key) {
            cached.last_used = Instant::now();
            return Ok(cached.client.clone());
        }
        if clients.len() >= 128 {
            let oldest = clients
                .iter()
                .filter(|((run, server), cached)| {
                    cached.client.is_idle()
                        && !controls
                            .list(run)
                            .iter()
                            .any(|access| access.server_id == *server)
                })
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                clients.remove(&oldest);
            } else {
                return Err(io::Error::other(
                    "All 128 MCP sessions are active; wait for an operation to finish",
                ));
            }
        }
        let client = McpClient::new(server.clone()).map_err(io::Error::other)?;
        clients.insert(
            key,
            CachedClient {
                client: client.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(client)
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        self.approvals.clear();
        let clients: Vec<_> = self
            .clients
            .write()
            .unwrap()
            .drain()
            .map(|(_, cached)| cached.client)
            .collect();
        for client in &clients {
            client.revoke();
        }
        futures_util::future::join_all(clients.iter().map(|client| client.shutdown())).await;
    }

    pub fn revoke_run(&self, run: &str) {
        // Same order as authorization: control registry -> client cache.
        let mut controls = self.controls.lock().unwrap();
        controls.revoke_run(run);
        self.approvals.clear_run(run);
        self.clients.write().unwrap().retain(|(owner, _), cached| {
            if owner == run {
                cached.client.revoke();
                false
            } else {
                true
            }
        });
    }

    fn control_plugin(&self, server: &str) -> Option<String> {
        self.plugins
            .read()
            .unwrap()
            .list()
            .into_iter()
            .find(|plugin| {
                plugin.enabled
                    && matches!(
                        plugin.id.as_str(),
                        "browser-use" | "computer-use" | "chrome-use"
                    )
                    && self.is_bundled_control(plugin)
                    && plugin
                        .mcp_servers
                        .iter()
                        .any(|entry| entry.id == server && entry.enabled)
            })
            .map(|plugin| plugin.id)
    }

    fn is_bundled_control(&self, plugin: &PluginInfo) -> bool {
        if !matches!(
            plugin.id.as_str(),
            "browser-use" | "computer-use" | "chrome-use"
        ) {
            return false;
        }
        let Some(root) = &self.bundled_plugins else {
            return false;
        };
        let expected = root.join(&plugin.id);
        // Only the application's fixed package location may opt into scoped
        // control. A third-party manifest name is never a trust credential.
        expected.parent() == Some(root.as_path())
            && fs::canonicalize(&expected).ok().as_ref() == Some(&expected)
            && fs::canonicalize(&plugin.source_path).ok().as_ref() == Some(&expected)
    }

    fn release_failed_control(&self, run: &str, server: &str, grant_id: u64) {
        let mut controls = self.controls.lock().unwrap();
        // A different server may have advanced the run epoch while this
        // selection was pending. Roll back this exact grant, but never revoke
        // a replacement authorized after this operation was abandoned.
        if !controls.owns_grant(run, server, grant_id) {
            return;
        }
        // Revoke all outstanding handles before releasing the desktop lease.
        if let Some(cached) = self
            .clients
            .write()
            .unwrap()
            .remove(&(run.into(), server.into()))
        {
            cached.client.revoke();
        }
        controls.release_server(run, server);
    }

    pub fn run_state(&self, run: &str, project: Option<&Path>) -> RunExtensions {
        let catalog = self.catalog(project);
        RunExtensions {
            skills: catalog.skills().to_vec(),
            diagnostics: catalog.diagnostics().to_vec(),
            approvals: self.approvals.list(run),
            controls: self.controls.lock().unwrap().list(run),
        }
    }
}

fn validate(config: &ExtensionConfig) -> io::Result<()> {
    if serde_json::to_vec_pretty(config)
        .map_err(io::Error::other)?
        .len()
        > 256 * 1024
    {
        return Err(io::Error::other("extension configuration exceeds 256 KiB"));
    }
    if config.mcp_servers.len() > 32 || config.disabled_skill_ids.len() > 256 {
        return Err(io::Error::other(
            "最多配置 32 个 MCP 服务和 256 个停用的 Skill",
        ));
    }
    let mut ids = HashSet::new();
    for server in &config.mcp_servers {
        if !ids.insert(&server.id) {
            return Err(io::Error::other("MCP server identifiers must be unique"));
        }
        McpClient::new(server.clone()).map_err(io::Error::other)?;
    }
    if config
        .disabled_skill_ids
        .iter()
        .any(|id| id.is_empty() || id.chars().count() > 129)
    {
        return Err(io::Error::other("Invalid disabled skill identifier"));
    }
    Ok(())
}
