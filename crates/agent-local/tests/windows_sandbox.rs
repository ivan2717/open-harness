#![cfg(windows)]
//! Native enforcement tests. These run on Windows, never a mocked subprocess.
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    Security::{
        GetTokenInformation, TOKEN_GROUPS, TOKEN_QUERY, TokenCapabilities, TokenIsAppContainer,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "openharness-native-sandbox-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("workspace/.git")).unwrap();
        fs::create_dir_all(root.join("outside")).unwrap();
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::create_dir_all(root.join("scratch")).unwrap();
        fs::create_dir_all(root.join("other-scratch")).unwrap();
        fs::create_dir_all(root.join("workspace/managed-runtime")).unwrap();
        fs::write(
            root.join("workspace/managed-runtime/library.txt"),
            "overlapping runtime",
        )
        .unwrap();
        fs::write(root.join("outside/secret.txt"), "outside secret").unwrap();
        fs::write(root.join("workspace/.env"), "protected secret").unwrap();
        fs::write(root.join("workspace/.git/config"), "git metadata").unwrap();
        fs::write(root.join("workspace/input.txt"), "project input").unwrap();
        fs::write(root.join("other-scratch/input.txt"), "other command").unwrap();
        fs::write(root.join("runtime/library.txt"), "shared library").unwrap();
        fs::hard_link(
            root.join("outside/secret.txt"),
            root.join("workspace/linked.txt"),
        )
        .unwrap();
        // Junction creation does not require the symlink privilege/admin mode.
        let status = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/j"])
            .arg(root.join("workspace/junction"))
            .arg(root.join("outside"))
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "native junction fixture creation failed");
        Self(fs::canonicalize(root).unwrap())
    }
    fn run(&self, allowed: bool) -> std::process::Output {
        self.spawn(allowed, false).wait_with_output().unwrap()
    }
    fn spawn(&self, allowed: bool, pause: bool) -> std::process::Child {
        self.spawn_with_access(allowed, pause, false)
    }
    fn spawn_with_access(
        &self,
        allowed: bool,
        pause: bool,
        read_only: bool,
    ) -> std::process::Child {
        self.spawn_with_probe(allowed, pause, read_only, "sandbox_probe")
    }
    fn spawn_with_probe(
        &self,
        allowed: bool,
        pause: bool,
        read_only: bool,
        probe: &str,
    ) -> std::process::Child {
        let program = std::env::current_exe().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_openharness-sandbox"))
            .env("OPENHARNESS_SANDBOX_PROBE_ROOT", &self.0)
            .env(
                "OPENHARNESS_SANDBOX_PROBE_READ_ONLY",
                if read_only { "1" } else { "0" },
            )
            .env(
                "OPENHARNESS_SANDBOX_PROBE_PAUSE",
                if pause { "1" } else { "0" },
            )
            .env(
                "OPENHARNESS_SANDBOX_PROBE_NETWORK",
                if allowed { "allowed" } else { "denied" },
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = json!({"version":1,"program":program,"args":["--exact",probe,"--nocapture"],
            "cwd":self.0.join("workspace"),"workspace":self.0.join("workspace"),
            "read_paths":[program,self.0.join("runtime"),self.0.join("workspace/managed-runtime")],
            "write_paths":[self.0.join(if read_only { "scratch" } else { "workspace" })],
            "denied_paths":[],"network_allowed":allowed});
        writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
        child
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir(self.0.join("workspace/junction"));
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn default_appcontainer_restricts_files_credentials_links_and_network() {
    let fixture = Fixture::new();
    let output = fixture.run(false);
    assert!(
        output.status.success(),
        "helper failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native sandbox verified"));
    assert_eq!(
        fs::read_to_string(fixture.0.join("workspace/output.txt")).unwrap(),
        "allowed"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("outside/secret.txt")).unwrap(),
        "outside secret"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("runtime/library.txt")).unwrap(),
        "shared library"
    );
    // Cleanup must not revoke the user's ordinary write permissions.
    fs::write(fixture.0.join("workspace/output.txt"), "after cleanup").unwrap();
}

#[test]
fn approved_network_adds_capabilities_without_removing_filesystem_isolation() {
    let fixture = Fixture::new();
    let output = fixture.run(true);
    assert!(
        output.status.success(),
        "helper failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn read_only_workspace_can_write_only_its_external_scratch() {
    let fixture = Fixture::new();
    let output = fixture
        .spawn_with_access(false, false, true)
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "helper failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native sandbox verified"));
    assert_eq!(
        fs::read_to_string(fixture.0.join("workspace/input.txt")).unwrap(),
        "project input"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("scratch/output.txt")).unwrap(),
        "scratch output"
    );
    assert!(!fixture.0.join("workspace/output.txt").exists());
    assert!(!fixture.0.join("other-scratch/output.txt").exists());
    fs::write(
        fixture.0.join("workspace/input.txt"),
        "host remains writable",
    )
    .unwrap();
}

#[test]
fn malformed_request_fails_closed() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_openharness-sandbox"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{{\"version\":999}}").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(125));
}

#[test]
fn read_only_commands_do_not_inherit_access_to_files_created_after_the_scan() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn_with_probe(false, false, true, "readonly_late_files_probe");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !fixture.0.join("scratch/ready").is_file() {
        if std::time::Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "probe never became ready: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(fixture.0.join("workspace/later.txt"), "new ordinary file").unwrap();
    fs::write(fixture.0.join("workspace/.env.after"), "new secret").unwrap();
    fs::write(fixture.0.join("workspace/private.pem"), "new private key").unwrap();
    fs::write(fixture.0.join("scratch/go"), "go").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "late file isolation failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let output = fixture
        .spawn_with_probe(
            false,
            false,
            true,
            "readonly_late_files_after_refresh_probe",
        )
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "next command scan failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn readonly_late_files_probe() {
    let Some(root) = std::env::var_os("OPENHARNESS_SANDBOX_PROBE_ROOT").map(PathBuf::from) else {
        return;
    };
    assert_eq!(
        fs::read_to_string(root.join("workspace/input.txt")).unwrap(),
        "project input"
    );
    fs::write(root.join("scratch/ready"), "ready").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !root.join("scratch/go").is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "host did not create late files"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for name in ["later.txt", ".env.after", "private.pem"] {
        let error = fs::read(root.join("workspace").join(name)).unwrap_err();
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{name}: {error}"
        );
    }
}

#[test]
fn readonly_late_files_after_refresh_probe() {
    let Some(root) = std::env::var_os("OPENHARNESS_SANDBOX_PROBE_ROOT").map(PathBuf::from) else {
        return;
    };
    assert_eq!(
        fs::read_to_string(root.join("workspace/later.txt")).unwrap(),
        "new ordinary file"
    );
    for name in [".env.after", "private.pem"] {
        let error = fs::read(root.join("workspace").join(name)).unwrap_err();
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{name}: {error}"
        );
    }
}

#[test]
fn sandbox_probe() {
    let Some(root) = std::env::var_os("OPENHARNESS_SANDBOX_PROBE_ROOT").map(PathBuf::from) else {
        return;
    };
    let allowed = std::env::var("OPENHARNESS_SANDBOX_PROBE_NETWORK").unwrap() == "allowed";
    unsafe {
        let mut token = std::ptr::null_mut();
        assert_ne!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
            0
        );
        let mut value = 0u32;
        let mut size = 0;
        assert_ne!(
            GetTokenInformation(
                token,
                TokenIsAppContainer,
                std::ptr::from_mut(&mut value).cast(),
                4,
                &mut size
            ),
            0
        );
        assert_eq!(value, 1, "command escaped AppContainer");
        GetTokenInformation(token, TokenCapabilities, std::ptr::null_mut(), 0, &mut size);
        let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        assert_ne!(
            GetTokenInformation(
                token,
                TokenCapabilities,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size
            ),
            0
        );
        let groups = &*buffer.as_ptr().cast::<TOKEN_GROUPS>();
        assert_eq!(groups.GroupCount, if allowed { 2 } else { 0 });
        CloseHandle(token);
    }
    assert_eq!(
        fs::read_to_string(root.join("workspace/input.txt")).unwrap(),
        "project input"
    );
    if std::env::var("OPENHARNESS_SANDBOX_PROBE_READ_ONLY").as_deref() == Ok("1") {
        assert!(fs::write(root.join("workspace/output.txt"), "new project file").is_err());
        assert!(fs::write(root.join("workspace/input.txt"), "project mutation").is_err());
        assert!(fs::remove_file(root.join("workspace/input.txt")).is_err());
        fs::write(root.join("scratch/output.txt"), "scratch output").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("scratch/output.txt")).unwrap(),
            "scratch output"
        );
        assert!(fs::write(root.join("other-scratch/output.txt"), "other command").is_err());
        assert!(fs::read(root.join("other-scratch/input.txt")).is_err());
    } else {
        fs::write(root.join("workspace/output.txt"), "allowed").unwrap();
    }
    assert!(
        fs::read(root.join("workspace/.env")).is_err(),
        "secret was readable"
    );
    assert!(fs::write(root.join("workspace/.git/config"), "changed").is_err());
    assert_eq!(
        fs::read_to_string(root.join("workspace/.git/config")).unwrap(),
        "git metadata"
    );
    assert!(fs::write(root.join("outside/new.txt"), "outside").is_err());
    assert!(fs::write(root.join("workspace/linked.txt"), "hardlink escape").is_err());
    assert!(
        fs::read(root.join("workspace/linked.txt")).is_err(),
        "hardlink could expose a protected external target"
    );
    assert!(fs::write(root.join("workspace/junction/new.txt"), "junction escape").is_err());
    assert!(fs::read(root.join("workspace/junction/secret.txt")).is_err());
    assert_eq!(
        fs::read_to_string(root.join("runtime/library.txt")).unwrap(),
        "shared library"
    );
    assert!(fs::write(root.join("runtime/library.txt"), "runtime mutation").is_err());
    assert_eq!(
        fs::read_to_string(root.join("workspace/managed-runtime/library.txt")).unwrap(),
        "overlapping runtime"
    );
    assert!(
        fs::write(
            root.join("workspace/managed-runtime/library.txt"),
            "overlap mutation"
        )
        .is_err()
    );
    if !allowed {
        // Assert the OS denial, not merely a timeout/unavailable test server.
        let error = std::net::TcpStream::connect_timeout(
            &"1.1.1.1:443".parse().unwrap(),
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(10013),
            "expected WSAEACCES from AppContainer: {error}"
        );
    }
    if std::env::var("OPENHARNESS_SANDBOX_PROBE_PAUSE").as_deref() == Ok("1") {
        fs::write(root.join("workspace/ready"), "ready").unwrap();
        std::thread::sleep(Duration::from_secs(120));
        panic!("sandbox child should have been terminated with its helper");
    }
    println!("native sandbox verified");
}

#[test]
fn killed_helper_leases_are_recovered_on_the_next_command() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(false, true);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !fixture.0.join("workspace/ready").exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "sandbox helper exited before ready"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "sandbox child did not become ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let state = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
        .join("OpenHarness/command-sandbox");
    let journal = fs::read_dir(&state)
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.path())
        .find(|path| {
            fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|value| value["pid"] == child.id())
        })
        .expect("active cleanup journal");
    child.kill().unwrap();
    child.wait().unwrap();
    let next = Fixture::new();
    let output = next.run(false);
    assert!(
        output.status.success(),
        "recovery failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !journal.exists(),
        "stale per-command ACL/profile cleanup was not completed"
    );
    fs::write(fixture.0.join("workspace/output.txt"), "after recovery").unwrap();
}
