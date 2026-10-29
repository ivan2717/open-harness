#![cfg(target_os = "macos")]
//! Explicit acceptance test using prepared desktop runtimes. It serves only
//! locally generated packages and never contacts a public registry.
//! Run with OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST pointing to a real manifest:
//! cargo test -p agent-local --test package_install_network -- --ignored --nocapture

use agent_local::{LocalTools, NetworkAccess, PermissionMode};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "openharness-package-acceptance-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for directory in ["workspace/outputs/node", "outside", "archive/package"] {
            fs::create_dir_all(path.join(directory)).unwrap();
        }
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Server {
    address: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn start(packages: BTreeMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let counter = requests.clone();
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = [0u8; 8192];
                    if let Ok(count) = stream.read(&mut request) {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let text = String::from_utf8_lossy(&request[..count]);
                        let path = text.split_whitespace().nth(1).unwrap_or_default();
                        let (status, body) = packages
                            .get(path)
                            .map(|bytes| ("200 OK", bytes.as_slice()))
                            .unwrap_or(("404 Not Found", b"fixture not found"));
                        let header = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body);
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn quoted(value: impl AsRef<str>) -> String {
    format!("'{}'", value.as_ref().replace('\'', "'\"'\"'"))
}
fn invocation(runtime: &Value) -> String {
    std::iter::once(runtime["executable"].as_str().unwrap())
        .chain(
            runtime["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap()),
        )
        .map(quoted)
        .collect::<Vec<_>>()
        .join(" ")
}

fn wheel() -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let files = [
        ("openharness_fixture.py", "VALUE = 'PYTHON_FIXTURE_OK'\n"),
        (
            "openharness_fixture-1.0.dist-info/METADATA",
            "Metadata-Version: 2.1\nName: openharness-fixture\nVersion: 1.0\n",
        ),
        (
            "openharness_fixture-1.0.dist-info/WHEEL",
            "Wheel-Version: 1.0\nGenerator: openharness-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        ),
    ];
    for (name, content) in files {
        archive
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(content.as_bytes()).unwrap();
    }
    archive
        .start_file(
            "openharness_fixture-1.0.dist-info/RECORD",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    let record = files
        .iter()
        .map(|(name, _)| format!("{name},,\n"))
        .collect::<String>()
        + "openharness_fixture-1.0.dist-info/RECORD,,\n";
    archive.write_all(record.as_bytes()).unwrap();
    archive.finish().unwrap().into_inner()
}

async fn run(tools: &LocalTools, command: String, network: NetworkAccess) -> Value {
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        worker.execute(ToolCall { id: "package-acceptance".into(), name: "run_command".into(),
            arguments: json!({"command":command,"network_access":network,"access":"workspace_write",
                "yield_time_ms":30000,"timeout_seconds":if network == NetworkAccess::Denied { 5 } else { 25 }}).to_string() }).await
    });
    if network == NetworkAccess::Allowed {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(approval) = tools.review().unwrap().approvals.first() {
                    assert_eq!(approval.network_access, network);
                    tools.approve(&approval.id, true).unwrap();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("missing approval");
    }
    serde_json::from_str(&task.await.unwrap().unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "requires prepared desktop Node/Python runtime manifest and native macOS sandbox"]
async fn pip_and_pnpm_install_only_after_network_approval_and_stay_in_the_workspace() {
    let manifest_path = std::env::var_os("OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST")
        .expect("set OPENHARNESS_WORKSPACE_RUNTIME_MANIFEST to the prepared desktop runtimes");
    let manifest: Value = serde_json::from_slice(&fs::read(manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["python"]["source"], "bundled");
    assert_eq!(manifest["node"]["source"], "bundled");
    assert_eq!(manifest["node"]["package_manager"]["name"], "pnpm");
    let python = invocation(&manifest["python"]);
    let node = invocation(&manifest["node"]);
    let pnpm = invocation(&manifest["node"]["package_manager"]);
    let fixture = Fixture::new();
    let root = fixture.0.join("workspace");
    fs::write(
        root.join("outputs/node/package.json"),
        r#"{"name":"openharness-acceptance","version":"1.0.0","private":true}"#,
    )
    .unwrap();
    fs::write(
        fixture.0.join("archive/package/package.json"),
        r#"{"name":"openharness-fixture","version":"1.0.0","main":"index.js"}"#,
    )
    .unwrap();
    fs::write(
        fixture.0.join("archive/package/index.js"),
        "module.exports = 'NODE_FIXTURE_OK'\n",
    )
    .unwrap();
    let tarball = fixture.0.join("fixture.tgz");
    assert!(
        Command::new("/usr/bin/tar")
            .args(["-czf"])
            .arg(&tarball)
            .args(["-C"])
            .arg(fixture.0.join("archive"))
            .arg("package")
            .status()
            .unwrap()
            .success()
    );
    let server = Server::start(BTreeMap::from([
        ("/openharness_fixture-1.0-py3-none-any.whl".into(), wheel()),
        ("/fixture.tgz".into(), fs::read(tarball).unwrap()),
    ]));
    let tools = LocalTools::open(
        root.clone(),
        fixture.0.join("state"),
        PermissionMode::WorkspaceWrite,
    )
    .unwrap();
    let wheel_url = format!(
        "{}/openharness_fixture-1.0-py3-none-any.whl",
        server.address
    );
    let pip = format!(
        "{python} -m pip --isolated --disable-pip-version-check --no-cache-dir install --no-index --no-deps --retries 0 --timeout 2 --target outputs/python {}",
        quoted(&wheel_url)
    );
    let result = run(&tools, pip.clone(), NetworkAccess::Denied).await;
    assert_eq!(result["status"], "failed", "pip without network: {result}");
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    let result = run(&tools, pip, NetworkAccess::Allowed).await;
    assert_eq!(result["status"], "completed", "pip with network: {result}");
    assert!(root.join("outputs/python/openharness_fixture.py").is_file());
    println!("pip: denied network failed; approved network installed local wheel");

    let install = format!(
        "cd outputs/node && {pnpm} add {} --ignore-scripts --reporter=append-only --store-dir .pnpm-store --config.cache-dir=.pnpm-cache --config.state-dir=.pnpm-state --registry={} --config.update-notifier=false --fetch-retries=0 --fetch-timeout=2000",
        quoted(format!("{}/fixture.tgz", server.address)),
        quoted(&server.address)
    );
    let before = server.requests.load(Ordering::SeqCst);
    let result = run(&tools, install.clone(), NetworkAccess::Denied).await;
    // pnpm's tarball HEAD resolver retries EPERM independently of fetch-retries.
    // A bounded deadline stops those retries; the OS error and zero HTTP
    // requests establish the network denial, not the timeout alone.
    assert!(
        matches!(result["status"].as_str(), Some("failed" | "timed_out")),
        "pnpm without network: {result}"
    );
    assert!(
        result["output"].as_str().unwrap().contains("EPERM"),
        "{result}"
    );
    assert_eq!(server.requests.load(Ordering::SeqCst), before);
    let result = run(&tools, install, NetworkAccess::Allowed).await;
    assert_eq!(result["status"], "completed", "pnpm with network: {result}");
    println!("pnpm: denied network failed; approved network installed local tarball");

    let pycheck = "import sys;sys.path.insert(0,'outputs/python');import openharness_fixture;print(openharness_fixture.VALUE)";
    let jscheck = "console.log(require('./outputs/node/node_modules/openharness-fixture'))";
    let result = run(
        &tools,
        format!(
            "{python} -c {} && {node} -e {}",
            quoted(pycheck),
            quoted(jscheck)
        ),
        NetworkAccess::Denied,
    )
    .await;
    assert_eq!(result["status"], "completed", "installed imports: {result}");
    let output = result["output"].as_str().unwrap();
    assert!(
        output.contains("PYTHON_FIXTURE_OK") && output.contains("NODE_FIXTURE_OK"),
        "{output}"
    );

    // Network approval must not allow writing the runtime or another host path.
    let runtime_root = Path::new(manifest["python"]["executable"].as_str().unwrap())
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let shared_target =
        runtime_root.join(format!("openharness-install-probe-{}", std::process::id()));
    let outside = fixture.0.join("outside/python");
    for target in [&shared_target, &outside] {
        assert!(!target.exists());
        let command = format!(
            "{python} -m pip --isolated --disable-pip-version-check --no-cache-dir install --no-index --no-deps --retries 0 --timeout 2 --target {} {}",
            quoted(target.to_string_lossy()),
            quoted(&wheel_url)
        );
        let result = run(&tools, command, NetworkAccess::Allowed).await;
        assert_eq!(result["status"], "failed", "unapproved target: {result}");
        assert!(
            !target.exists(),
            "sandbox wrote outside the workspace: {}",
            target.display()
        );
    }
    println!(
        "Python/Node imports verified; approved network still blocks shared-runtime and host-directory installation"
    );
}
