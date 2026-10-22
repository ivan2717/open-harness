use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::{McpError, Result};

/// Configuration contains environment variable *names*, never credential values.
/// Saving this configuration does not start processes or make network requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    pub transport: McpTransport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env_vars: Vec<String>,
    },
    Http {
        url: String,
        #[serde(default)]
        bearer_token_env: Option<String>,
    },
}

impl McpServerConfig {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(McpError::Config(
                "server id must contain 1–64 letters, digits, underscores or hyphens",
            ));
        }
        if self.name.as_ref().is_some_and(|name| name.len() > 256) {
            return Err(McpError::Config("server display name is too long"));
        }
        match &self.transport {
            McpTransport::Stdio {
                command,
                args,
                env_vars,
            } => {
                if command.trim().is_empty() || command.len() > 4096 || command.contains('\0') {
                    return Err(McpError::Config("stdio command is invalid"));
                }
                if args.len() > 128
                    || args
                        .iter()
                        .any(|arg| arg.len() > 8192 || arg.contains('\0'))
                {
                    return Err(McpError::Config(
                        "stdio arguments exceed the supported limits",
                    ));
                }
                if env_vars.len() > 64 || env_vars.iter().any(|name| !valid_env_name(name)) {
                    return Err(McpError::Config(
                        "environment entries must be variable names, not values",
                    ));
                }
            }
            McpTransport::Http {
                url,
                bearer_token_env,
            } => {
                let parsed =
                    Url::parse(url).map_err(|_| McpError::Config("invalid MCP server URL"))?;
                let loopback = match parsed.host() {
                    Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                    Some(Host::Ipv4(ip)) => ip.is_loopback(),
                    Some(Host::Ipv6(ip)) => ip.is_loopback(),
                    None => false,
                };
                if url.len() > 4096
                    || parsed.host().is_none()
                    || !(parsed.scheme() == "https" || parsed.scheme() == "http" && loopback)
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.query().is_some()
                    || parsed.fragment().is_some()
                {
                    return Err(McpError::Config(
                        "MCP URL must use HTTPS (or loopback HTTP), without credentials, query or fragment",
                    ));
                }
                if bearer_token_env
                    .as_ref()
                    .is_some_and(|name| !valid_env_name(name))
                {
                    return Err(McpError::Config(
                        "bearer_token_env must be an environment variable name",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 128
        && bytes
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_urls_without_echoing_secrets() {
        for url in [
            "http://example.com/mcp",
            "https://user:secret@example.com",
            "https://example.com?api_key=secret",
            "file:///tmp/mcp",
        ] {
            let config = McpServerConfig {
                id: "test".into(),
                name: None,
                enabled: true,
                transport: McpTransport::Http {
                    url: url.into(),
                    bearer_token_env: None,
                },
            };
            let error = config.validate().unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
        for url in [
            "https://example.com/mcp",
            "http://127.0.0.1:9000/mcp",
            "http://[::1]:9000/mcp",
        ] {
            McpServerConfig {
                id: "test".into(),
                name: None,
                enabled: true,
                transport: McpTransport::Http {
                    url: url.into(),
                    bearer_token_env: Some("MCP_TOKEN".into()),
                },
            }
            .validate()
            .unwrap();
        }
    }
}
