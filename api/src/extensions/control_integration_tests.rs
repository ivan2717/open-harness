//! Host-level regression coverage for control/client revocation consistency.
//! Attach with `#[cfg(test)] mod control_integration_tests;` from extensions/mod.rs.
use super::*;
use agent_mcp::McpTransport;
use serde_json::json;
use std::sync::atomic::AtomicU64;

static NEXT_CONTROL_TEST: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "harness-control-host-{}-{}",
            std::process::id(),
            NEXT_CONTROL_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn host(&self) -> Arc<ExtensionHost> {
        let host = ExtensionHost::open(&self.0).unwrap();
        host.save(ExtensionConfig {
            mcp_servers: vec![server("browser"), server("desktop")],
            ..Default::default()
        })
        .unwrap();
        host
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn server(id: &str) -> McpServerConfig {
    McpServerConfig {
        id: id.into(),
        name: Some(id.into()),
        enabled: true,
        transport: McpTransport::Stdio {
            command: "control-test-must-never-be-started".into(),
            args: Vec::new(),
            env_vars: Vec::new(),
        },
    }
}
fn app_scope() -> ControlScope {
    ControlScope::Application {
        bundle_id: "dev.openharness.fixture".into(),
        pid: 1234,
    }
}
fn grant(host: &ExtensionHost, run: &str, server_id: &str, scope: ControlScope) {
    let server = host.server(server_id).unwrap();
    host.controls
        .lock()
        .unwrap()
        .grant(
            run,
            server_id,
            server_id,
            &serde_json::to_string(&server).unwrap(),
            scope,
            true,
        )
        .unwrap();
}

#[tokio::test]
async fn disabling_one_server_never_releases_desktop_while_its_other_client_survives() {
    let fixture = Fixture::new();
    let host = fixture.host();
    grant(&host, "run-a", "browser", ControlScope::Browser);
    grant(&host, "run-a", "desktop", app_scope());
    let desktop = host
        .client("run-a", &host.server("desktop").unwrap())
        .unwrap();
    let mut updated = host.settings().config;
    updated
        .mcp_servers
        .iter_mut()
        .find(|server| server.id == "browser")
        .unwrap()
        .enabled = false;
    host.save(updated).unwrap();

    let retained = host
        .clients
        .read()
        .unwrap()
        .contains_key(&("run-a".into(), "desktop".into()));
    if retained {
        assert!(
            host.controls
                .lock()
                .unwrap()
                .acquire_desktop("run-b")
                .is_err(),
            "an unaffected desktop client still exists: its exclusivity must survive another server's configuration change"
        );
    } else {
        let error = desktop.discover().await.unwrap_err().to_string();
        assert!(
            error.contains("revoked"),
            "removing the cache entry must also revoke outstanding desktop clones: {error}"
        );
    }
}

#[tokio::test]
async fn revoking_a_run_invalidates_all_clones_and_rejects_old_epoch_without_launching() {
    let fixture = Fixture::new();
    let host = fixture.host();
    grant(&host, "run-a", "browser", ControlScope::Browser);
    grant(&host, "run-a", "desktop", app_scope());
    let config = host.server("desktop").unwrap();
    let epoch = host.controls.lock().unwrap().epoch("run-a");
    let desktop = host
        .client_with_epoch("run-a", &config, Some(epoch))
        .unwrap();
    let browser = host
        .client("run-a", &host.server("browser").unwrap())
        .unwrap();
    host.revoke_run("run-a");
    assert!(host.clients.read().unwrap().is_empty());
    assert!(host.controls.lock().unwrap().list("run-a").is_empty());
    assert!(
        host.client_with_epoch("run-a", &config, Some(epoch))
            .is_err()
    );
    for client in [desktop, browser] {
        let error = client.discover().await.unwrap_err().to_string();
        assert!(
            error.contains("revoked"),
            "revoked clients must not reconnect: {error}"
        );
    }
    assert!(
        host.controls
            .lock()
            .unwrap()
            .acquire_desktop("run-b")
            .is_ok()
    );
}

#[tokio::test]
async fn approval_cannot_cross_runs_or_upgrade_an_unscoped_operation() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let queue = host.approvals.clone();
    let task = tokio::spawn(async move {
        queue
            .request_scoped(
                "run-a",
                ExtensionApproval {
                    id: String::new(),
                    server_id: "desktop".into(),
                    server_name: "Fixture".into(),
                    operation: "computer_status".into(),
                    arguments: json!({}),
                    source_run_id: None,
                    task_title: None,
                    session_scope: None,
                },
            )
            .await
    });
    let pending = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(pending) = host.approvals.list("run-a").pop() {
                break pending;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        host.approvals
            .decide_scoped("run-b", &pending.id, true, false)
            .is_err()
    );
    assert!(
        host.approvals
            .decide_scoped("run-a", &pending.id, true, true)
            .is_err()
    );
    host.revoke_run("run-a");
    assert!(task.await.unwrap().is_err());
    assert!(host.approvals.list("run-a").is_empty());
    assert!(host.clients.read().unwrap().is_empty());
}

#[tokio::test]
async fn changing_a_bound_server_revokes_its_clones_and_cannot_reuse_old_configuration() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let before = host.server("desktop").unwrap();
    grant(&host, "run-a", "desktop", app_scope());
    let client = host.client("run-a", &before).unwrap();
    let mut config = host.settings().config;
    if let McpTransport::Stdio { args, .. } = &mut config
        .mcp_servers
        .iter_mut()
        .find(|server| server.id == "desktop")
        .unwrap()
        .transport
    {
        args.push("changed".into());
    }
    host.save(config).unwrap();
    assert!(host.client("run-a", &before).is_err());
    assert!(host.controls.lock().unwrap().list("run-a").is_empty());
    assert!(
        client
            .discover()
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked")
    );
}

fn control_package(root: &Path, id: &str, mode: &str, marker: &Path) -> PathBuf {
    let source = root.join(id);
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("plugin.json"),
        json!({"name":id,"version":"1.0.0"}).to_string(),
    )
    .unwrap();
    let server = if mode == "missing" {
        json!({"command":"control-test-must-never-be-started","args":[]})
    } else {
        fs::write(source.join("fixture.py"), r#"import json,sys,time,pathlib
mode,marker=sys.argv[1:3]
for line in sys.stdin:
    req=json.loads(line)
    if 'id' not in req: continue
    method=req.get('method')
    if method=='initialize':
        result={'protocolVersion':req['params']['protocolVersion'],'serverInfo':{'name':'control-fixture','version':'1'},'capabilities':{'tools':{}}}
    elif method=='tools/list':
        result={'tools':[{'name':'computer_select_app','description':'Fixture selection','inputSchema':{'type':'object'}}]}
    elif method=='tools/call':
        if mode=='serialized':
            with open(marker,'a',newline='\n') as output: output.write('start\n')
            time.sleep(0.1)
            with open(marker,'a',newline='\n') as output: output.write('end\n')
        else: pathlib.Path(marker).write_text('called')
        if mode=='hang': time.sleep(30)
        result={'isError': mode=='error','content':[{'type':'text','text':'fixture app selection refused' if mode=='error' else 'selected'}]}
        if req['params']['name']=='chrome_open' and mode.startswith('open_'):
            origin='https://redirect.example' if mode=='open_redirect' else 'https://example.com'
            meta={'created':True,'selected':True,'tab_id':0 if mode=='open_invalid' else 42,'origin':origin,'url':origin+'/task'}
            result={'content':[{'type':'text','text':json.dumps(meta)}]}
    else: result={}
    print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}),flush=True)
"#).unwrap();
        let python = if cfg!(windows) { "python" } else { "python3" };
        json!({"command":python,"args":["-u","${PLUGIN_ROOT}/fixture.py",mode,marker]})
    };
    fs::write(
        source.join("mcp.json"),
        json!({"mcpServers":{"computer":server}}).to_string(),
    )
    .unwrap();
    source
}
async fn pending_control(host: &ExtensionHost, run: &str) -> ExtensionApproval {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(pending) = host.approvals.list(run).pop() {
                break pending;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
fn select_task(
    host: Arc<ExtensionHost>,
    run: &str,
    server_id: &str,
) -> tokio::task::JoinHandle<io::Result<String>> {
    use agent_runtime::ToolExecutor;
    let tools = ExtensionTools::new(host, run.into(), None, None);
    let server_id = server_id.to_owned();
    tokio::spawn(async move {
        tools.execute(agent_runtime::ToolCall {
            id: "selection".into(), name: "call_mcp_tool".into(),
            arguments: json!({"server_id":server_id,"name":"computer_select_app","arguments":{"bundle_id":"dev.openharness.fixture","pid":1234}}).to_string(),
        }).await
    })
}

fn control_task(
    host: Arc<ExtensionHost>,
    run: &str,
    server_id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> tokio::task::JoinHandle<io::Result<String>> {
    use agent_runtime::ToolExecutor;
    let tools = ExtensionTools::new(host, run.into(), None, None);
    let call = agent_runtime::ToolCall {
        id: "control-fixture".into(),
        name: "call_mcp_tool".into(),
        arguments: json!({"server_id":server_id,"name":name,"arguments":arguments}).to_string(),
    };
    tokio::spawn(async move { tools.execute(call).await })
}

async fn complete_control(task: tokio::task::JoinHandle<io::Result<String>>) -> io::Result<String> {
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn trusted_metadata_skips_approval_but_external_names_do_not() {
    let fixture = Fixture::new();
    for (plugin, names) in [
        ("computer-use", ["computer_status", "computer_apps"]),
        ("chrome-use", ["chrome_status", "chrome_tabs"]),
    ] {
        let bundled = fixture.0.join(format!("bundled-{plugin}"));
        let source = control_package(&bundled, plugin, "ok", &fixture.0.join("called"));
        let host = ExtensionHost::open_with_bundled(
            &fixture.0.join(format!("state-{plugin}")),
            Some(bundled),
        )
        .unwrap();
        let installed = host.install_plugin(&source, None).unwrap();
        let server = &installed.plugins[0].mcp_servers[0].id;
        for name in names {
            assert!(
                complete_control(control_task(host.clone(), "r", server, name, json!({})))
                    .await
                    .is_ok()
            );
            assert!(host.approvals.list("r").is_empty());
            assert!(host.controls.lock().unwrap().list("r").is_empty());
        }
        host.shutdown().await;

        let external =
            ExtensionHost::open_with_bundled(&fixture.0.join(format!("external-{plugin}")), None)
                .unwrap();
        let installed = external.install_plugin(&source, None).unwrap();
        let server = &installed.plugins[0].mcp_servers[0].id;
        let task = control_task(external.clone(), "r", server, names[0], json!({}));
        let pending = pending_control(&external, "r").await;
        assert!(pending.session_scope.is_none());
        external.approvals.decide("r", &pending.id, false).unwrap();
        assert!(complete_control(task).await.is_err());
        external.shutdown().await;
    }
}

#[test]
fn chrome_layout_identity_is_host_bound_and_never_saved_to_server_configuration() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "chrome-use", "missing", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let original = &installed.plugins[0].mcp_servers[0];
    let first = host.connection_config("run-a", original);
    ExtensionTools::new(host.clone(), "child-a".into(), None, None)
        .with_authorization_scope("run-a");
    let child = host.connection_config("child-a", original);
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&child).unwrap()
    );
    let second = host.connection_config("run-b", original);
    let McpTransport::Stdio { args, .. } = &first.transport else {
        panic!("stdio expected")
    };
    assert_eq!(args, &["--task-id", "run-a"]);
    let McpTransport::Stdio { args, .. } = &second.transport else {
        panic!("stdio expected")
    };
    assert_eq!(args, &["--task-id", "run-b"]);
    assert_eq!(
        serde_json::to_value(host.server(&original.id).unwrap()).unwrap(),
        serde_json::to_value(original).unwrap(),
        "connection-only metadata must not change approval bindings or saved configuration"
    );
    host.revoke_run("run-a");
    assert_eq!(
        serde_json::to_value(host.connection_config("run-a", original)).unwrap(),
        serde_json::to_value(first).unwrap(),
        "visual identity survives client revocation without restoring grants"
    );
    assert!(host.controls.lock().unwrap().list("run-a").is_empty());

    let external = ExtensionHost::open_with_bundled(&fixture.0.join("external"), None).unwrap();
    let installed = external.install_plugin(&source, None).unwrap();
    let untrusted = &installed.plugins[0].mcp_servers[0];
    assert_eq!(
        serde_json::to_value(external.connection_config("run-a", untrusted)).unwrap(),
        serde_json::to_value(untrusted).unwrap(),
        "a matching third-party plugin name must not receive host identity arguments"
    );
    let manual = server("chrome");
    assert_eq!(
        serde_json::to_value(host.connection_config("run-a", &manual)).unwrap(),
        serde_json::to_value(manual).unwrap()
    );
}

#[tokio::test]
async fn application_switching_reuses_only_previously_approved_processes() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "computer-use", "ok", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    for (bundle, pid) in [("app.a", 11), ("app.b", 12)] {
        let task = control_task(
            host.clone(),
            "r",
            server,
            "computer_select_app",
            json!({"bundle_id":bundle,"pid":pid}),
        );
        let pending = pending_control(&host, "r").await;
        host.approvals
            .decide_scoped("r", &pending.id, true, true)
            .unwrap();
        assert!(complete_control(task).await.is_ok());
    }
    let task = control_task(
        host.clone(),
        "r",
        server,
        "computer_select_app",
        json!({"bundle_id":"app.a","pid":11}),
    );
    assert!(complete_control(task).await.is_ok());
    assert_eq!(
        host.controls.lock().unwrap().list("r")[0].scope,
        ControlScope::Application {
            bundle_id: "app.a".into(),
            pid: 11
        }
    );
    for pid in [13, 11] {
        if pid == 11 {
            host.revoke_run("r");
        }
        let task = control_task(
            host.clone(),
            "r",
            server,
            "computer_select_app",
            json!({"bundle_id":"app.a","pid":pid}),
        );
        let pending = pending_control(&host, "r").await;
        host.approvals.decide("r", &pending.id, false).unwrap();
        assert!(complete_control(task).await.is_err());
    }
    host.shutdown().await;
}

#[tokio::test]
async fn chrome_tab_origin_scope_does_not_authorize_other_tabs_origins_or_opening_urls() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "chrome-use", "ok", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let task = control_task(
        host.clone(),
        "r",
        server,
        "chrome_select_tab",
        json!({"tab_id":7,"origin":"https://example.com"}),
    );
    let pending = pending_control(&host, "r").await;
    assert_eq!(
        pending.session_scope,
        Some(ControlScope::ChromeTab {
            tab_id: 7,
            origin: "https://example.com".into()
        })
    );
    host.approvals
        .decide_scoped("r", &pending.id, true, true)
        .unwrap();
    assert!(complete_control(task).await.is_ok());
    assert!(
        complete_control(control_task(
            host.clone(),
            "r",
            server,
            "chrome_snapshot",
            json!({"tab_id":7})
        ))
        .await
        .is_ok()
    );
    assert!(
        complete_control(control_task(
            host.clone(),
            "r",
            server,
            "chrome_snapshot",
            json!({"tab_id":8})
        ))
        .await
        .is_err()
    );
    for (name, args, scoped) in [
        (
            "chrome_select_tab",
            json!({"tab_id":7,"origin":"https://other.example"}),
            true,
        ),
        (
            "chrome_select_tab",
            json!({"tab_id":8,"origin":"https://example.com"}),
            true,
        ),
        (
            "chrome_open",
            json!({"url":"https://example.com/new"}),
            true,
        ),
    ] {
        let task = control_task(host.clone(), "r", server, name, args);
        let pending = pending_control(&host, "r").await;
        assert_eq!(pending.session_scope.is_some(), scoped);
        host.approvals.decide("r", &pending.id, false).unwrap();
        assert!(complete_control(task).await.is_err());
    }
    assert!(
        complete_control(control_task(
            host.clone(),
            "r",
            server,
            "chrome_release",
            json!({})
        ))
        .await
        .is_ok()
    );
    assert!(
        complete_control(control_task(
            host.clone(),
            "r",
            server,
            "chrome_snapshot",
            json!({})
        ))
        .await
        .is_err()
    );
    let task = control_task(
        host.clone(),
        "r",
        server,
        "chrome_select_tab",
        json!({"tab_id":7,"origin":"https://example.com"}),
    );
    let pending = pending_control(&host, "r").await;
    host.approvals.decide("r", &pending.id, false).unwrap();
    assert!(complete_control(task).await.is_err());
    host.shutdown().await;
}

#[tokio::test]
async fn creating_one_chrome_tab_can_grant_that_tab_but_never_a_new_origin_or_a_future_tab() {
    let fixture = Fixture::new();
    for (mode, session, success) in [
        ("open_ok", true, true),
        ("open_ok", false, true),
        ("open_redirect", true, false),
        ("open_invalid", true, false),
    ] {
        let label = format!("{mode}-{session}");
        let bundled = fixture.0.join(format!("bundled-{label}"));
        let source = control_package(
            &bundled,
            "chrome-use",
            mode,
            &fixture.0.join(format!("called-{label}")),
        );
        let host = ExtensionHost::open_with_bundled(
            &fixture.0.join(format!("state-{label}")),
            Some(bundled),
        )
        .unwrap();
        let installed = host.install_plugin(&source, None).unwrap();
        let server = &installed.plugins[0].mcp_servers[0].id;
        grant(
            &host,
            "r",
            server,
            ControlScope::ChromeTab {
                tab_id: 7,
                origin: "https://example.com".into(),
            },
        );
        let task = control_task(
            host.clone(),
            "r",
            server,
            "chrome_open",
            json!({"url":"https://EXAMPLE.com:443/task#section"}),
        );
        let pending = pending_control(&host, "r").await;
        assert_eq!(
            pending.session_scope,
            Some(ControlScope::ChromeNewTab {
                origin: "https://example.com".into()
            })
        );
        host.approvals
            .decide_scoped("r", &pending.id, true, session)
            .unwrap();
        assert_eq!(complete_control(task).await.is_ok(), success);
        if success && session {
            assert_eq!(
                host.controls.lock().unwrap().list("r")[0].scope,
                ControlScope::ChromeTab {
                    tab_id: 42,
                    origin: "https://example.com".into()
                }
            );
            assert!(
                complete_control(control_task(
                    host.clone(),
                    "r",
                    server,
                    "chrome_snapshot",
                    json!({})
                ))
                .await
                .is_ok()
            );
            let task = control_task(
                host.clone(),
                "r",
                server,
                "chrome_open",
                json!({"url":"https://example.com/second"}),
            );
            let pending = pending_control(&host, "r").await;
            host.approvals.decide("r", &pending.id, false).unwrap();
            assert!(complete_control(task).await.is_err());
        } else if success {
            let controls = host.controls.lock().unwrap().list("r");
            assert_eq!(controls.len(), 1);
            assert!(!controls[0].remembered);
            assert_eq!(
                controls[0].scope,
                ControlScope::ChromeTab {
                    tab_id: 42,
                    origin: "https://example.com".into()
                }
            );
            let task = control_task(host.clone(), "r", server, "chrome_snapshot", json!({}));
            let pending = pending_control(&host, "r").await;
            host.approvals.decide("r", &pending.id, false).unwrap();
            assert!(complete_control(task).await.is_err());
        } else {
            assert!(host.controls.lock().unwrap().list("r").is_empty());
            assert!(
                complete_control(control_task(
                    host.clone(),
                    "r",
                    server,
                    "chrome_snapshot",
                    json!({})
                ))
                .await
                .is_err()
            );
        }
        host.shutdown().await;
    }
}

#[tokio::test]
async fn failed_new_selection_releases_grant_client_and_desktop_lease() {
    for mode in ["missing", "error"] {
        let fixture = Fixture::new();
        let bundled = fixture.0.join("bundled");
        let source = control_package(&bundled, "computer-use", mode, &fixture.0.join("called"));
        let host =
            ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
        let installed = host.install_plugin(&source, None).unwrap();
        let server_id = &installed.plugins[0].mcp_servers[0].id;
        let task = select_task(host.clone(), "run-a", server_id);
        let pending = pending_control(&host, "run-a").await;
        assert!(matches!(
            pending.session_scope,
            Some(ControlScope::Application { .. })
        ));
        host.approvals
            .decide_scoped("run-a", &pending.id, true, true)
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err(), "fixture mode {mode} must fail");
        if mode == "error" {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("fixture app selection refused"),
                "the error fixture must return its intentional tool failure",
            );
        }
        assert!(
            host.controls.lock().unwrap().list("run-a").is_empty(),
            "failed {mode} selection retained a grant"
        );
        assert!(
            host.clients.read().unwrap().is_empty(),
            "failed {mode} selection retained a client"
        );
        assert!(
            host.controls
                .lock()
                .unwrap()
                .acquire_desktop("run-b")
                .is_ok(),
            "failed {mode} selection retained desktop ownership"
        );
        host.shutdown().await;
    }
}

#[tokio::test]
async fn dropping_an_inflight_new_selection_releases_desktop_lease() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let marker = fixture.0.join("called");
    let source = control_package(&bundled, "computer-use", "hang", &marker);
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server_id = &installed.plugins[0].mcp_servers[0].id;
    let task = select_task(host.clone(), "run-a", server_id);
    let pending = pending_control(&host, "run-a").await;
    host.approvals
        .decide_scoped("run-a", &pending.id, true, true)
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        host.controls
            .lock()
            .unwrap()
            .acquire_desktop("run-b")
            .is_err()
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        host.controls.lock().unwrap().list("run-a").is_empty(),
        "dropping a pending selection must roll back its new grant"
    );
    assert!(host.clients.read().unwrap().is_empty());
    assert!(
        host.controls
            .lock()
            .unwrap()
            .acquire_desktop("run-b")
            .is_ok()
    );
    host.shutdown().await;
}

#[tokio::test]
async fn third_party_control_name_is_not_a_session_permission_credential() {
    let fixture = Fixture::new();
    let source = control_package(
        &fixture.0.join("external"),
        "computer-use",
        "missing",
        &fixture.0.join("called"),
    );
    // CLI-style hosts without a configured bundled root accept the package as
    // an ordinary MCP integration, but never infer an app-scoped session grant.
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("cli"), None).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server_id = &installed.plugins[0].mcp_servers[0].id;
    assert!(host.control_plugin(server_id).is_none());
    let task = select_task(host.clone(), "run-a", server_id);
    let pending = pending_control(&host, "run-a").await;
    assert!(pending.session_scope.is_none());
    assert!(
        host.approvals
            .decide_scoped("run-a", &pending.id, true, true)
            .is_err()
    );
    host.approvals.decide("run-a", &pending.id, false).unwrap();
    assert!(task.await.unwrap().is_err());

    let bundled = fixture.0.join("bundled");
    let trusted = control_package(
        &bundled,
        "computer-use",
        "missing",
        &fixture.0.join("called"),
    );
    let desktop =
        ExtensionHost::open_with_bundled(&fixture.0.join("desktop"), Some(bundled)).unwrap();
    assert!(
        desktop.install_plugin(&source, None).is_err(),
        "same manifest name outside the fixed root must not impersonate a bundled package"
    );
    let installed = desktop.install_plugin(&trusted, None).unwrap();
    let id = &installed.plugins[0].mcp_servers[0].id;
    assert_eq!(desktop.control_plugin(id).as_deref(), Some("computer-use"));
    desktop.shutdown().await;
    host.shutdown().await;
}

#[tokio::test]
async fn unrelated_server_change_cannot_skip_pending_desktop_cleanup() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let marker = fixture.0.join("called");
    let source = control_package(&bundled, "computer-use", "hang", &marker);
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    host.save(ExtensionConfig {
        mcp_servers: vec![server("browser")],
        ..Default::default()
    })
    .unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server_id = &installed.plugins[0].mcp_servers[0].id;
    grant(&host, "run-a", "browser", ControlScope::Browser);
    let epoch = host.controls.lock().unwrap().epoch("run-a");
    let task = select_task(host.clone(), "run-a", server_id);
    let pending = pending_control(&host, "run-a").await;
    host.approvals
        .decide_scoped("run-a", &pending.id, true, true)
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    // Only the browser is disabled. The native selection is still in flight.
    let mut config = host.settings().config;
    config.mcp_servers[0].enabled = false;
    host.save(config).unwrap();
    assert!(
        host.controls
            .lock()
            .unwrap()
            .check_epoch("run-a", epoch)
            .is_err()
    );
    assert_eq!(host.controls.lock().unwrap().list("run-a").len(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        host.controls.lock().unwrap().list("run-a").is_empty(),
        "the pending computer grant must be removed even after an unrelated browser config changes"
    );
    assert!(host.clients.read().unwrap().is_empty());
    assert!(
        host.controls
            .lock()
            .unwrap()
            .acquire_desktop("run-b")
            .is_ok()
    );
    host.shutdown().await;
}

#[tokio::test]
async fn abandoned_selection_cannot_revoke_a_replacement_grant() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let config = host.server("desktop").unwrap();
    let binding = serde_json::to_string(&config).unwrap();
    let old_grant = host
        .controls
        .lock()
        .unwrap()
        .grant("run-a", "desktop", "Desktop", &binding, app_scope(), true)
        .unwrap();
    host.revoke_run("run-a");
    let replacement = host
        .controls
        .lock()
        .unwrap()
        .grant("run-a", "desktop", "Desktop", &binding, app_scope(), true)
        .unwrap();
    host.client("run-a", &config).unwrap();
    host.release_failed_control("run-a", "desktop", old_grant);
    assert!(
        host.controls
            .lock()
            .unwrap()
            .owns_grant("run-a", "desktop", replacement)
    );
    assert!(
        host.clients
            .read()
            .unwrap()
            .contains_key(&("run-a".into(), "desktop".into()))
    );
    assert!(
        host.controls
            .lock()
            .unwrap()
            .acquire_desktop("run-b")
            .is_err()
    );
    host.release_failed_control("run-a", "desktop", replacement);
    assert!(host.controls.lock().unwrap().list("run-a").is_empty());
    assert!(host.clients.read().unwrap().is_empty());
    assert!(
        host.controls
            .lock()
            .unwrap()
            .acquire_desktop("run-b")
            .is_ok()
    );
    host.shutdown().await;
}

#[tokio::test]
async fn children_inherit_only_successful_session_grants_and_keep_independent_clients() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "computer-use", "ok", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    for run in ["child-a", "child-b"] {
        ExtensionTools::new(host.clone(), run.into(), None, None).with_authorization_scope("root");
    }

    let parent = select_task(host.clone(), "root", server);
    let approval = pending_control(&host, "root").await;
    host.approvals.decide("root", &approval.id, true).unwrap();
    complete_control(parent).await.unwrap();
    // A one-shot selection does not propagate authority to another agent.
    let child = select_task(host.clone(), "child-a", server);
    let approval = pending_control(&host, "child-a").await;
    host.approvals
        .decide("child-a", &approval.id, false)
        .unwrap();
    assert!(complete_control(child).await.is_err());

    let parent = select_task(host.clone(), "root", server);
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_scoped("root", &approval.id, true, true)
        .unwrap();
    complete_control(parent).await.unwrap();
    for run in ["child-a", "child-b"] {
        complete_control(select_task(host.clone(), run, server))
            .await
            .unwrap();
        assert!(host.approvals.list(run).is_empty());
    }
    assert_eq!(host.clients.read().unwrap().len(), 3);
    let config = host.server(server).unwrap();
    let binding = serde_json::to_string(&config).unwrap();
    let child_client = host.client("child-a", &config).unwrap();
    host.revoke_run("child-a");
    assert!(
        host.controls
            .lock()
            .unwrap()
            .permits("root", server, &binding, Some(&app_scope()))
    );
    assert!(
        host.clients
            .read()
            .unwrap()
            .contains_key(&("root".into(), server.clone()))
    );
    assert!(
        child_client
            .discover()
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked")
    );
    // Other roots cannot reuse this permission.
    let outside = select_task(host.clone(), "outside", server);
    let approval = pending_control(&host, "outside").await;
    host.approvals
        .decide("outside", &approval.id, false)
        .unwrap();
    assert!(complete_control(outside).await.is_err());
    let sibling = host.client("child-b", &config).unwrap();
    let epoch = host.controls.lock().unwrap().epoch("child-b");
    host.revoke_run("root");
    assert!(
        host.controls
            .lock()
            .unwrap()
            .check_epoch("child-b", epoch)
            .is_err()
    );
    assert!(!host.controls.lock().unwrap().permits(
        "child-b",
        server,
        &binding,
        Some(&app_scope())
    ));
    assert!(
        sibling
            .discover()
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked")
    );
    assert!(host.clients.read().unwrap().is_empty());
    host.shutdown().await;
}

#[tokio::test]
async fn computer_actions_of_sibling_agents_are_serialized() {
    let fixture = Fixture::new();
    let marker = fixture.0.join("operations");
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "computer-use", "serialized", &marker);
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    for run in ["child-a", "child-b"] {
        ExtensionTools::new(host.clone(), run.into(), None, None).with_authorization_scope("root");
    }
    grant(&host, "root", server, app_scope());
    let first = select_task(host.clone(), "child-a", server);
    let second = select_task(host.clone(), "child-b", server);
    complete_control(first).await.unwrap();
    complete_control(second).await.unwrap();
    assert_eq!(
        fs::read_to_string(marker).unwrap(),
        "start\nend\nstart\nend\n"
    );
    host.shutdown().await;
}

#[tokio::test]
async fn failed_child_selection_never_leaks_a_family_session_grant() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "computer-use", "error", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    ExtensionTools::new(host.clone(), "child".into(), None, None).with_authorization_scope("root");
    let child = select_task(host.clone(), "child", server);
    let approval = pending_control(&host, "child").await;
    host.approvals
        .decide_scoped("child", &approval.id, true, true)
        .unwrap();
    let error = complete_control(child).await.unwrap_err();
    assert!(error.to_string().contains("fixture app selection refused"));
    let binding = serde_json::to_string(&host.server(server).unwrap()).unwrap();
    assert!(
        !host
            .controls
            .lock()
            .unwrap()
            .permits("root", server, &binding, Some(&app_scope()))
    );
    host.shutdown().await;
}
