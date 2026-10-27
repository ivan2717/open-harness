use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use agent_mcp::{McpServerConfig, McpTransport};
use agent_skills::{SkillCatalog, SkillCatalogConfig, SkillRoot, SkillScope};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{invalid, package};

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const ROOT_MARKERS: [&str; 3] = [
    "${PLUGIN_ROOT}",
    "${CODEX_PLUGIN_ROOT}",
    "${CLAUDE_PLUGIN_ROOT}",
];

pub(crate) struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub skill_paths: Vec<PathBuf>,
    pub skill_count: usize,
    pub warnings: Vec<String>,
    servers: Vec<McpServerConfig>,
}

impl Manifest {
    pub fn resolve_servers(&self, destination: &Path) -> io::Result<Vec<McpServerConfig>> {
        let root = destination
            .to_str()
            .ok_or_else(|| invalid("plugin store path must be UTF-8"))?;
        let mut servers = self.servers.clone();
        for server in &mut servers {
            if let McpTransport::Stdio { command, args, .. } = &mut server.transport {
                *command = replace_root(command, root);
                for argument in args {
                    *argument = replace_root(argument, root);
                }
            }
            server.validate().map_err(|_| {
                invalid("resolved plugin MCP configuration exceeds supported limits")
            })?;
        }
        Ok(servers)
    }
}

pub(crate) fn validate_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id.len() > 64
        || id.contains("..")
        || !id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        || !id.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
        || !id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
    {
        return Err(invalid(
            "plugin name must contain 1–64 lowercase ASCII letters, digits, hyphens, underscores or single dots, with alphanumeric ends",
        ));
    }
    Ok(())
}

pub(crate) fn read(root: &Path) -> io::Result<Manifest> {
    let portable = root.join("plugin.json").is_file();
    let path = if portable {
        root.join("plugin.json")
    } else {
        root.join(".codex-plugin/plugin.json")
    };
    let raw = json_file(&path)?;
    let object = raw
        .as_object()
        .ok_or_else(|| invalid("plugin manifest must be a JSON object"))?;
    let id = required_text(object, "name", 64)?;
    validate_id(&id)?;
    let version = optional_text(object, "version", 128)?.unwrap_or_else(|| "0.0.0".to_owned());
    let description = optional_text(object, "description", 2048)?.unwrap_or_default();
    let mut warnings = Vec::new();
    if !object.contains_key("version") {
        warnings
            .push("No version declared; using 0.0.0. Updates still track the content hash.".into());
    }
    unsupported_fields(object, &mut warnings);
    let extension = object.get("extensions").and_then(Value::as_object);
    if let Some(extensions) = extension {
        for key in extensions.keys() {
            warnings.push(format!(
                "Extension {key:?} is not executed; only skills and MCP capabilities are supported."
            ));
        }
        if let Some(openai) = extensions.get("com.openai").and_then(Value::as_object) {
            unsupported_fields(openai, &mut warnings);
        }
    }
    if portable && root.join(".codex-plugin/plugin.json").exists() {
        warnings.push(
            "Root plugin.json takes precedence; the legacy compatibility overlay is not imported."
                .into(),
        );
    }
    let name = object
        .get("interface")
        .and_then(Value::as_object)
        .map(|interface| optional_text(interface, "displayName", 128))
        .transpose()?
        .flatten()
        .unwrap_or_else(|| id.clone());
    let skill_paths = read_skill_paths(root, object.get("skills"))?;
    let skill_count = validate_skills(root, &id, &skill_paths)?;
    let mcp_decl = object
        .get("mcpServers")
        .or_else(|| object.get("mcp_servers"));
    let mcp = if let Some(value) = mcp_decl {
        match value {
            Value::String(path) => Some(json_file(&root.join(package::relative_path(path)?))?),
            Value::Object(_) => Some(value.clone()),
            _ => {
                return Err(invalid(
                    "mcpServers must be a relative JSON file path or server configuration object",
                ));
            }
        }
    } else {
        let default = if portable { "mcp.json" } else { ".mcp.json" };
        let path = root.join(default);
        if path.exists() {
            Some(json_file(&path)?)
        } else {
            None
        }
    };
    let servers = mcp
        .as_ref()
        .map(|value| parse_servers(root, &id, &name, value))
        .transpose()?
        .unwrap_or_default();
    if skill_count == 0 && servers.is_empty() {
        let suffix = if warnings.is_empty() {
            String::new()
        } else {
            format!(" {}", warnings.join(" "))
        };
        return Err(invalid(format!(
            "plugin contains no supported skills or MCP servers.{suffix}"
        )));
    }
    Ok(Manifest {
        id,
        name,
        version,
        description,
        skill_paths,
        skill_count,
        warnings,
        servers,
    })
}

fn unsupported_fields(object: &Map<String, Value>, warnings: &mut Vec<String>) {
    for key in [
        "apps",
        "hooks",
        "commands",
        "agents",
        "scripts",
        "dependencies",
        "install",
        "onboardingSkill",
        "onboarding_skill",
    ] {
        if object.contains_key(key) {
            warnings.push(format!("{key} is not supported and will not be loaded or executed. This host imports skills and MCP configurations only."));
        }
    }
}

fn read_skill_paths(root: &Path, declaration: Option<&Value>) -> io::Result<Vec<PathBuf>> {
    let paths = match declaration {
        None => {
            if root.join("skills").exists() {
                vec![PathBuf::from("skills")]
            } else {
                Vec::new()
            }
        }
        Some(Value::String(path)) => vec![package::relative_path(path)?],
        Some(Value::Array(paths)) => paths
            .iter()
            .map(|path| {
                path.as_str()
                    .ok_or_else(|| invalid("skill paths must be strings"))
                    .and_then(package::relative_path)
            })
            .collect::<io::Result<_>>()?,
        _ => {
            return Err(invalid(
                "skills must be a package-relative directory or array of directories",
            ));
        }
    };
    if paths.len() > 64 {
        return Err(invalid("plugin declares too many skill roots"));
    }
    let mut unique = BTreeSet::new();
    for path in paths {
        package::validate_tree_path(root, &path)?;
        if !root.join(&path).is_dir() {
            return Err(invalid("each skills path must be an existing directory"));
        }
        unique.insert(path);
    }
    let paths = unique.into_iter().collect::<Vec<_>>();
    if paths.iter().enumerate().any(|(i, path)| {
        paths
            .iter()
            .skip(i + 1)
            .any(|other| other.starts_with(path) || path.starts_with(other))
    }) {
        return Err(invalid("plugin skill roots must not overlap"));
    }
    Ok(paths)
}

fn validate_skills(root: &Path, id: &str, paths: &[PathBuf]) -> io::Result<usize> {
    let catalog = SkillCatalog::discover(SkillCatalogConfig {
        roots: paths
            .iter()
            .map(|path| SkillRoot {
                path: root.join(path),
                scope: SkillScope::Additional,
                namespace: Some(id.to_owned()),
            })
            .collect(),
        disabled_skill_ids: Vec::new(),
    });
    if let Some(diagnostic) = catalog.diagnostics().first() {
        // Do not repeat arbitrary manifest/file content in an installation error.
        return Err(invalid(format!(
            "plugin skill validation failed ({}); check SKILL.md frontmatter, duplicate names and discovery limits",
            diagnostic.code
        )));
    }
    Ok(catalog.skills().len())
}

fn parse_servers(
    root: &Path,
    plugin: &str,
    display_name: &str,
    value: &Value,
) -> io::Result<Vec<McpServerConfig>> {
    let map = value
        .get("mcpServers")
        .unwrap_or(value)
        .as_object()
        .ok_or_else(|| invalid("MCP configuration must contain a server object"))?;
    if map.len() > 64 {
        return Err(invalid("plugin declares more than 64 MCP servers"));
    }
    let mut result = Vec::new();
    for (key, entry) in map {
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err(invalid(
                "MCP server names must contain 1–128 text characters",
            ));
        }
        let config = entry
            .as_object()
            .ok_or_else(|| invalid("MCP server configuration must be an object"))?;
        let supported = [
            "command",
            "args",
            "env",
            "env_vars",
            "url",
            "type",
            "enabled",
            "name",
            "bearer_token_env",
            "bearerTokenEnv",
            "headers",
        ];
        if config.keys().any(|key| !supported.contains(&key.as_str())) {
            return Err(invalid(
                "unsupported MCP configuration field; supported fields are command, args, env variable references, env_vars, url, type, enabled, name, bearer_token_env and a bearer environment header",
            ));
        }
        let kind = config
            .get("type")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| invalid("MCP transport type must be a string"))
            })
            .transpose()?;
        let transport = if config.contains_key("command") {
            if kind.is_some_and(|kind| kind != "stdio")
                || config.contains_key("url")
                || config.contains_key("headers")
                || config.contains_key("bearer_token_env")
                || config.contains_key("bearerTokenEnv")
            {
                return Err(invalid(
                    "stdio MCP config cannot include HTTP fields or a different transport type",
                ));
            }
            let mut command = required_text(config, "command", 4096)?;
            if command.starts_with("./") {
                command = format!(
                    "${{PLUGIN_ROOT}}/{}",
                    package::relative_path(&command)?.to_string_lossy()
                );
            }
            validate_local_command(root, &command)?;
            let mut args = string_array(config.get("args"), "MCP args")?;
            for argument in &mut args {
                if argument.starts_with("./") {
                    *argument = format!(
                        "${{PLUGIN_ROOT}}/{}",
                        package::relative_path(argument)?.to_string_lossy()
                    );
                } else if argument.starts_with("../") {
                    return Err(invalid(
                        "MCP file arguments must stay inside the package; use ${PLUGIN_ROOT}/path",
                    ));
                }
                validate_root_reference(root, argument, false)?;
            }
            let mut env_vars = string_array(config.get("env_vars"), "MCP env_vars")?;
            if let Some(env) = config.get("env") {
                let env = env.as_object().ok_or_else(|| {
                    invalid("MCP env must map names to matching environment references")
                })?;
                for (name, value) in env {
                    if value.as_str() != Some(&format!("${{{name}}}")) {
                        return Err(invalid(
                            "literal or renamed MCP environment values are not supported; use env_vars or KEY: '${KEY}' and configure secrets in the host environment",
                        ));
                    }
                    env_vars.push(name.clone());
                }
            }
            env_vars.sort();
            env_vars.dedup();
            McpTransport::Stdio {
                command,
                args,
                env_vars,
            }
        } else if config.contains_key("url") {
            if kind
                .is_some_and(|kind| !matches!(kind, "http" | "streamable-http" | "streamable_http"))
                || config.contains_key("env")
                || config.contains_key("env_vars")
                || config.contains_key("args")
            {
                return Err(invalid(
                    "only Streamable HTTP MCP is supported; SSE, stdio fields and transport-specific auth are not supported",
                ));
            }
            let url = required_text(config, "url", 4096)?;
            let mut bearer_token_env = optional_text(config, "bearer_token_env", 128)?
                .or(optional_text(config, "bearerTokenEnv", 128)?);
            if let Some(headers) = config.get("headers") {
                let headers = headers
                    .as_object()
                    .ok_or_else(|| invalid("MCP headers must be an object"))?;
                if headers.len() != 1 || bearer_token_env.is_some() {
                    return Err(invalid(
                        "only a single Authorization bearer environment reference is supported; use bearer_token_env",
                    ));
                }
                let (name, value) = headers
                    .iter()
                    .next()
                    .ok_or_else(|| invalid("empty MCP headers"))?;
                if !name.eq_ignore_ascii_case("authorization") {
                    return Err(invalid("custom MCP headers are not supported"));
                }
                bearer_token_env = Some(value.as_str().and_then(|s| s.strip_prefix("Bearer ${")).and_then(|s| s.strip_suffix('}')).ok_or_else(|| invalid("literal authorization headers are not supported; use 'Bearer ${ENV_NAME}'"))?.to_owned());
            }
            McpTransport::Http {
                url,
                bearer_token_env,
            }
        } else {
            return Err(invalid("each MCP server must declare command or url"));
        };
        let digest = Sha256::digest(format!("{plugin}\0{key}").as_bytes());
        let id = format!("plugin_{digest:x}");
        let id = id[..55].to_owned();
        let enabled = config
            .get("enabled")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| invalid("MCP enabled must be a boolean"))
            })
            .transpose()?
            .unwrap_or(true);
        let label = optional_text(config, "name", 128)?.unwrap_or_else(|| key.clone());
        let name = format!("{display_name} · {label}");
        let server = McpServerConfig {
            id,
            name: Some(name),
            enabled,
            transport,
        };
        server.validate().map_err(|_| invalid("invalid MCP transport configuration or limits; URLs must be HTTPS (or loopback HTTP) without embedded credentials, and environment entries must be variable names"))?;
        result.push(server);
    }
    Ok(result)
}

fn validate_local_command(root: &Path, command: &str) -> io::Result<()> {
    if Path::new(command).is_absolute() || command.starts_with("../") || command.contains('\\') {
        return Err(invalid(
            "plugin commands must be executable names on PATH or package-relative paths",
        ));
    }
    validate_root_reference(root, command, true)?;
    if !ROOT_MARKERS
        .iter()
        .any(|marker| command.starts_with(marker))
        && command.contains('/')
    {
        return Err(invalid(
            "use ./ or ${PLUGIN_ROOT}/ for a bundled MCP executable",
        ));
    }
    Ok(())
}

fn validate_root_reference(root: &Path, value: &str, command: bool) -> io::Result<()> {
    for marker in ROOT_MARKERS {
        if value.contains(marker) {
            // Embedded roots in flags like --config=${PLUGIN_ROOT}/file are
            // accepted; the suffix must still remain inside the package.
            let (_, suffix) = value
                .split_once(marker)
                .ok_or_else(|| invalid("invalid package root placeholder"))?;
            if suffix.is_empty() && !command {
                continue;
            }
            let suffix = suffix.strip_prefix('/').ok_or_else(|| {
                invalid("package root placeholder must be followed by a relative file path")
            })?;
            let path = package::relative_path(suffix)?;
            package::validate_tree_path(root, &path)?;
            if command && !root.join(&path).is_file() {
                return Err(invalid(
                    "bundled MCP command does not name an existing file",
                ));
            }
            #[cfg(unix)]
            if command {
                use std::os::unix::fs::PermissionsExt;
                if std::fs::metadata(root.join(&path))?.permissions().mode() & 0o111 == 0 {
                    return Err(invalid(
                        "bundled MCP command must be executable; set its executable permission or use an interpreter command with the script as an argument",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn replace_root(value: &str, root: &str) -> String {
    let resolved = ROOT_MARKERS
        .iter()
        .fold(value.to_owned(), |text, marker| text.replace(marker, root));
    if root.starts_with(r"\\?\")
        && let Some(start) = ROOT_MARKERS
            .iter()
            .filter_map(|marker| value.find(marker))
            .min()
    {
        // Canonical Windows roots use the verbatim \\?\ prefix, which disables
        // slash conversion. Normalize the expanded path, keeping any literal
        // flag prefix intact (for example --config=${PLUGIN_ROOT}/file).
        let (prefix, path) = resolved.split_at(start);
        return format!("{prefix}{}", path.replace('/', "\\"));
    }
    resolved
}

fn string_array(value: Option<&Value>, field: &str) -> io::Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) if values.len() <= 128 => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|s| s.len() <= 8192 && !s.contains('\0'))
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(format!("{field} must contain bounded strings")))
            })
            .collect(),
        _ => Err(invalid(format!(
            "{field} must be an array of at most 128 strings"
        ))),
    }
}

fn json_file(path: &Path) -> io::Result<Value> {
    let bytes = package::read_regular(path, MAX_MANIFEST_BYTES)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| invalid("plugin manifest or MCP file is not valid JSON"))
}

fn required_text(object: &Map<String, Value>, key: &str, max: usize) -> io::Result<String> {
    optional_text(object, key, max)?
        .ok_or_else(|| invalid(format!("plugin configuration requires {key}")))
}

fn optional_text(object: &Map<String, Value>, key: &str, max: usize) -> io::Result<Option<String>> {
    object
        .get(key)
        .map(|value| {
            value
                .as_str()
                .filter(|text| {
                    !text.trim().is_empty()
                        && text.len() <= max
                        && !text.chars().any(char::is_control)
                })
                .map(|text| text.trim().to_owned())
                .ok_or_else(|| {
                    invalid(format!(
                        "invalid {key}; expected nonempty text up to {max} bytes"
                    ))
                })
        })
        .transpose()
}

#[cfg(test)]
mod root_resolution_tests {
    use super::*;

    #[test]
    fn verbatim_roots_normalize_only_placeholder_paths_and_preserve_literal_prefixes() {
        for root in [
            r"\\?\C:\Plugin Store\package",
            r"\\?\UNC\server\share\package",
        ] {
            for marker in ROOT_MARKERS {
                assert_eq!(replace_root(marker, root), root);
                assert_eq!(
                    replace_root(&format!("{marker}/bin/fixture.py"), root),
                    format!(r"{root}\bin\fixture.py"),
                );
                assert_eq!(
                    replace_root(&format!("--config/file={marker}/data/settings.json"), root),
                    format!(r"--config/file={root}\data\settings.json"),
                );
            }
            for literal in [
                "https://example.com/api/v1",
                "--mode=read/write",
                "data/input.json",
            ] {
                assert_eq!(replace_root(literal, root), literal);
            }
        }
    }

    #[test]
    fn unix_roots_keep_their_original_separator_and_argument_behavior() {
        let root = "/tmp/Plugin Store/package";
        for marker in ROOT_MARKERS {
            assert_eq!(
                replace_root(&format!("--config={marker}/data/settings.json"), root),
                format!("--config={root}/data/settings.json"),
            );
        }
    }
}
