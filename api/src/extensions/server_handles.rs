//! Model-facing handles only. Persisted configuration, clients and permissions
//! always use the original server ID.
use agent_mcp::McpServerConfig;
use sha2::{Digest, Sha256};
use std::io;

fn short_id(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    // ':' is forbidden in canonical McpServerConfig IDs. New configurations
    // therefore cannot steal a handle already present in conversation history.
    format!("mcp:{:x}", digest)[..16].to_owned()
}

pub(super) fn model_id(servers: &[McpServerConfig], server: &McpServerConfig) -> String {
    // Already concise user-defined IDs need no alias. A hash does not depend on
    // catalog order, display names, process lifetime or enabled peers.
    if server.id.len() <= 24 {
        return server.id.clone();
    }
    let handle = short_id(&server.id);
    if servers
        .iter()
        .any(|other| other.id != server.id && (other.id == handle || short_id(&other.id) == handle))
    {
        return server.id.clone();
    }
    handle
}

pub(super) fn resolve(servers: &[McpServerConfig], id: &str) -> io::Result<McpServerConfig> {
    // Legacy histories and host APIs may still contain the complete ID.
    servers
        .iter()
        .find(|server| server.id == id)
        .or_else(|| servers.iter().find(|server| model_id(servers, server) == id))
        .cloned()
        .ok_or_else(|| io::Error::other(
            "Unknown or unavailable MCP server ID. Call list_mcp_servers and copy its exact id; do not guess or reconstruct an ID. No operation was dispatched.",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_mcp::McpTransport;

    fn server(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.into(),
            name: None,
            enabled: true,
            transport: McpTransport::Stdio {
                command: "unused".into(),
                args: vec![],
                env_vars: vec![],
            },
        }
    }

    #[test]
    fn handles_survive_reordering_and_resolve_to_canonical_permissions_identity() {
        let first = server("plugin_7e496aa72ed140bfeb8b0584bba6b5ac92fe567a328bf6b2");
        let second = server("plugin_7e496aa72ed140bfeb8b0584bba6b5ac92fe567a328bf6b3");
        let catalog = vec![first.clone(), second.clone()];
        let id = model_id(&catalog, &first);
        assert_eq!(id.len(), 16);
        assert_ne!(id, model_id(&catalog, &second));
        assert_eq!(id, model_id(&[second, first.clone()], &first));
        assert_eq!(resolve(&catalog, &id).unwrap().id, first.id);
        assert_eq!(resolve(&catalog, &first.id).unwrap().id, first.id);
        assert!(
            resolve(
                &catalog,
                "plugin_7e496aa72ed140bfeb8b0584bba6b5ac92fe567b6b2"
            )
            .is_err()
        );
        assert!(resolve(&[], &id).is_err());
    }

    #[test]
    fn handles_use_a_namespace_unavailable_to_canonical_ids() {
        let original = server("plugin_very_long_canonical_server_identity");
        let collision = server(&short_id(&original.id));
        assert!(collision.validate().is_err());
        let lookalike = server(&collision.id.replace(':', "_"));
        assert!(lookalike.validate().is_ok());
        let catalog = vec![original.clone(), lookalike.clone()];
        assert_eq!(model_id(&catalog, &original), collision.id);
        assert_eq!(resolve(&catalog, &collision.id).unwrap().id, original.id);
        assert_eq!(resolve(&catalog, &lookalike.id).unwrap().id, lookalike.id);
        assert_eq!(model_id(&[server("local")], &server("local")), "local");
    }
}
