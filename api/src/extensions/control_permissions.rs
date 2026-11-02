//! Host-owned permissions, separate from active windows/tabs and model history.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCapability {
    Chrome,
    Browser,
    Computer,
    Mcp,
}
impl ControlCapability {
    pub fn for_plugin(plugin: &str) -> Option<Self> {
        match plugin {
            "chrome-use" => Some(Self::Chrome),
            "browser-use" => Some(Self::Browser),
            "computer-use" => Some(Self::Computer),
            _ => None,
        }
    }
}

/// Stable authorization identity. A process/tab lease is deliberately not part
/// of an app/site permission: a restart or a second task tab does not widen it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionTarget {
    Application { bundle_id: String },
    Origin { origin: String },
    McpTool { name: String },
    McpResource { uri: String },
    McpPrompt { name: String },
}
impl PermissionTarget {
    pub fn key(&self) -> String {
        serde_json::to_string(self).expect("permission target serializes")
    }
    fn valid_for(&self, capability: ControlCapability) -> bool {
        match (capability, self) {
            (ControlCapability::Computer, Self::Application { bundle_id }) => {
                !bundle_id.is_empty() && bundle_id.len() <= 4096
            }
            (ControlCapability::Chrome | ControlCapability::Browser, Self::Origin { origin }) => {
                url::Url::parse(origin).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.origin().ascii_serialization() == *origin
                })
            }
            (ControlCapability::Mcp, Self::McpTool { name } | Self::McpPrompt { name }) => {
                !name.is_empty() && name.len() <= 4096
            }
            (ControlCapability::Mcp, Self::McpResource { uri }) => {
                !uri.is_empty() && uri.len() <= 4096
            }
            _ => false,
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLifetime {
    #[default]
    Session,
    Always,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPermission {
    pub server_id: String,
    pub server_name: String,
    pub capability: ControlCapability,
    /// None means explicitly approved entire capability, never inferred from a
    /// new target. Old saved permissions were already whole-capability consent.
    #[serde(default)]
    pub target: Option<PermissionTarget>,
    #[serde(default)]
    pub lifetime: PermissionLifetime,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPermission {
    view: ControlPermission,
    binding_hash: String,
}
#[derive(Default)]
pub(super) struct CapabilityPermissions {
    path: Option<PathBuf>,
    persistent: Vec<StoredPermission>,
    session: HashMap<(String, String), Vec<StoredPermission>>,
    stopped: HashSet<String>,
}
fn binding_hash(binding: &str) -> String {
    format!("{:x}", Sha256::digest(binding.as_bytes()))
}
fn same_target(
    entry: &StoredPermission,
    server: &str,
    capability: ControlCapability,
    target: Option<&PermissionTarget>,
) -> bool {
    entry.view.server_id == server
        && entry.view.capability == capability
        && entry.view.target.as_ref() == target
}
impl CapabilityPermissions {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut persistent: Vec<StoredPermission> = if path.exists() {
            if fs::metadata(path)?.len() > 1024 * 1024 {
                return Err(io::Error::other("extension permissions exceed 1 MiB"));
            }
            serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?
        } else {
            Vec::new()
        };
        if persistent.len() > 512 {
            return Err(io::Error::other("too many extension permissions"));
        }
        for entry in &mut persistent {
            if entry
                .view
                .target
                .as_ref()
                .is_some_and(|target| !target.valid_for(entry.view.capability))
                || (entry.view.capability == ControlCapability::Mcp && entry.view.target.is_none())
            {
                return Err(io::Error::other("invalid extension permission target"));
            }
            entry.view.lifetime = PermissionLifetime::Always;
        }
        Ok(Self {
            path: Some(path.into()),
            persistent,
            ..Default::default()
        })
    }
    pub fn permits(
        &self,
        root: &str,
        server: &str,
        binding: &str,
        capability: ControlCapability,
        target: Option<&PermissionTarget>,
    ) -> bool {
        let hash = binding_hash(binding);
        let matches = |entry: &StoredPermission| {
            entry.view.server_id == server
                && entry.view.capability == capability
                && entry.binding_hash == hash
                && (entry.view.target.is_none() || entry.view.target.as_ref() == target)
        };
        self.session
            .get(&(root.into(), server.into()))
            .is_some_and(|entries| entries.iter().any(matches))
            || (!self.stopped.contains(root) && self.persistent.iter().any(matches))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn remember(
        &mut self,
        root: &str,
        server: &str,
        name: &str,
        binding: &str,
        capability: ControlCapability,
        target: Option<PermissionTarget>,
        always: bool,
    ) -> io::Result<()> {
        if target
            .as_ref()
            .is_some_and(|target| !target.valid_for(capability))
            || (capability == ControlCapability::Mcp && target.is_none())
        {
            return Err(io::Error::other("invalid extension permission target"));
        }
        let mut entry = StoredPermission {
            view: ControlPermission {
                server_id: server.into(),
                server_name: name.into(),
                capability,
                target,
                lifetime: if always {
                    PermissionLifetime::Always
                } else {
                    PermissionLifetime::Session
                },
            },
            binding_hash: binding_hash(binding),
        };
        if always {
            let mut updated = self.persistent.clone();
            updated.retain(|old| !same_target(old, server, capability, entry.view.target.as_ref()));
            updated.push(entry.clone());
            if updated.len() > 512 {
                return Err(io::Error::other("too many extension permissions"));
            }
            self.write(&updated)?;
            self.persistent = updated;
        }
        if self.persistent.iter().any(|saved| {
            same_target(saved, server, capability, entry.view.target.as_ref())
                && saved.binding_hash == entry.binding_hash
        }) {
            entry.view.lifetime = PermissionLifetime::Always;
        }
        let entries = self
            .session
            .entry((root.into(), server.into()))
            .or_default();
        entries.retain(|old| !same_target(old, server, capability, entry.view.target.as_ref()));
        entries.push(entry);
        Ok(())
    }
    pub fn list(&self) -> Vec<ControlPermission> {
        self.persistent.iter().map(|p| p.view.clone()).collect()
    }
    pub fn persistent_matches(&self, permission: &ControlPermission, binding: &str) -> bool {
        let hash = binding_hash(binding);
        self.persistent.iter().any(|entry| {
            same_target(
                entry,
                &permission.server_id,
                permission.capability,
                permission.target.as_ref(),
            ) && entry.binding_hash == hash
        })
    }
    pub fn session_list(&self, root: &str) -> Vec<ControlPermission> {
        let mut result: Vec<_> = self
            .session
            .iter()
            .filter(|((owner, _), _)| owner == root)
            .flat_map(|(_, entries)| entries.iter().map(|entry| entry.view.clone()))
            .collect();
        result.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        result
    }
    pub fn revoke(&mut self, server: &str) -> io::Result<()> {
        let mut updated = self.persistent.clone();
        updated.retain(|p| p.view.server_id != server);
        self.write(&updated)?;
        self.persistent = updated;
        self.session.retain(|(_, s), _| s != server);
        Ok(())
    }
    pub fn revoke_target(
        &mut self,
        root: Option<&str>,
        permission: &ControlPermission,
    ) -> io::Result<()> {
        let matches = |entry: &StoredPermission| {
            same_target(
                entry,
                &permission.server_id,
                permission.capability,
                permission.target.as_ref(),
            )
        };
        if permission.lifetime == PermissionLifetime::Always {
            let updated: Vec<_> = self
                .persistent
                .iter()
                .filter(|entry| !matches(entry))
                .cloned()
                .collect();
            self.write(&updated)?;
            self.persistent = updated;
        } else if root.is_none() {
            return Err(io::Error::other(
                "session permission revocation requires a conversation",
            ));
        }
        for ((owner, _), entries) in &mut self.session {
            if permission.lifetime == PermissionLifetime::Always || root == Some(owner.as_str()) {
                entries.retain(|entry| !matches(entry));
            }
        }
        Ok(())
    }
    pub fn stop(&mut self, root: &str) {
        self.session.retain(|(owner, _), _| owner != root);
        self.stopped.insert(root.into());
    }
    pub fn invalidate(&mut self, changed: &HashSet<String>) {
        self.session
            .retain(|(_, server), _| !changed.contains(server));
    }
    fn write(&self, entries: &[StoredPermission]) -> io::Result<()> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| io::Error::other("persistent control storage unavailable"))?;
        crate::snapshot::write_json(path, &entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "harness-target-permissions-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn site(host: &str) -> PermissionTarget {
        PermissionTarget::Origin {
            origin: format!("https://{host}"),
        }
    }

    #[test]
    fn persisted_targets_remain_separate_and_exact_revocation_keeps_other_sites() {
        let directory = directory();
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("permissions.json");
        let a = site("one.example");
        let b = site("two.example");
        let mut permissions = CapabilityPermissions::open(&path).unwrap();
        for target in [a.clone(), b.clone()] {
            permissions
                .remember(
                    "root",
                    "chrome",
                    "Chrome",
                    "v1",
                    ControlCapability::Chrome,
                    Some(target),
                    true,
                )
                .unwrap();
        }
        drop(permissions);
        let mut permissions = CapabilityPermissions::open(&path).unwrap();
        assert!(permissions.permits("other", "chrome", "v1", ControlCapability::Chrome, Some(&a)));
        assert!(!permissions.permits(
            "other",
            "chrome",
            "v1",
            ControlCapability::Chrome,
            Some(&site("sub.one.example"))
        ));
        assert!(!permissions.permits("other", "chrome", "v2", ControlCapability::Chrome, Some(&a)));
        let entry = permissions
            .list()
            .into_iter()
            .find(|entry| entry.target.as_ref() == Some(&a))
            .unwrap();
        permissions.revoke_target(None, &entry).unwrap();
        assert!(!permissions.permits("other", "chrome", "v1", ControlCapability::Chrome, Some(&a)));
        assert!(permissions.permits("other", "chrome", "v1", ControlCapability::Chrome, Some(&b)));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_whole_permissions_remain_explicit_whole_and_bound_to_original_code() {
        let directory = directory();
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("permissions.json");
        fs::write(&path, serde_json::to_vec(&serde_json::json!([{"view":{"server_id":"chrome","server_name":"Chrome","capability":"chrome"},"binding_hash":binding_hash("old-code")}])).unwrap()).unwrap();
        let permissions = CapabilityPermissions::open(&path).unwrap();
        assert!(permissions.list()[0].target.is_none());
        assert_eq!(permissions.list()[0].lifetime, PermissionLifetime::Always);
        assert!(permissions.permits(
            "root",
            "chrome",
            "old-code",
            ControlCapability::Chrome,
            Some(&site("example.com"))
        ));
        assert!(!permissions.permits(
            "root",
            "chrome",
            "replacement",
            ControlCapability::Chrome,
            Some(&site("example.com"))
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mcp_resource_prompt_and_tool_permissions_do_not_alias() {
        let mut permissions = CapabilityPermissions::default();
        let tool = PermissionTarget::McpTool {
            name: "read".into(),
        };
        permissions
            .remember(
                "root",
                "server",
                "Server",
                "v1",
                ControlCapability::Mcp,
                Some(tool.clone()),
                false,
            )
            .unwrap();
        assert!(permissions.permits("root", "server", "v1", ControlCapability::Mcp, Some(&tool)));
        assert!(!permissions.permits(
            "root",
            "server",
            "v1",
            ControlCapability::Mcp,
            Some(&PermissionTarget::McpPrompt {
                name: "read".into()
            })
        ));
        assert!(!permissions.permits(
            "root",
            "server",
            "v1",
            ControlCapability::Mcp,
            Some(&PermissionTarget::McpResource { uri: "read".into() })
        ));
        assert!(
            permissions
                .remember(
                    "root",
                    "server",
                    "Server",
                    "v1",
                    ControlCapability::Mcp,
                    None,
                    false
                )
                .is_err()
        );
    }
}
