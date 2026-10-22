//! Host-owned extensions. Model-visible tools never accept executable server configuration.
mod approvals;
#[cfg(test)]
mod tests;
mod tools;

use agent_mcp::{McpCatalog, McpClient, McpServerConfig};
use agent_skills::{SkillCatalog, SkillCatalogConfig, SkillDiagnostic, SkillInfo};
pub use approvals::ExtensionApproval;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
pub use tools::ExtensionTools;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionConfig {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub disabled_skill_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct ExtensionSettings {
    pub config: ExtensionConfig,
    pub skills_directory: String,
    pub skills: Vec<SkillInfo>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

#[derive(Serialize)]
pub struct RunExtensions {
    pub skills: Vec<SkillInfo>,
    pub diagnostics: Vec<SkillDiagnostic>,
    pub approvals: Vec<ExtensionApproval>,
}

pub struct ExtensionHost {
    closed: AtomicBool,
    directory: PathBuf,
    config: RwLock<ExtensionConfig>,
    catalogs: RwLock<HashMap<Option<PathBuf>, Arc<SkillCatalog>>>,
    clients: RwLock<HashMap<(String, String), CachedClient>>,
    pub approvals: Arc<approvals::ApprovalQueue>,
}
struct CachedClient {
    client: McpClient,
    last_used: Instant,
}

impl ExtensionHost {
    pub fn open(directory: &Path) -> io::Result<Arc<Self>> {
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
        Ok(Arc::new(Self {
            closed: AtomicBool::new(false),
            directory,
            config: RwLock::new(config),
            catalogs: RwLock::default(),
            clients: RwLock::default(),
            approvals: Arc::default(),
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
        ExtensionSettings {
            config: self.config.read().unwrap().clone(),
            skills_directory: self.directory.join("skills").to_string_lossy().into_owned(),
            skills: catalog.skills().to_vec(),
            diagnostics: catalog.diagnostics().to_vec(),
        }
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
        drop(current);
        Ok(self.settings())
    }

    pub fn servers(&self) -> Vec<McpServerConfig> {
        self.config
            .read()
            .unwrap()
            .mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .cloned()
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
        let config = self.config.read().unwrap();
        let server = config
            .mcp_servers
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
                .filter(|(_, cached)| cached.client.is_idle())
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

    pub fn run_state(&self, run: &str, project: Option<&Path>) -> RunExtensions {
        let catalog = self.catalog(project);
        RunExtensions {
            skills: catalog.skills().to_vec(),
            diagnostics: catalog.diagnostics().to_vec(),
            approvals: self.approvals.list(run),
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
        .any(|id| id.is_empty() || id.chars().count() > 64)
    {
        return Err(io::Error::other("Invalid disabled skill identifier"));
    }
    Ok(())
}
