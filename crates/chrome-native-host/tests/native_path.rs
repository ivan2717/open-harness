use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "oh-native-path-process-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn setup_query_returns_only_a_utf8_json_path_without_executing_it() {
    let fixture = Fixture::new();
    let path = fixture.0.join("中文 & host.exe");
    fs::write(&path, b"not an executable").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_openharness-chrome-host"))
        .arg("--native-path")
        .arg(&path)
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result.stderr);
    assert!(result.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<String>(&result.stdout).unwrap(),
        path.to_str().unwrap(),
    );
    assert_eq!(result.stdout.last(), Some(&b'\n'));
    assert_eq!(fs::read(&path).unwrap(), b"not an executable");
}

#[test]
fn invalid_query_writes_no_stdout_and_does_not_echo_its_argument() {
    let fixture = Fixture::new();
    let path = fixture.0.join("private-sentinel-missing.exe");
    let result = Command::new(env!("CARGO_BIN_EXE_openharness-chrome-host"))
        .arg("--native-path")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(stderr.contains("Chrome cannot launch this path safely"));
    assert!(!stderr.contains("private-sentinel"));
}
