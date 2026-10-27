//! Keep normal host identity without inheriting credentials or manufacturing
//! per-conversation homes, package caches, stores or dependency environments.
use crate::paths;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) fn prepare_temporary_directory(root: &Path) -> io::Result<PathBuf> {
    // Libraries need writable temporary files inside the workspace sandbox.
    // This is scratch space, not a package environment or a replacement HOME.
    let relative = ".openharness-tmp/tmp";
    let directory = paths::resolve(root, relative)?;
    fs::create_dir_all(&directory)?;
    let checked = paths::resolve(root, relative)?;
    if fs::canonicalize(&checked)? != checked {
        return Err(paths::denied("command temporary directory changed"));
    }
    Ok(checked)
}

/// Read-only commands never create files in the project. Their temporary
/// files are private to one command, not a dependency environment.
pub(crate) struct ReadOnlyScratch(PathBuf);
impl ReadOnlyScratch {
    pub fn create(id: &str) -> io::Result<Self> {
        let directory = std::env::temp_dir().join(format!("openharness-{id}"));
        fs::create_dir(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(fs::canonicalize(directory)?))
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for ReadOnlyScratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn environment(temporary: &Path) -> BTreeMap<&'static str, OsString> {
    let mut result = BTreeMap::new();
    // The parent may hold model credentials and application state. Preserve
    // only standard identity/location/locale values; PATH is set by the host.
    for key in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
        "USER",
        "USERNAME",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    ] {
        if let Some(value) = std::env::var_os(key) {
            result.insert(key, value);
        }
    }
    result.entry("LANG").or_insert_with(|| "en_US.UTF-8".into());
    for key in ["TMPDIR", "TMP", "TEMP"] {
        result.insert(key, temporary.as_os_str().to_owned());
    }
    result.insert(
        "MPLCONFIGDIR",
        temporary.join("matplotlib").into_os_string(),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_host_identity_without_injecting_package_environments() {
        let temporary = Path::new("/workspace/.openharness-tmp/tmp");
        let result = environment(temporary);
        for key in [
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_CACHE_HOME",
        ] {
            assert_eq!(result.get(key).cloned(), std::env::var_os(key));
        }
        for key in ["TMPDIR", "TMP", "TEMP"] {
            assert_eq!(result[key], temporary.as_os_str());
        }
        for key in [
            "PIP_CACHE_DIR",
            "PIP_REQUIRE_VIRTUALENV",
            "UV_CACHE_DIR",
            "UV_PROJECT_ENVIRONMENT",
            "PNPM_HOME",
            "pnpm_config_store_dir",
            "npm_config_store_dir",
            "OPENHARNESS_TASK_DIRECTORY",
            "OPENAI_API_KEY",
        ] {
            assert!(!result.contains_key(key), "unexpected variable: {key}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_redirected_temporary_directory() {
        let root = std::env::temp_dir().join(format!("command-env-{}", std::process::id()));
        fs::create_dir_all(root.join(".openharness-tmp")).unwrap();
        fs::create_dir_all(root.join("outside")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join(".openharness-tmp/tmp"))
            .unwrap();
        let result = prepare_temporary_directory(&root);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_dir(root.join("outside")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
