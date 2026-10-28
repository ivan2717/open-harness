//! User decisions for trusted built-in control capabilities. Kept separate from
//! model-visible configuration and from the currently selected window or tab.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCapability {
    Chrome,
    Browser,
    Computer,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPermission {
    pub server_id: String,
    pub server_name: String,
    pub capability: ControlCapability,
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
    session: HashMap<(String, String), StoredPermission>,
    stopped: HashSet<String>,
}
fn binding_hash(binding: &str) -> String {
    format!("{:x}", Sha256::digest(binding.as_bytes()))
}

impl CapabilityPermissions {
    pub fn open(path: &Path) -> io::Result<Self> {
        let persistent: Vec<StoredPermission> = if path.exists() {
            if fs::metadata(path)?.len() > 128 * 1024 {
                return Err(io::Error::other("control permissions exceed 128 KiB"));
            }
            serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?
        } else {
            Vec::new()
        };
        if persistent.len() > 128 {
            return Err(io::Error::other("too many control permissions"));
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
    ) -> bool {
        let hash = binding_hash(binding);
        let matches = |entry: &StoredPermission| {
            entry.view.server_id == server
                && entry.view.capability == capability
                && entry.binding_hash == hash
        };
        self.session
            .get(&(root.into(), server.into()))
            .is_some_and(matches)
            || (!self.stopped.contains(root) && self.persistent.iter().any(matches))
    }
    pub fn remember(
        &mut self,
        root: &str,
        server: &str,
        name: &str,
        binding: &str,
        capability: ControlCapability,
        always: bool,
    ) -> io::Result<()> {
        let entry = StoredPermission {
            view: ControlPermission {
                server_id: server.into(),
                server_name: name.into(),
                capability,
            },
            binding_hash: binding_hash(binding),
        };
        if always {
            let mut updated = self.persistent.clone();
            updated.retain(|p| p.view.server_id != server);
            updated.push(entry.clone());
            if updated.len() > 128 {
                return Err(io::Error::other("too many control permissions"));
            }
            self.write(&updated)?;
            self.persistent = updated;
        }
        self.session.insert((root.into(), server.into()), entry);
        Ok(())
    }
    pub fn list(&self) -> Vec<ControlPermission> {
        self.persistent.iter().map(|p| p.view.clone()).collect()
    }
    pub fn persistent_matches(
        &self,
        server: &str,
        binding: &str,
        capability: ControlCapability,
    ) -> bool {
        let hash = binding_hash(binding);
        self.persistent.iter().any(|entry| {
            entry.view.server_id == server
                && entry.view.capability == capability
                && entry.binding_hash == hash
        })
    }
    pub fn session_list(&self, root: &str) -> Vec<ControlPermission> {
        let mut result: Vec<_> = self
            .session
            .iter()
            .filter(|((owner, _), _)| owner == root)
            .map(|(_, entry)| entry.view.clone())
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
    pub fn stop(&mut self, root: &str) {
        self.session.retain(|(owner, _), _| owner != root);
        self.stopped.insert(root.into());
    }
    pub fn invalidate(&mut self, changed: &HashSet<String>) {
        self.session
            .retain(|(_, server), _| !changed.contains(server));
        // Bindings independently prevent reusing a saved decision for a changed
        // service. Settings only expose entries matching the current binding.
    }
    fn write(&self, entries: &[StoredPermission]) -> io::Result<()> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| io::Error::other("persistent control storage unavailable"))?;
        crate::snapshot::write_json(path, &entries)
    }
}
