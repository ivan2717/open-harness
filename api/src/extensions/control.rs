//! Ephemeral UI-control grants. Never restored from conversation history.
use serde::Serialize;
use std::{collections::HashMap, io};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlScope {
    Browser,
    Application {
        bundle_id: String,
        pid: u32,
    },
    ChromeTab {
        tab_id: u32,
        origin: String,
    },
    /// Approval intent only; converted to one concrete tab after creation.
    ChromeNewTab {
        origin: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlAccess {
    pub server_id: String,
    pub server_name: String,
    pub scope: ControlScope,
    pub remembered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
    #[serde(skip)]
    binding: String,
    #[serde(skip)]
    grant_id: u64,
}

#[derive(Default)]
pub struct ControlRegistry {
    /// Host-owned delegation binding. A model can never choose its approval family.
    families: HashMap<String, String>,
    epochs: HashMap<String, u64>,
    access: HashMap<(String, String), ControlAccess>,
    // Active selection and remembered permissions are separate. Returning to
    // an explicitly approved application/tab must not erase other approvals.
    remembered: HashMap<(String, String), (String, Vec<ControlScope>)>,
    desktop_owner: Option<String>,
    next_grant_id: u64,
}

impl ControlRegistry {
    pub fn bind_scope(&mut self, run: &str, root: &str) {
        self.families.insert(run.into(), root.into());
    }
    pub fn authorization_scope<'a>(&'a self, run: &'a str) -> &'a str {
        self.families.get(run).map_or(run, String::as_str)
    }
    pub fn family_members(&self, root: &str) -> Vec<String> {
        let mut members: Vec<_> = self
            .families
            .iter()
            .filter(|(_, scope)| scope.as_str() == root)
            .map(|(run, _)| run.clone())
            .collect();
        if !members.iter().any(|run| run == root) {
            members.push(root.into());
        }
        members
    }
    pub fn epoch(&self, run: &str) -> u64 {
        self.epochs.get(run).copied().unwrap_or_default()
    }
    pub fn check_epoch(&self, run: &str, epoch: u64) -> io::Result<()> {
        if self.epoch(run) != epoch {
            return Err(io::Error::other("控制权限已撤销，请重新申请"));
        }
        Ok(())
    }
    pub fn permits(
        &self,
        run: &str,
        server: &str,
        binding: &str,
        scope: Option<&ControlScope>,
    ) -> bool {
        if let Some(scope) = scope {
            return self
                .remembered
                .get(&(self.authorization_scope(run).into(), server.into()))
                .is_some_and(|(approved_binding, scopes)| {
                    approved_binding == binding && scopes.contains(scope)
                });
        }
        self.access
            .get(&(run.into(), server.into()))
            .is_some_and(|access| {
                access.remembered
                    && access.binding == binding
                    && self.permits(run, server, binding, Some(&access.scope))
            })
    }

    pub fn activate_remembered(
        &mut self,
        run: &str,
        server: &str,
        name: &str,
        binding: &str,
        scope: &ControlScope,
    ) -> io::Result<Option<u64>> {
        if !self.permits(run, server, binding, Some(scope)) {
            return Err(io::Error::other("控制范围尚未获得会话授权"));
        }
        if self
            .access
            .get(&(run.into(), server.into()))
            .is_some_and(|current| {
                current.scope == *scope && current.binding == binding && current.remembered
            })
        {
            return Ok(None);
        }
        self.grant(run, server, name, binding, scope.clone(), true)
            .map(Some)
    }

    pub fn current_scope(&self, run: &str, server: &str, binding: &str) -> Option<&ControlScope> {
        self.access
            .get(&(run.into(), server.into()))
            .filter(|access| access.binding == binding)
            .map(|access| &access.scope)
    }

    /// Tab creation can change the bridge's selected tab before its result is
    /// known. Existing approvals remain reusable only through explicit selection.
    pub fn clear_active_tab(&mut self, run: &str, server: &str) {
        if self
            .access
            .get(&(run.into(), server.into()))
            .is_some_and(|access| matches!(access.scope, ControlScope::ChromeTab { .. }))
        {
            self.access.remove(&(run.into(), server.into()));
        }
    }
    pub fn acquire_desktop(&mut self, run: &str) -> io::Result<()> {
        let scope = self.authorization_scope(run).to_owned();
        if self
            .desktop_owner
            .as_deref()
            .is_some_and(|owner| owner != scope)
        {
            return Err(io::Error::other(
                "其他会话正在控制电脑，请先在该会话停止控制",
            ));
        }
        self.desktop_owner = Some(scope);
        Ok(())
    }
    pub fn owns_grant(&self, run: &str, server: &str, grant_id: u64) -> bool {
        self.access
            .get(&(run.into(), server.into()))
            .is_some_and(|access| access.grant_id == grant_id)
    }

    /// Share a session grant only after its first operation succeeds.
    pub fn remember_grant(&mut self, run: &str, server: &str, grant_id: u64) -> io::Result<()> {
        let permission_key = (self.authorization_scope(run).to_owned(), server.to_owned());
        let access = self
            .access
            .get_mut(&(run.into(), server.into()))
            .filter(|access| access.grant_id == grant_id)
            .ok_or_else(|| io::Error::other("控制权限已撤销，请重新申请"))?;
        access.remembered = true;
        let (binding, scopes) = self
            .remembered
            .entry(permission_key)
            .or_insert_with(|| (access.binding.clone(), Vec::new()));
        if *binding != access.binding {
            *binding = access.binding.clone();
            scopes.clear();
        }
        if !scopes.contains(&access.scope) {
            scopes.push(access.scope.clone());
        }
        Ok(())
    }

    pub fn grant(
        &mut self,
        run: &str,
        server: &str,
        name: &str,
        binding: &str,
        scope: ControlScope,
        remembered: bool,
    ) -> io::Result<u64> {
        if matches!(scope, ControlScope::ChromeNewTab { .. }) {
            return Err(io::Error::other(
                "Chrome tab creation must resolve to a concrete tab before granting control",
            ));
        }
        let grant_id = self
            .next_grant_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("control grant limit reached"))?;
        if matches!(scope, ControlScope::Application { .. }) {
            self.acquire_desktop(run)?;
        }
        self.next_grant_id = grant_id;
        let key = (run.into(), server.into());
        let permission_key = (self.authorization_scope(run).into(), server.into());
        if self
            .remembered
            .get(&permission_key)
            .is_some_and(|(previous, _)| previous != binding)
        {
            self.remembered.remove(&permission_key);
        }
        if remembered {
            let (_, scopes) = self
                .remembered
                .entry(permission_key)
                .or_insert_with(|| (binding.into(), Vec::new()));
            if !scopes.contains(&scope) {
                scopes.push(scope.clone());
            }
        }
        self.access.insert(
            key,
            ControlAccess {
                server_id: server.into(),
                server_name: name.into(),
                scope,
                remembered,
                source_run_id: None,
                task_title: None,
                binding: binding.into(),
                grant_id,
            },
        );
        Ok(grant_id)
    }
    pub fn list(&self, run: &str) -> Vec<ControlAccess> {
        let mut result: Vec<_> = self
            .access
            .iter()
            .filter(|((owner, _), _)| owner == run)
            .map(|(_, access)| access.clone())
            .collect();
        result.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        result
    }
    /// Keep revocation visible even after the agent which obtained a reusable
    /// permission has stopped. Active selections are displayed separately.
    pub fn retained_grants(&self, run: &str) -> Vec<ControlAccess> {
        if self.authorization_scope(run) != run {
            return Vec::new();
        }
        let mut result = Vec::new();
        for ((owner, server), (binding, scopes)) in &self.remembered {
            if owner != run {
                continue;
            }
            for scope in scopes {
                if self.access.iter().any(|((agent, target), access)| {
                    self.authorization_scope(agent) == run
                        && target == server
                        && access.scope == *scope
                        && access.binding == *binding
                }) {
                    continue;
                }
                result.push(ControlAccess {
                    server_id: server.clone(),
                    server_name: server.clone(),
                    scope: scope.clone(),
                    remembered: true,
                    source_run_id: None,
                    task_title: None,
                    binding: binding.clone(),
                    grant_id: 0,
                });
            }
        }
        result.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        result
    }
    pub fn release_server(&mut self, run: &str, server: &str) {
        self.access.remove(&(run.into(), server.into()));
        let scope = self.authorization_scope(run).to_owned();
        if scope == run {
            self.remembered.remove(&(run.into(), server.into()));
        }
        if self.desktop_owner.as_deref() == Some(scope.as_str())
            && !self.access.iter().any(|((owner, _), item)| {
                self.authorization_scope(owner) == scope
                    && matches!(item.scope, ControlScope::Application { .. })
            })
        {
            self.desktop_owner = None;
        }
    }
    pub fn revoke_run(&mut self, run: &str) {
        let scope = self.authorization_scope(run).to_owned();
        let members = if scope == run {
            self.family_members(run)
        } else {
            vec![run.into()]
        };
        for member in &members {
            *self.epochs.entry(member.clone()).or_default() += 1;
        }
        self.access.retain(|(owner, _), _| !members.contains(owner));
        if scope == run {
            self.remembered.retain(|(owner, _), _| owner != run);
        }
        if self.desktop_owner.as_deref() == Some(scope.as_str())
            && !self.access.iter().any(|((owner, _), item)| {
                self.authorization_scope(owner) == scope
                    && matches!(item.scope, ControlScope::Application { .. })
            })
        {
            self.desktop_owner = None;
        }
    }
    pub fn invalidate_servers(&mut self, changed: &std::collections::HashSet<String>) {
        let runs: std::collections::HashSet<_> = self
            .access
            .keys()
            .chain(self.remembered.keys())
            .filter(|(_, server)| changed.contains(server))
            .map(|(run, _)| run.clone())
            .collect();
        for run in runs {
            *self.epochs.entry(run.clone()).or_default() += 1;
            for server in changed {
                self.release_server(&run, server);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reusable_child_grants_remain_visible_and_revocable_after_child_stops() {
        let mut registry = ControlRegistry::default();
        registry.bind_scope("child", "root");
        registry
            .grant(
                "child",
                "browser",
                "Browser",
                "v1",
                ControlScope::Browser,
                true,
            )
            .unwrap();
        assert!(registry.retained_grants("root").is_empty());
        registry.revoke_run("child");
        assert!(registry.permits("root", "browser", "v1", Some(&ControlScope::Browser)));
        assert_eq!(registry.retained_grants("root").len(), 1);
        registry.revoke_run("root");
        assert!(registry.retained_grants("root").is_empty());
        assert!(!registry.permits("child", "browser", "v1", Some(&ControlScope::Browser)));
    }
    #[test]
    fn grants_are_bound_to_run_configuration_and_application_and_revocation() {
        let mut registry = ControlRegistry::default();
        let app = ControlScope::Application {
            bundle_id: "test.fixture".into(),
            pid: 42,
        };
        registry
            .grant("a", "server", "Desktop", "v1", app.clone(), true)
            .unwrap();
        assert!(registry.permits("a", "server", "v1", Some(&app)));
        assert!(!registry.permits("b", "server", "v1", Some(&app)));
        assert!(!registry.permits("a", "server", "v2", Some(&app)));
        assert!(!registry.permits(
            "a",
            "server",
            "v1",
            Some(&ControlScope::Application {
                bundle_id: "test.other".into(),
                pid: 43
            })
        ));
        assert!(registry.acquire_desktop("b").is_err());
        let epoch = registry.epoch("a");
        registry.revoke_run("a");
        assert!(registry.check_epoch("a", epoch).is_err());
        assert!(!registry.permits("a", "server", "v1", None));
        assert!(registry.acquire_desktop("b").is_ok());
    }
    #[test]
    fn one_shot_does_not_authorize_following_actions() {
        let mut registry = ControlRegistry::default();
        registry
            .grant("a", "s", "Browser", "v1", ControlScope::Browser, false)
            .unwrap();
        assert!(!registry.permits("a", "s", "v1", None));
        assert_eq!(registry.list("a").len(), 1);
    }

    #[test]
    fn remembered_selections_survive_switching_but_not_identity_changes_or_revocation() {
        let mut registry = ControlRegistry::default();
        let a = ControlScope::Application {
            bundle_id: "app.a".into(),
            pid: 12,
        };
        let b = ControlScope::Application {
            bundle_id: "app.b".into(),
            pid: 13,
        };
        registry
            .grant("run", "s", "Desktop", "v1", a.clone(), true)
            .unwrap();
        registry
            .grant("run", "s", "Desktop", "v1", b.clone(), true)
            .unwrap();
        assert!(registry.permits("run", "s", "v1", Some(&a)));
        assert!(
            registry
                .activate_remembered("run", "s", "Desktop", "v1", &a)
                .unwrap()
                .is_some()
        );
        assert_eq!(registry.current_scope("run", "s", "v1"), Some(&a));
        assert!(registry.permits("run", "s", "v1", Some(&b)));
        let restarted = ControlScope::Application {
            bundle_id: "app.a".into(),
            pid: 14,
        };
        assert!(!registry.permits("run", "s", "v1", Some(&restarted)));
        assert!(!registry.permits("run", "s", "v2", Some(&a)));
        registry
            .grant("run", "s", "Desktop", "v1", restarted.clone(), false)
            .unwrap();
        assert!(!registry.permits("run", "s", "v1", None));
        assert!(!registry.permits("run", "s", "v1", Some(&restarted)));
        assert!(registry.permits("run", "s", "v1", Some(&a)));
        registry.revoke_run("run");
        assert!(!registry.permits("run", "s", "v1", Some(&a)));
        assert!(!registry.permits("run", "s", "v1", Some(&b)));
        assert!(registry.acquire_desktop("other").is_ok());
    }

    #[test]
    fn chrome_permissions_bind_tab_and_origin_and_are_removed_on_release() {
        let mut registry = ControlRegistry::default();
        let scope = ControlScope::ChromeTab {
            tab_id: 9,
            origin: "https://example.com".into(),
        };
        registry
            .grant("r", "s", "Chrome", "v1", scope.clone(), true)
            .unwrap();
        assert!(registry.permits("r", "s", "v1", Some(&scope)));
        for other in [
            ControlScope::ChromeTab {
                tab_id: 10,
                origin: "https://example.com".into(),
            },
            ControlScope::ChromeTab {
                tab_id: 9,
                origin: "https://other.example".into(),
            },
        ] {
            assert!(!registry.permits("r", "s", "v1", Some(&other)));
        }
        registry.release_server("r", "s");
        assert!(!registry.permits("r", "s", "v1", Some(&scope)));
    }
}
