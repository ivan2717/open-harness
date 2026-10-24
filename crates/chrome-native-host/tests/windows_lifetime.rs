#![cfg(windows)]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        TerminateProcess, WaitForSingleObject,
    },
};

// This executable deliberately behaves like an MCP server, its owned helper,
// or a user's Chrome. No real browser/profile is touched by this test.
const RUNTIME: &str = r#"
use std::{env, fs, io::{self, BufRead, Read, Write}, process::{self, Command, Stdio}, thread, time::Duration};
fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    let role = if args == ["--no-startup-window"] { "chrome" }
        else if args == ["--owned-child"] { "owned" } else { "runtime" };
    fs::write(env::var(format!("FIXTURE_{}_PID", role.to_uppercase())).unwrap(), process::id().to_string()).unwrap();
    if role != "runtime" { loop { thread::sleep(Duration::from_secs(1)); } }
    assert!(args.len() == 1 && args[0].ends_with("server.mjs"));
    let mut stdin = io::stdin().lock();
    let mut line = String::new();
    stdin.read_line(&mut line).unwrap();
    assert_eq!(line, "start\n");
    let result = Command::new(env::var_os("FIXTURE_LAUNCHER").unwrap())
        .arg("--launch-chrome").arg(env::var_os("FIXTURE_CHROME").unwrap())
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(result.success());
    Command::new(env::current_exe().unwrap()).arg("--owned-child")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    println!("ready"); io::stdout().flush().unwrap();
    stdin.read_to_end(&mut Vec::new()).unwrap();
}
"#;

struct OwnedProcess(HANDLE);

impl OwnedProcess {
    fn open(pid: u32) -> Self {
        // SAFETY: only PIDs recorded by our private fixture are opened, and the
        // returned owned handle is closed once in Drop.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        assert!(!handle.is_null(), "could not inspect fixture process {pid}");
        Self(handle)
    }

    fn alive(&self) -> bool {
        // SAFETY: this wrapper retains a valid process handle.
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_TIMEOUT }
    }

    fn wait(&self, milliseconds: u32) -> bool {
        // SAFETY: this wrapper retains a valid process handle.
        unsafe { WaitForSingleObject(self.0, milliseconds) == WAIT_OBJECT_0 }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // SAFETY: these handles refer exclusively to this test's fixture
        // processes; cleanup must run even when an assertion fails.
        unsafe {
            TerminateProcess(self.0, 1);
            WaitForSingleObject(self.0, 5_000);
            CloseHandle(self.0);
        }
    }
}

struct Fixture {
    directory: PathBuf,
    launcher: Option<Child>,
}

impl Fixture {
    fn pid(&self, role: &str) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(value) = fs::read_to_string(self.directory.join(format!("{role}.pid")))
                && let Ok(pid) = value.parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "missing {role} fixture PID");
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.launcher {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Cover failures before the test acquired process handles.
        for role in ["chrome", "owned", "runtime"] {
            if let Ok(value) = fs::read_to_string(self.directory.join(format!("{role}.pid")))
                && let Ok(pid) = value.parse::<u32>()
            {
                // SAFETY: PID files are written solely by private test fixtures.
                unsafe {
                    let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
                    if !handle.is_null() {
                        TerminateProcess(handle, 1);
                        WaitForSingleObject(handle, 5_000);
                        CloseHandle(handle);
                    }
                }
            }
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn mcp_shutdown_reaps_owned_children_but_keeps_explicitly_launched_chrome_alive() {
    let mut fixture = Fixture {
        directory: std::env::temp_dir().join(format!(
            "openharness-lifetime-中文 & 100%-{}",
            std::process::id()
        )),
        launcher: None,
    };
    fs::create_dir_all(&fixture.directory).unwrap();
    let runtime_source = fixture.directory.join("fixture.rs");
    let runtime = fixture.directory.join("runtime.exe");
    let chrome = fixture.directory.join("chrome.exe");
    let launcher = fixture.directory.join("launch.exe");
    fs::write(&runtime_source, RUNTIME).unwrap();
    let compiled = Command::new("rustc")
        .args(["--edition=2024", "--crate-name", "lifetime_fixture"])
        .arg(&runtime_source)
        .arg("-o")
        .arg(&runtime)
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{:?}", compiled.stderr);
    fs::copy(&runtime, &chrome).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_openharness-chrome-host"), &launcher).unwrap();
    fs::write(fixture.directory.join("server.mjs"), "fixture runtime").unwrap();
    let mut child = Command::new(&launcher)
        .arg("--mcp")
        .env("OPENHARNESS_NODE_BINARY", &runtime)
        .env("FIXTURE_LAUNCHER", &launcher)
        .env("FIXTURE_CHROME", &chrome)
        .env("FIXTURE_CHROME_PID", fixture.directory.join("chrome.pid"))
        .env("FIXTURE_OWNED_PID", fixture.directory.join("owned.pid"))
        .env("FIXTURE_RUNTIME_PID", fixture.directory.join("runtime.pid"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = send.send((result, line));
    });
    child.stdin.as_mut().unwrap().write_all(b"start\n").unwrap();
    fixture.launcher = Some(child);
    let (result, line) = receive.recv_timeout(Duration::from_secs(20)).unwrap();
    result.unwrap();
    assert_eq!(line.trim(), "ready");
    let user_browser = OwnedProcess::open(fixture.pid("chrome"));
    let owned_helper = OwnedProcess::open(fixture.pid("owned"));
    let runtime_process = OwnedProcess::open(fixture.pid("runtime"));
    assert!(user_browser.alive() && owned_helper.alive() && runtime_process.alive());

    // Production MCP shutdown closes stdin; the launcher returns when its
    // runtime exits and closes the exact same production Job wrapper.
    let child = fixture.launcher.as_mut().unwrap();
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "MCP launcher did not stop");
        thread::sleep(Duration::from_millis(25));
    }
    assert!(runtime_process.wait(5_000));
    assert!(
        owned_helper.wait(5_000),
        "normal descendants must remain in the job"
    );
    assert!(
        user_browser.alive(),
        "closing MCP must not close the user's Chrome"
    );
}
