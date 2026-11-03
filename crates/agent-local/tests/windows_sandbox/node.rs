use super::Fixture;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

pub(super) fn copy_runtime(fixture: &Fixture) -> PathBuf {
    // CI supplies the same patched runtime that goes into the installer. An
    // explicit override must exist; never silently fall back to a host Node.
    let source = match std::env::var_os("OPENHARNESS_TEST_NODE") {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(path.is_absolute(), "OPENHARNESS_TEST_NODE must be absolute");
            path
        }
        None => {
            let output = Command::new("node")
                .args(["-p", "process.execPath"])
                .output()
                .expect("Node is required for the native Windows runtime regression");
            assert!(output.status.success(), "cannot locate Node: {output:?}");
            PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        }
    };
    let program = fixture.0.join("runtime/node.exe");
    fs::copy(&source, &program)
        .unwrap_or_else(|error| panic!("cannot copy test Node {}: {error}", source.display()));
    program
}

pub(super) fn run(request: &serde_json::Value) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_openharness-sandbox"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    // A libuv pipe-creation loop happens before Node can observe its own
    // timeout. Enforce a deadline outside AppContainer, then close the helper's
    // kill-on-close job so a broken runtime cannot strand child processes.
    let deadline = Instant::now() + Duration::from_secs(45);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "Node AppContainer regression exceeded 45 seconds:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    child.wait_with_output().unwrap()
}
