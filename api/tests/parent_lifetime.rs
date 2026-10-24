use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

struct Runtime {
    child: Child,
    directory: PathBuf,
}

impl Runtime {
    fn start(managed: bool) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "oh-parent-lifetime-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut command = Command::new(env!("CARGO_BIN_EXE_api"));
        command
            .env("OPENHARNESS_BIND", "127.0.0.1:0")
            .env("OPENHARNESS_DATA_DIR", &directory)
            .env("OPENHARNESS_API_TOKEN", "parent-lifetime-fixture")
            .env_remove("OPENHARNESS_EXIT_ON_STDIN_EOF")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if managed {
            command.env("OPENHARNESS_EXIT_ON_STDIN_EOF", "1");
        }
        let child = command.spawn().expect("start actual API binary");
        let mut runtime = Self { child, directory };
        let stdout = runtime.child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                    && value["event"] == "ready"
                {
                    let _ = sender.send(());
                    return;
                }
            }
        });
        receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("API ready");
        runtime
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn desktop_pipe_eof_requests_a_clean_runtime_exit() {
    let mut runtime = Runtime::start(true);
    let mut input = runtime.child.stdin.take().unwrap();
    input
        .write_all(b"data is not a shutdown command\n")
        .unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(runtime.child.try_wait().unwrap().is_none());
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = runtime.child.try_wait().unwrap() {
            assert!(status.success(), "graceful exit failed: {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "API did not stop after desktop EOF"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn standalone_server_does_not_exit_when_stdin_is_closed() {
    let mut runtime = Runtime::start(false);
    drop(runtime.child.stdin.take());
    thread::sleep(Duration::from_millis(250));
    assert!(runtime.child.try_wait().unwrap().is_none());
}
