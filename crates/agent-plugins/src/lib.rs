//! Local declarative capability packages. Installation only copies and validates
//! files; it never executes commands, starts MCP servers, or contacts a registry.
//! Callers must serialize mutations and keep using normal MCP approval checks.

mod manifest;
mod package;

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use agent_mcp::McpServerConfig;
use serde::{Deserialize, Serialize};

pub const MAX_PLUGINS: usize = 64;
pub const MAX_PLUGIN_SERVERS: usize = 64;
pub const MAX_PLUGIN_SKILL_ROOTS: usize = 64;
/// Leave capacity in the shared 256-entry catalog for independent user/project skills.
pub const MAX_PLUGIN_SKILLS: usize = 192;
const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub source_path: PathBuf,
    pub content_hash: String,
    pub skill_count: usize,
    pub mcp_servers: Vec<McpServerConfig>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledPlugin {
    info: PluginInfo,
    /// A generated directory name, never a manifest-controlled filesystem path.
    revision: String,
    skill_paths: Vec<PathBuf>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    #[serde(default = "registry_version")]
    version: u32,
    plugins: BTreeMap<String, InstalledPlugin>,
}

fn registry_version() -> u32 {
    1
}

pub struct PluginStore {
    root: PathBuf,
    registry: Registry,
}

impl PluginStore {
    pub fn open(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        if fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(invalid("plugin store must not be a symbolic link"));
        }
        let root = fs::canonicalize(root)?;
        for directory in ["packages", "staging"] {
            let path = root.join(directory);
            fs::create_dir_all(&path)?;
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(invalid(
                    "plugin store directories must not be symbolic links",
                ));
            }
        }
        let registry_path = root.join("registry.json");
        let registry = match package::read_regular(&registry_path, MAX_REGISTRY_BYTES) {
            Ok(bytes) => serde_json::from_slice::<Registry>(&bytes)
                .map_err(|_| invalid("invalid plugin registry"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Registry {
                version: 1,
                ..Registry::default()
            },
            Err(error) => return Err(error),
        };
        let mut store = Self { root, registry };
        store.validate_registry(&store.registry)?;
        let damaged = store
            .registry
            .plugins
            .iter()
            .filter_map(|(id, plugin)| {
                store
                    .validate_installation(plugin)
                    .err()
                    .map(|_| id.clone())
            })
            .collect::<Vec<_>>();
        for id in damaged {
            if let Some(plugin) = store.registry.plugins.get_mut(&id) {
                plugin.info.enabled = false;
                let warning = "Installed package is missing, modified or unsafe; disabled until repaired by an update.";
                if !plugin.info.warnings.iter().any(|item| item == warning) {
                    plugin.info.warnings.push(warning.into());
                }
            }
        }
        store.cleanup_orphans();
        Ok(store)
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.registry
            .plugins
            .values()
            .map(|plugin| plugin.info.clone())
            .collect()
    }

    /// `None` installs a new identity and rejects duplicates. `Some(id)` updates
    /// that exact identity while preserving its enabled state. Old immutable
    /// revisions remain available to in-flight skill snapshots until maintenance.
    pub fn install(&mut self, source: &Path, expected_id: Option<&str>) -> io::Result<PluginInfo> {
        self.install_checked(source, expected_id, |_| Ok(()))
    }

    /// Validate host-wide constraints (for example standalone MCP ID collisions)
    /// before any published state changes. A rejected guard leaves the old
    /// installation and its enabled state intact.
    pub fn install_checked(
        &mut self,
        source: &Path,
        expected_id: Option<&str>,
        validate: impl FnOnce(&PluginInfo) -> io::Result<()>,
    ) -> io::Result<PluginInfo> {
        if let Some(id) = expected_id {
            if !self.registry.plugins.contains_key(id) {
                return Err(invalid("cannot update an unknown plugin"));
            }
        } else if self.registry.plugins.len() >= MAX_PLUGINS {
            return Err(invalid("plugin limit reached (64)"));
        }
        let source = source.to_path_buf();
        if fs::symlink_metadata(&source)?.file_type().is_symlink() {
            return Err(invalid("plugin import source must not be a symbolic link"));
        }
        let source = fs::canonicalize(source)?;
        // Importing the store itself can otherwise recursively copy staging.
        if self.root.starts_with(&source) || source.starts_with(&self.root) {
            return Err(invalid(
                "plugin import source must be outside the plugin store",
            ));
        }
        let staging = self.root.join("staging").join(unique_name("import"));
        fs::create_dir(&staging)?;
        let staged = self.prepare_install(&source, &staging, expected_id, validate);
        if staged.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        staged
    }

    fn prepare_install(
        &mut self,
        source: &Path,
        staging: &Path,
        expected_id: Option<&str>,
        validate: impl FnOnce(&PluginInfo) -> io::Result<()>,
    ) -> io::Result<PluginInfo> {
        package::import(source, staging)?;
        let content_hash = package::content_hash(staging)?;
        let metadata = manifest::read(staging)?;
        let id = metadata.id.clone();
        match expected_id {
            Some(expected) if expected != id => {
                return Err(invalid(
                    "updated plugin manifest name must match the installed identity",
                ));
            }
            None if self.registry.plugins.contains_key(&id) => {
                return Err(invalid(
                    "plugin already installed; use update to replace it",
                ));
            }
            _ => (),
        }
        let revision = unique_name(&content_hash);
        let destination = self.root.join("packages").join(&revision);
        let enabled = self
            .registry
            .plugins
            .get(&id)
            .is_none_or(|old| old.info.enabled);
        let servers = metadata.resolve_servers(&destination)?;
        let info = PluginInfo {
            id: id.clone(),
            name: metadata.name,
            version: metadata.version,
            description: metadata.description,
            enabled,
            source_path: source.to_path_buf(),
            content_hash,
            skill_count: metadata.skill_count,
            mcp_servers: servers,
            warnings: metadata.warnings,
        };
        validate(&info)?;
        let plugin = InstalledPlugin {
            info: info.clone(),
            revision,
            skill_paths: metadata.skill_paths,
        };
        let mut next = self.registry.clone();
        next.plugins.insert(id, plugin);
        validate_limits(&next)?;
        // Move a complete immutable revision into place before publishing it.
        // If registry persistence fails, the previous registry remains active.
        fs::rename(staging, &destination)?;
        if let Err(error) = self.persist(&next) {
            let _ = fs::remove_dir_all(&destination);
            return Err(error);
        }
        self.registry = next;
        Ok(info)
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> io::Result<()> {
        let mut next = self.registry.clone();
        let plugin = next
            .plugins
            .get_mut(id)
            .ok_or_else(|| invalid("unknown plugin"))?;
        if enabled {
            self.validate_installation(plugin)?;
        }
        plugin.info.enabled = enabled;
        self.persist(&next)?;
        self.registry = next;
        Ok(())
    }

    pub fn uninstall(&mut self, id: &str) -> io::Result<()> {
        let mut next = self.registry.clone();
        if next.plugins.remove(id).is_none() {
            return Err(invalid("unknown plugin"));
        }
        self.persist(&next)?;
        self.registry = next;
        // Retain immutable package files for already-running snapshots. They are
        // no longer discoverable and are reclaimed on the next application start.
        Ok(())
    }

    /// Only enabled packages contribute roots. The first tuple item is the
    /// namespace used by the skill catalog, not a display name.
    pub fn skill_roots(&self) -> Vec<(String, PathBuf)> {
        self.registry
            .plugins
            .values()
            .filter(|p| p.info.enabled)
            .flat_map(|p| {
                p.skill_paths
                    .iter()
                    .map(|path| (p.info.id.clone(), self.package_root(p).join(path)))
            })
            .collect()
    }

    /// Configurations only. The caller still owns discovery, connection and
    /// per-call MCP authorization; installation never starts a connection.
    pub fn servers(&self) -> Vec<McpServerConfig> {
        self.registry
            .plugins
            .values()
            .filter(|p| p.info.enabled)
            .flat_map(|p| p.info.mcp_servers.iter().filter(|s| s.enabled).cloned())
            .collect()
    }

    fn package_root(&self, plugin: &InstalledPlugin) -> PathBuf {
        self.root.join("packages").join(&plugin.revision)
    }

    /// The owning application opens one store before starting runs. Once a new
    /// process starts, no old in-memory snapshot can reference these revisions.
    /// Cleanup never follows links or changes active revisions and is best effort.
    fn cleanup_orphans(&self) {
        for directory in ["packages", "staging"] {
            let Ok(entries) = fs::read_dir(self.root.join(directory)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if directory == "packages"
                    && self.registry.plugins.values().any(|p| p.revision == name)
                {
                    continue;
                }
                let generated = if directory == "staging" {
                    name.strip_prefix("import-")
                } else {
                    Some(name)
                };
                if generated.is_none_or(|suffix| {
                    suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                }) {
                    continue;
                }
                let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    fn validate_registry(&self, registry: &Registry) -> io::Result<()> {
        if registry.version != 1 {
            return Err(invalid("unsupported plugin registry version"));
        }
        validate_limits(registry)?;
        for (id, plugin) in &registry.plugins {
            manifest::validate_id(id)?;
            if id != &plugin.info.id
                || plugin.revision.is_empty()
                || plugin.revision.len() > 160
                || !plugin
                    .revision
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b'-')
                || plugin.info.content_hash.len() != 64
                || !plugin
                    .info
                    .content_hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
            {
                return Err(invalid("invalid plugin registry identity"));
            }
            for path in &plugin.skill_paths {
                package::relative_path(
                    path.to_str().ok_or_else(|| invalid("invalid skill path"))?,
                )?;
            }
            for server in &plugin.info.mcp_servers {
                server
                    .validate()
                    .map_err(|_| invalid("invalid installed MCP configuration"))?;
            }
        }
        Ok(())
    }

    fn validate_installation(&self, plugin: &InstalledPlugin) -> io::Result<()> {
        let root = self.package_root(plugin);
        if fs::symlink_metadata(&root)?.file_type().is_symlink() || !root.is_dir() {
            return Err(invalid(
                "installed plugin directory is missing or unsafe; update it to repair",
            ));
        }
        for path in &plugin.skill_paths {
            package::validate_tree_path(&root, path)?;
        }
        if package::content_hash(&root)? != plugin.info.content_hash {
            return Err(invalid(
                "installed plugin content has changed; update it to repair",
            ));
        }
        Ok(())
    }

    fn persist(&self, registry: &Registry) -> io::Result<()> {
        validate_limits(registry)?;
        let bytes = serde_json::to_vec_pretty(registry).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(invalid("plugin registry exceeds size limit"));
        }
        let temporary = self.root.join(unique_name("registry"));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.root.join("registry.json"))?;
            // Syncing the directory is best effort after the atomic commit: an
            // error after rename must never cause in-memory/disk divergence.
            #[cfg(unix)]
            if let Ok(directory) = fs::File::open(&self.root) {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn validate_limits(registry: &Registry) -> io::Result<()> {
    if registry.plugins.len() > MAX_PLUGINS
        || registry
            .plugins
            .values()
            .map(|p| p.info.mcp_servers.len())
            .sum::<usize>()
            > MAX_PLUGIN_SERVERS
        || registry
            .plugins
            .values()
            .map(|p| p.skill_paths.len())
            .sum::<usize>()
            > MAX_PLUGIN_SKILL_ROOTS
        || registry
            .plugins
            .values()
            .map(|p| p.info.skill_count)
            .sum::<usize>()
            > MAX_PLUGIN_SKILLS
    {
        return Err(invalid(
            "installed plugin capabilities exceed limits (64 plugins, 64 MCP servers, 64 skill roots, 192 skills)",
        ));
    }
    Ok(())
}

fn unique_name(prefix: &str) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{prefix}-{stamp:x}-{:x}-{:x}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests;
