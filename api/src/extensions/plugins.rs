use super::{ExtensionConfig, ExtensionHost, ExtensionSettings, PluginInfo, PluginStore};
use agent_skills::{MAX_SKILLS, SkillCatalog, SkillCatalogConfig};
use std::{collections::HashSet, io, path::Path};

impl ExtensionHost {
    pub fn install_plugin(
        &self,
        source: &Path,
        expected_id: Option<&str>,
    ) -> io::Result<ExtensionSettings> {
        self.mutate_plugin(|store, config| {
            let installed = store.list();
            store
                .install_checked(source, expected_id, |plugin| {
                    if plugin.id.starts_with("builtin.") {
                        return Err(io::Error::other("builtin. 是内置插件保留标识"));
                    }
                    if plugin.mcp_servers.iter().any(|server| {
                        config
                            .mcp_servers
                            .iter()
                            .any(|manual| manual.id == server.id)
                    }) {
                        return Err(io::Error::other(
                            "插件工具标识与已有 MCP 服务冲突，请先修改该服务标识",
                        ));
                    }
                    self.validate_plugin_skill_capacity(&installed, plugin)?;
                    Ok(())
                })
                .map(|plugin| plugin.id)
        })
    }

    pub fn set_plugin_enabled(&self, id: &str, enabled: bool) -> io::Result<ExtensionSettings> {
        if id == crate::memory::PLUGIN_ID {
            let mut config = self.config.write().unwrap();
            let mut candidate = config.clone();
            candidate.llm_wiki_enabled = enabled;
            candidate.revision = candidate
                .revision
                .checked_add(1)
                .ok_or_else(|| io::Error::other("extension revision limit reached"))?;
            crate::snapshot::write_json(&self.directory.join("extensions.json"), &candidate)?;
            *config = candidate;
            drop(config);
            return Ok(self.settings());
        }
        self.mutate_plugin(|store, _| {
            if enabled {
                let installed = store.list();
                if let Some(mut plugin) = installed.iter().find(|plugin| plugin.id == id).cloned() {
                    plugin.enabled = true;
                    self.validate_plugin_skill_capacity(&installed, &plugin)?;
                }
            }
            store.set_enabled(id, enabled)?;
            Ok(id.to_owned())
        })
    }

    fn validate_plugin_skill_capacity(
        &self,
        installed: &[PluginInfo],
        candidate: &PluginInfo,
    ) -> io::Result<()> {
        if !candidate.enabled || candidate.skill_count == 0 {
            return Ok(());
        }
        let plugin_skills = installed
            .iter()
            .filter(|plugin| plugin.enabled && plugin.id != candidate.id)
            .map(|plugin| plugin.skill_count)
            .sum::<usize>()
            + candidate.skill_count;
        let mut projects: HashSet<_> = self.catalogs.read().unwrap().keys().cloned().collect();
        projects.insert(None);
        // Check personal skills and every project already observed by this host.
        // New or externally edited project roots still report catalog limits on discovery.
        for project in projects {
            let standalone = SkillCatalog::discover(SkillCatalogConfig::for_workspace(
                self.directory.join("skills"),
                project.as_deref(),
            ));
            if standalone.skills().len() + plugin_skills > MAX_SKILLS {
                return Err(io::Error::other(
                    "插件与现有技能合计超过 256 个，请先停用其他插件或减少独立技能",
                ));
            }
        }
        Ok(())
    }

    pub fn uninstall_plugin(&self, id: &str) -> io::Result<ExtensionSettings> {
        if id == crate::memory::PLUGIN_ID {
            return Err(io::Error::other("内置插件可以关闭，无需卸载"));
        }
        self.mutate_plugin(|store, _| {
            store.uninstall(id)?;
            Ok(id.to_owned())
        })
    }

    fn mutate_plugin(
        &self,
        mutate: impl FnOnce(&mut PluginStore, &ExtensionConfig) -> io::Result<String>,
    ) -> io::Result<ExtensionSettings> {
        // Serialize installation with configuration changes and MCP client creation.
        // The store commits its registry atomically before publishing new components.
        let config = self.config.write().unwrap();
        let mut plugins = self.plugins.write().unwrap();
        let before = plugins.list();
        let id = mutate(&mut plugins, &config)?;
        let affected: HashSet<_> = before
            .into_iter()
            .chain(plugins.list())
            .filter(|plugin| plugin.id == id)
            .flat_map(|plugin| plugin.mcp_servers.into_iter().map(|server| server.id))
            .collect();
        self.catalogs.write().unwrap().clear();
        self.clients.write().unwrap().retain(|(_, server), cached| {
            if affected.contains(server) {
                cached.client.revoke();
                false
            } else {
                true
            }
        });
        self.approvals.invalidate_servers(&affected);
        drop(plugins);
        drop(config);
        Ok(self.settings())
    }
}
