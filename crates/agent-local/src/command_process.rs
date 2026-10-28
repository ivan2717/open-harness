//! OS isolation and child process lifetime share one command-scoped boundary.
#[cfg(not(windows))]
use crate::paths;
use crate::{LocalTools, NetworkAccess, command_environment};
use std::{fs, io, path::PathBuf, process::Stdio};
use tokio::process::{Child, ChildStdin, Command};

#[derive(Default)]
pub(crate) struct CommandProcessOptions {
    /// None retains workspace writes; Some([]) is a read-only workspace.
    pub write_paths: Option<Vec<PathBuf>>,
    /// An exact host-owned scratch directory, never a model-supplied path.
    pub temporary_directory: Option<PathBuf>,
}

pub(crate) fn sandbox_available() -> bool {
    #[cfg(windows)]
    {
        windows_helper().is_ok()
    }
    #[cfg(not(windows))]
    {
        cfg!(target_os = "macos") && std::path::Path::new("/usr/bin/sandbox-exec").is_file()
    }
}

#[cfg(windows)]
fn windows_helper() -> io::Result<PathBuf> {
    let path = std::env::var_os("OPENHARNESS_WINDOWS_SANDBOX_HELPER")
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::other(
                "Windows command sandbox is missing; install the complete desktop package",
            )
        })?;
    if !path.is_absolute() || !path.is_file() {
        return Err(io::Error::other(
            "Windows command sandbox helper must be an existing absolute executable path",
        ));
    }
    fs::canonicalize(path)
}

pub(crate) struct CommandProcess {
    pub child: Child,
    group: Option<u32>,
    _supervision: Option<ChildStdin>,
    #[cfg(windows)]
    job: windows::Job,
}
impl CommandProcess {
    pub async fn spawn_with_options(
        tools: &LocalTools,
        script: &str,
        network_access: NetworkAccess,
        options: CommandProcessOptions,
    ) -> io::Result<Self> {
        let temporary = match options.temporary_directory {
            Some(path) => fs::canonicalize(path)?,
            None => command_environment::prepare_temporary_directory(tools.root())?,
        };
        if !temporary.is_dir() {
            return Err(io::Error::other("command scratch must be a directory"));
        }
        tools.verify_root()?;
        let mut write_paths = options
            .write_paths
            .unwrap_or_else(|| vec![tools.root().to_owned()]);
        for path in &mut write_paths {
            *path = fs::canonicalize(&*path)?;
            if !path.starts_with(tools.root()) {
                return Err(io::Error::other(
                    "command write paths must stay in the workspace",
                ));
            }
        }
        write_paths.push(temporary.clone());
        let mut runtime = RuntimeEnvironment::load()?;
        // These are exact immutable files authorized by the host from durable
        // user attachments, never their parent directory or model-chosen paths.
        runtime.read_paths.extend(tools.attachment_read_paths()?);
        #[cfg(not(windows))]
        let mut command = {
            // EOF on the separate supervision pipe kills the process group,
            // including if the API process exits without running destructors.
            const WRAPPER: &str = "owner=$$\n(while IFS= read -r line; do :; done; kill -KILL -- \"-$owner\") <&0 >/dev/null 2>&1 &\n/usr/bin/sandbox-exec -p \"$1\" /bin/sh -c \"$2\" </dev/null\nexit \"$?\"";
            let mut command = Command::new("/bin/sh");
            command.args([
                "-c",
                WRAPPER,
                "openharness-command",
                &sandbox_profile(
                    tools.root(),
                    &tools.inner.state_dir,
                    &runtime.read_paths,
                    &runtime.public_certificates,
                    network_access,
                    &write_paths,
                ),
                script,
            ]);
            command.process_group(0);
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new(windows_helper()?);
            // The trusted helper and all its children enter the Job before execution.
            command.creation_flags(
                windows_sys::Win32::System::Threading::CREATE_SUSPENDED
                    | windows_sys::Win32::System::Threading::CREATE_NO_WINDOW,
            );
            command
        };
        command
            .current_dir(tools.root())
            .env_clear()
            .env("PATH", runtime.path.clone())
            .envs(command_environment::environment(&temporary))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.env(
            "NODE_PATH",
            std::env::join_paths(&runtime.module_paths).map_err(io::Error::other)?,
        );
        if runtime.electron_node {
            command.env("ELECTRON_RUN_AS_NODE", "1");
        }
        #[cfg(windows)]
        for key in [
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
            "ProgramFiles",
            "ProgramFiles(x86)",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut child = command.spawn()?;
        #[cfg(windows)]
        let job = match windows::Job::assign_and_resume(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.start_kill();
                return Err(error);
            }
        };
        let group = child.id();
        #[cfg(not(windows))]
        let supervision = child.stdin.take();
        #[cfg(windows)]
        let supervision = {
            use tokio::io::AsyncWriteExt;
            let request = serde_json::json!({
                "version":1, "program":windows::shell(),
                "args":["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &format!("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding; {script}")],
                "cwd":tools.root(), "workspace":tools.root(), "read_paths":runtime.read_paths, "write_paths":write_paths,
                "denied_paths":[tools.inner.state_dir],
                "network_allowed":network_access == NetworkAccess::Allowed,
            });
            let mut encoded = serde_json::to_vec(&request).map_err(io::Error::other)?;
            encoded.push(b'\n');
            let mut pipe = child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("missing sandbox input"))?;
            pipe.write_all(&encoded).await?;
            pipe.shutdown().await?;
            None
        };
        Ok(Self {
            child,
            group,
            _supervision: supervision,
            #[cfg(windows)]
            job,
        })
    }
    pub fn terminate(&mut self) {
        #[cfg(unix)]
        if let Some(group) = self
            .group
            .take()
            .and_then(|group| i32::try_from(group).ok())
        {
            // This group is created by us, held alive by the trusted watchdog.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        {
            self.group.take();
            self.job.terminate();
        }
    }
}
impl Drop for CommandProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(not(windows))]
fn quoted(path: &std::path::Path) -> String {
    serde_json::to_string(&path.to_string_lossy()).expect("path serialization")
}

#[cfg(not(windows))]
fn sandbox_profile(
    root: &std::path::Path,
    state: &std::path::Path,
    runtimes: &[PathBuf],
    public_certificates: &[PathBuf],
    network_access: NetworkAccess,
    write_paths: &[PathBuf],
) -> String {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process*)\n(allow sysctl-read)\n(allow mach-lookup)\n(allow file-read-metadata)\n(allow file-read* (literal \"/\"))\n",
    );
    if network_access == NetworkAccess::Allowed {
        profile.push_str("(allow network*)\n");
    }
    for directory in [
        "/usr",
        "/bin",
        "/sbin",
        "/System",
        "/Library",
        "/dev",
        "/private/etc",
        "/private/var/db",
        "/opt/homebrew",
    ] {
        profile.push_str(&format!(
            "(allow file-read* (subpath {}))\n",
            quoted(std::path::Path::new(directory))
        ));
    }
    for directory in runtimes {
        profile.push_str(&format!(
            "(allow file-read* (subpath {}))\n(deny file-write* (subpath {}))\n",
            quoted(directory),
            quoted(directory)
        ));
    }
    if let Some(home) = std::env::var_os("HOME") {
        for directory in [".cargo", ".rustup", ".nvm", ".local/share/mise", ".bun/bin"] {
            profile.push_str(&format!(
                "(allow file-read* (subpath {}))\n",
                quoted(&std::path::Path::new(&home).join(directory))
            ));
        }
        for secret in [".cargo/credentials", ".cargo/credentials.toml"] {
            profile.push_str(&format!(
                "(deny file-read* (literal {}))\n",
                quoted(&std::path::Path::new(&home).join(secret))
            ));
        }
    }
    profile.push_str(&format!("(allow file-read* (subpath {}))\n", quoted(root)));
    for path in write_paths {
        profile.push_str(&format!(
            "(allow file-read* file-write* (subpath {}))\n",
            quoted(path)
        ));
    }
    // Regex restrictions apply throughout nested projects as well as at root.
    profile.push_str(&format!(
        "(deny file-write* (regex #\"(^|/){}(/|$)\"))\n",
        paths::regex_literal(".git")
    ));
    for pattern in paths::secret_patterns() {
        profile.push_str(&format!("(deny file-write* (regex #\"{pattern}\"))\n"));
        // certifi's public trust roots are application resources, not user
        // private keys. Exempt only those exact managed files from reads; the
        // suffix guard still denies every other PEM and all certificate writes.
        let exceptions = public_certificates
            .iter()
            .map(|path| format!(" (require-not (literal {}))", quoted(path)))
            .collect::<String>();
        profile.push_str(&format!(
            "(deny file-read* (require-all (regex #\"{pattern}\"){exceptions}))\n"
        ));
    }
    profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");
    profile.push_str(&format!(
        "(deny file-read* file-write* (subpath {}))\n",
        quoted(state)
    ));
    profile
}

#[cfg(windows)]
#[path = "command_windows.rs"]
mod windows;

struct RuntimeEnvironment {
    path: std::ffi::OsString,
    read_paths: Vec<PathBuf>,
    electron_node: bool,
    module_paths: Vec<PathBuf>,
    public_certificates: Vec<PathBuf>,
}
impl RuntimeEnvironment {
    fn load() -> io::Result<Self> {
        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let mut result = Self {
            path: original_path.clone(),
            read_paths: Vec::new(),
            electron_node: false,
            module_paths: Vec::new(),
            public_certificates: Vec::new(),
        };
        let Some(manifest_path) = std::env::var_os("OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST") else {
            return Ok(result);
        };
        if !PathBuf::from(&manifest_path).is_absolute() {
            return Err(io::Error::other(
                "workspace runtime manifest path must be absolute",
            ));
        }
        let metadata = fs::metadata(&manifest_path)?;
        if metadata.len() > 256 * 1024 {
            return Err(io::Error::other("workspace runtime manifest is too large"));
        }
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(manifest_path)?).map_err(io::Error::other)?;
        if manifest["version"] != 1 {
            return Err(io::Error::other("unsupported workspace runtime manifest"));
        }
        let mut paths = Vec::new();
        for name in ["node", "python"] {
            let runtime = &manifest[name];
            if let Some(executable) = runtime["executable"].as_str() {
                let executable = PathBuf::from(executable);
                if !executable.is_absolute() || !executable.is_file() {
                    continue;
                }
                if let Some(parent) = executable.parent() {
                    paths.push(parent.to_path_buf());
                }
                if let Some(directories) = runtime["read_paths"].as_array() {
                    for directory in directories.iter().filter_map(serde_json::Value::as_str) {
                        let directory = PathBuf::from(directory);
                        if directory.is_absolute()
                            && directory.parent().is_some()
                            && directory.exists()
                        {
                            let directory = fs::canonicalize(directory)?;
                            let is_home_ancestor = ["HOME", "USERPROFILE"]
                                .into_iter()
                                .filter_map(std::env::var_os)
                                .filter_map(|home| fs::canonicalize(home).ok())
                                .any(|home| home.starts_with(&directory));
                            if directory.parent().is_some() && !is_home_ancestor {
                                if name == "python" && runtime["source"] == "bundled" {
                                    let certificate =
                                        directory.join("pip/_vendor/certifi/cacert.pem");
                                    if certificate.is_file() {
                                        let certificate = fs::canonicalize(certificate)?;
                                        if certificate.starts_with(&directory) {
                                            result.public_certificates.push(certificate);
                                        }
                                    }
                                }
                                result.read_paths.push(directory);
                            }
                        }
                    }
                }
                if name == "node" && runtime["environment"]["ELECTRON_RUN_AS_NODE"] == "1" {
                    result.electron_node = true;
                }
                if name == "node"
                    && let Some(module_paths) = runtime["module_paths"].as_array()
                {
                    result.module_paths.extend(
                        module_paths
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(PathBuf::from)
                            .filter(|path| path.is_absolute()),
                    );
                }
            }
        }
        paths.extend(std::env::split_paths(&original_path));
        result.path = std::env::join_paths(paths).map_err(io::Error::other)?;
        Ok(result)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod sandbox_tests {
    use super::*;

    #[test]
    fn read_only_workspace_keeps_reads_but_writes_only_to_private_external_scratch() {
        let base = std::env::temp_dir().join(format!(
            "openharness-readonly-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for path in ["workspace/.git", "scratch", "other-scratch", "state"] {
            fs::create_dir_all(base.join(path)).unwrap();
        }
        let base = fs::canonicalize(base).unwrap();
        let root = base.join("workspace");
        let scratch = base.join("scratch");
        fs::write(root.join("input.txt"), "project input").unwrap();
        fs::write(root.join(".env"), "project secret").unwrap();
        fs::write(root.join(".git/config"), "git metadata").unwrap();
        fs::write(base.join("other-scratch/input.txt"), "other command").unwrap();
        let profile = sandbox_profile(
            &root,
            &base.join("state"),
            &[],
            &[],
            NetworkAccess::Denied,
            std::slice::from_ref(&scratch),
        );
        let script = format!(
            "set -e\ncat input.txt\nprintf 'scratch output' > {scratch}\ncat {scratch}\ncat .git/config\nif printf changed > input.txt; then exit 10; fi\nif printf changed > new.txt; then exit 11; fi\nif rm input.txt; then exit 12; fi\nif cat .env; then exit 13; fi\nif printf changed > {other}; then exit 14; fi\nif cat {other_read}; then exit 15; fi\nif printf changed > .git/config; then exit 16; fi\n",
            scratch = quoted(&scratch.join("output.txt")),
            other = quoted(&base.join("other-scratch/output.txt")),
            other_read = quoted(&base.join("other-scratch/input.txt")),
        );
        let result = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/bin/sh", "-c", &script])
            .current_dir(&root)
            .output()
            .unwrap();
        let project = fs::read_to_string(root.join("input.txt")).unwrap();
        let output = fs::read_to_string(scratch.join("output.txt")).ok();
        let leaked_write =
            root.join("new.txt").exists() || base.join("other-scratch/output.txt").exists();
        fs::remove_dir_all(base).unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout),
            "project inputscratch outputgit metadata"
        );
        assert_eq!(project, "project input");
        assert_eq!(output.as_deref(), Some("scratch output"));
        assert!(!leaked_write);
    }

    #[test]
    fn managed_public_ca_can_be_read_but_private_certificates_and_all_ca_writes_are_denied() {
        let base = std::env::temp_dir().join(format!(
            "openharness-public-ca-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(base.join("workspace")).unwrap();
        fs::create_dir_all(base.join("runtime")).unwrap();
        let base = fs::canonicalize(base).unwrap();
        let root = base.join("workspace");
        let runtime = base.join("runtime");
        let public_ca = runtime.join("cacert.pem");
        fs::write(&public_ca, "PUBLIC_CA").unwrap();
        fs::write(root.join("private.pem"), "PRIVATE_KEY").unwrap();
        fs::write(root.join("PRIVATE.KEY"), "PRIVATE_KEY").unwrap();
        let profile = sandbox_profile(
            &root,
            &base.join("state"),
            &[runtime],
            std::slice::from_ref(&public_ca),
            NetworkAccess::Denied,
            std::slice::from_ref(&root),
        );
        let script = format!(
            "cat {}; if cat private.pem PRIVATE.KEY 2>/dev/null; then exit 1; fi; if printf mutation > {} 2>/dev/null; then exit 2; fi; if printf mutation > another.pem 2>/dev/null; then exit 3; fi; if printf mutation > another.key 2>/dev/null; then exit 4; fi",
            quoted(&public_ca),
            quoted(&public_ca)
        );
        let result = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/bin/sh", "-c", &script])
            .current_dir(&root)
            .output()
            .unwrap();
        let public_unchanged = fs::read_to_string(&public_ca).unwrap();
        let new_private_exists =
            root.join("another.pem").exists() || root.join("another.key").exists();
        fs::remove_dir_all(&base).unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&result.stdout), "PUBLIC_CA");
        assert_eq!(public_unchanged, "PUBLIC_CA");
        assert!(!new_private_exists);
    }
}
