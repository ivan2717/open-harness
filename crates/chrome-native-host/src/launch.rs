use serde::Deserialize;
use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeLauncher {
    version: u8,
    executable: PathBuf,
    script: PathBuf,
    config: PathBuf,
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid Chrome launcher configuration",
    )
}

fn regular_file(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid());
    }
    Ok(())
}

fn native_command(directory: &Path, args: &[OsString]) -> io::Result<Command> {
    validate_native_args(args)?;
    let path = directory.join("native-launcher.json");
    regular_file(&path)?;
    let mut bytes = Vec::new();
    File::open(&path)?.take(16_385).read_to_end(&mut bytes)?;
    if bytes.len() > 16_384 {
        return Err(invalid());
    }
    let config: NativeLauncher = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if config.version != 1
        || config.config != directory.join("config.json")
        || config.script.file_name() != Some(OsStr::new("native-host.mjs"))
    {
        return Err(invalid());
    }
    regular_file(&config.executable)?;
    regular_file(&config.script)?;
    regular_file(&config.config)?;
    let mut command = runtime_command(&config.executable, &config.script);
    command.env("OPENHARNESS_CHROME_CONFIG", config.config);
    command.args(args);
    Ok(command)
}

fn validate_native_args(args: &[OsString]) -> io::Result<()> {
    let origin = args
        .first()
        .and_then(|arg| arg.to_str())
        .ok_or_else(invalid)?;
    let id = origin
        .strip_prefix("chrome-extension://")
        .and_then(|value| value.strip_suffix('/'))
        .ok_or_else(invalid)?;
    if id.len() != 32 || !id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)) {
        return Err(invalid());
    }
    // Chrome supplies an optional native window handle on Windows. Keep it as
    // data; neither this launcher nor its child ever passes it through a shell.
    if args.len() > 2 {
        return Err(invalid());
    }
    if let Some(argument) = args.get(1) {
        let handle = argument
            .to_str()
            .and_then(|arg| arg.strip_prefix("--parent-window="))
            .ok_or_else(invalid)?;
        if handle.is_empty() || !handle.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn mcp_command(
    directory: &Path,
    executable: Option<OsString>,
    args: &[OsString],
) -> io::Result<Command> {
    if !args.is_empty() {
        if args.len() != 2 || args[0] != "--task-id" {
            return Err(invalid());
        }
        let task = args[1].to_str().ok_or_else(invalid)?;
        if task.is_empty() || task.encode_utf16().count() > 256 || task.contains('\0') {
            return Err(invalid());
        }
    }
    let executable = PathBuf::from(executable.ok_or_else(invalid)?);
    let script = directory.join("server.mjs");
    regular_file(&executable)?;
    regular_file(&script)?;
    let mut command = runtime_command(&executable, &script);
    command.args(args);
    Ok(command)
}

fn runtime_command(executable: &Path, script: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg(script)
        .env("ELECTRON_RUN_AS_NODE", "1")
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

pub fn run() -> io::Result<u8> {
    let executable = env::current_exe()?;
    let directory = executable.parent().ok_or_else(invalid)?;
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let mut command = if args.first().is_some_and(|arg| arg == "--mcp") {
        mcp_command(
            directory,
            env::var_os("OPENHARNESS_NODE_BINARY"),
            &args[1..],
        )?
    } else {
        native_command(directory, &args)?
    };
    #[cfg(windows)]
    let job = crate::windows_job::Job::new()?;
    let mut child = command.spawn()?;
    #[cfg(windows)]
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let status = child.wait()?;
    Ok(status
        .code()
        .and_then(|code| code.try_into().ok())
        .unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let path = env::temp_dir().join(format!(
                "oh chrome launcher-{}-{} with & % quotes'",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            for name in [
                "runtime.exe",
                "native-host.mjs",
                "config.json",
                "server.mjs",
            ] {
                fs::write(path.join(name), "fixture").unwrap();
            }
            Self(path)
        }

        fn config(&self) -> serde_json::Value {
            serde_json::json!({
                "version": 1,
                "executable": self.0.join("runtime.exe"),
                "script": self.0.join("native-host.mjs"),
                "config": self.0.join("config.json"),
            })
        }

        fn write(&self, config: serde_json::Value) {
            fs::write(self.0.join("native-launcher.json"), config.to_string()).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn args() -> Vec<OsString> {
        vec![
            "chrome-extension://keibjmjjimfgkikahpjgbfebiglbjkee/".into(),
            "--parent-window=0".into(),
        ]
    }

    #[test]
    fn native_paths_and_origin_are_passed_as_arguments_without_a_shell() {
        let fixture = Fixture::new();
        fixture.write(fixture.config());
        let command = native_command(&fixture.0, &args()).unwrap();
        assert_eq!(command.get_program(), fixture.0.join("runtime.exe"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                fixture.0.join("native-host.mjs").as_os_str(),
                args()[0].as_os_str(),
                args()[1].as_os_str(),
            ]
        );
        let environment = command.get_envs().collect::<Vec<_>>();
        assert!(environment.contains(&(OsStr::new("NODE_OPTIONS"), None)));
        assert!(environment.contains(&(OsStr::new("NODE_PATH"), None)));
        assert!(environment.contains(&(OsStr::new("ELECTRON_RUN_AS_NODE"), Some(OsStr::new("1")))));
    }

    #[test]
    fn invalid_origins_and_additional_launch_options_are_rejected() {
        for args in [
            vec![],
            vec!["https://example.com/".into()],
            vec!["chrome-extension://short/".into()],
            vec![args()[0].clone(), "--inspect".into()],
            vec![args()[0].clone(), "--parent-window=-1".into()],
            vec![
                args()[0].clone(),
                "--parent-window=0".into(),
                "extra".into(),
            ],
        ] {
            assert!(validate_native_args(&args).is_err());
        }
    }

    #[test]
    fn configuration_must_be_local_bounded_and_have_known_fields() {
        let fixture = Fixture::new();
        for (key, value) in [
            ("version", serde_json::json!(2)),
            ("executable", serde_json::json!("relative.exe")),
            ("script", serde_json::json!(fixture.0.join("server.mjs"))),
            (
                "config",
                serde_json::json!(fixture.0.join("elsewhere.json")),
            ),
            ("extra", serde_json::json!(true)),
        ] {
            let mut config = fixture.config();
            config[key] = value;
            fixture.write(config);
            assert!(native_command(&fixture.0, &args()).is_err(), "{key}");
        }
        fs::write(fixture.0.join("native-launcher.json"), vec![b' '; 16_385]).unwrap();
        assert!(native_command(&fixture.0, &args()).is_err());
    }

    #[test]
    fn mcp_uses_only_the_fixed_sibling_server_and_requires_an_absolute_runtime() {
        let fixture = Fixture::new();
        let command =
            mcp_command(&fixture.0, Some(fixture.0.join("runtime.exe").into()), &[]).unwrap();
        assert_eq!(command.get_program(), fixture.0.join("runtime.exe"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [fixture.0.join("server.mjs")]
        );
        assert!(mcp_command(&fixture.0, None, &[]).is_err());
        assert!(mcp_command(&fixture.0, Some("node".into()), &[]).is_err());
    }

    #[test]
    fn mcp_accepts_only_the_host_task_id_after_the_fixed_script() {
        let fixture = Fixture::new();
        let executable = Some(fixture.0.join("runtime.exe").into());
        let args = ["--task-id".into(), "run-中文 & % '".into()];
        let command = mcp_command(&fixture.0, executable.clone(), &args).unwrap();
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                fixture.0.join("server.mjs").as_os_str(),
                args[0].as_os_str(),
                args[1].as_os_str()
            ]
        );
        for invalid_args in [
            vec!["--inspect".into()],
            vec!["--task-id".into()],
            vec!["--task-id".into(), "".into()],
            vec!["--task-id".into(), "x".repeat(257).into()],
            vec!["--task-id".into(), "bad\0id".into()],
            vec!["--task-id".into(), "task".into(), "extra".into()],
        ] {
            assert!(mcp_command(&fixture.0, executable.clone(), &invalid_args).is_err());
        }
    }

    #[test]
    #[cfg(unix)]
    fn linked_configuration_is_rejected() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("outside.json"), fixture.config().to_string()).unwrap();
        std::os::unix::fs::symlink(
            fixture.0.join("outside.json"),
            fixture.0.join("native-launcher.json"),
        )
        .unwrap();
        assert!(native_command(&fixture.0, &args()).is_err());
    }
}
