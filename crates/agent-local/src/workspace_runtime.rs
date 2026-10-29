//! Read the desktop host's runtime inventory. It is configuration, never a
//! model-provided path or a request to install/execute arbitrary packages.
use serde_json::{Value, json};
use std::{fs, io, path::Path};

pub(crate) fn describe(root: &Path, execution_mode: &str, arguments: &str) -> io::Result<String> {
    let arguments: Value = serde_json::from_str(arguments).map_err(io::Error::other)?;
    if !arguments
        .as_object()
        .is_some_and(|object| object.is_empty())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an empty object",
        ));
    }
    let inventory = match std::env::var_os("OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST") {
        Some(path) => read_inventory(Path::new(&path))?,
        None => json!({"version":1,"node":null,"python":null,"notes":[
            "No desktop runtime inventory is available. Check installed interpreters through run_command before choosing one; do not assume Python or third-party libraries exist."
        ]}),
    };
    let mut result = describe_inventory(root, inventory);
    result["execution_mode"] = execution_mode.into();
    Ok(result.to_string())
}

fn describe_inventory(root: &Path, inventory: Value) -> Value {
    let mut result = json!({
        "working_directory":root,
        "scratch_directory":".openharness-tmp",
        "output_directory":"outputs",
        "shell":if cfg!(windows) {"powershell"} else {"/bin/sh"},
        "execution_mode":crate::command::execution_mode(),
    });
    // Report the shared inventory unchanged. Discovering dependencies must not
    // create environments, choose a conversation venv, or redirect pnpm stores.
    for key in ["version", "node", "python", "notes"] {
        if let Some(value) = inventory.get(key) {
            result[key] = value.clone();
        }
    }
    for key in ["node", "python"] {
        if let Some(runtime) = result[key].as_object_mut() {
            runtime.remove("read_paths");
        }
    }
    // Do not expose the old npm command when reading an older host manifest.
    if let Some(node) = result["node"].as_object_mut()
        && node
            .get("package_manager")
            .is_some_and(|manager| manager["name"] != "pnpm")
    {
        node.remove("package_manager");
    }
    result
}

fn read_inventory(path: &Path) -> io::Result<Value> {
    if !path.is_absolute() || fs::metadata(path)?.len() > 64 * 1024 {
        return Err(io::Error::other("invalid workspace runtime inventory"));
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?;
    if value["version"] != 1 {
        return Err(io::Error::other(
            "unsupported workspace runtime inventory version",
        ));
    }
    for key in ["node", "python"] {
        if let Some(runtime) = value.get(key).filter(|runtime| !runtime.is_null()) {
            let executable = runtime["executable"]
                .as_str()
                .filter(|value| Path::new(value).is_absolute());
            if executable.is_none() || !runtime["args"].is_array() {
                return Err(io::Error::other(
                    "runtime inventory is missing an absolute interpreter or arguments",
                ));
            }
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_model_paths_instead_of_reading_them() {
        assert!(describe(Path::new("/work"), "sandboxed", r#"{"path":"/etc/passwd"}"#).is_err());
    }

    #[test]
    fn rejects_relative_manifest_paths() {
        assert!(read_inventory(Path::new("runtime.json")).is_err());
    }

    #[test]
    fn discovery_preserves_shared_runtimes_and_package_manager_configuration() {
        let root = std::env::temp_dir().join(format!("runtime-inventory-{}", std::process::id()));
        let inventory = json!({"version":1,
            "python":{"executable":"/managed/python","args":["-I","-B"],"environment":{},"libraries":[{"name":"PIL"}],"read_paths":["/managed/python-root"]},
            "node":{"executable":"/managed/node","args":[],"module_paths":["/managed/node_modules"],"package_manager":{"name":"pnpm","executable":"/managed/node","args":["/managed/pnpm.cjs"]}}
        });
        let result = describe_inventory(&root, inventory.clone());
        assert_eq!(
            result["python"]["executable"],
            inventory["python"]["executable"]
        );
        assert_eq!(result["python"]["args"], inventory["python"]["args"]);
        assert_eq!(result["node"], inventory["node"]);
        assert!(result["python"].get("read_paths").is_none());
        assert!(result.get("dependency_environment").is_none());
        assert!(!root.exists(), "discovery must not create any files");
    }

    #[test]
    fn stale_npm_inventory_is_not_exposed_as_a_pnpm_invocation() {
        let inventory = json!({"version":1,"node":{"executable":"/managed/node","args":[],"package_manager":{"executable":"/managed/node","args":["/managed/npm-cli.js"]}},"python":null});
        let result = describe_inventory(Path::new("/work"), inventory);
        assert!(result["node"].get("package_manager").is_none());
    }
}
