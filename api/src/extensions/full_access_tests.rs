use super::*;
use agent_local::ApprovalMode;
use agent_runtime::ToolExecutor;

#[tokio::test]
async fn restoring_automatic_review_does_not_revoke_a_saved_human_permission() {
    let fixture = Fixture::new();
    let source = control_package(
        &fixture.0.join("packages"),
        "ordinary-mcp",
        "ok",
        &fixture.0.join("called"),
    );
    let state = fixture.0.join("state");
    let host = ExtensionHost::open(&state).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    host.set_approval_mode("root", ApprovalMode::AutoReview);
    let operation = control_task(host.clone(), "root", server, "lookup", json!({}));
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Always)
        .unwrap();
    complete_control(operation).await.unwrap();
    host.shutdown().await;
    drop(host);

    let restored = ExtensionHost::open(&state).unwrap();
    ExtensionTools::new(restored.clone(), "root".into(), None, None)
        .with_authorization_scope("root")
        .with_approval_mode(ApprovalMode::AutoReview);
    complete_control(control_task(
        restored.clone(),
        "root",
        server,
        "lookup",
        json!({}),
    ))
    .await
    .unwrap();
    assert!(restored.approvals.list("root").is_empty());
    assert_eq!(restored.permissions(None).len(), 1);
    restored.shutdown().await;
}

#[tokio::test]
async fn full_access_is_live_root_policy_not_a_saved_or_cross_conversation_grant() {
    let fixture = Fixture::new();
    let source = control_package(
        &fixture.0.join("packages"),
        "ordinary-mcp",
        "ok",
        &fixture.0.join("called"),
    );
    let state = fixture.0.join("state");
    let host = ExtensionHost::open(&state).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let tools = ExtensionTools::new(host.clone(), "root".into(), None, None)
        .with_authorization_scope("root")
        .with_approval_mode(ApprovalMode::FullAccess);
    // A child restored from an old snapshot cannot reset the live root mode.
    ExtensionTools::new(host.clone(), "child".into(), None, None)
        .with_authorization_scope("root")
        .with_approval_mode(ApprovalMode::Ask);
    for run in ["root", "child"] {
        complete_control(control_task(host.clone(), run, server, "write", json!({})))
            .await
            .unwrap();
        assert!(host.approvals.list(run).is_empty());
        assert!(host.permissions(Some(run)).is_empty());
    }
    for (name, arguments) in [
        (
            "read_mcp_resource",
            json!({"server_id":server, "uri":"records://one"}),
        ),
        (
            "get_mcp_prompt",
            json!({"server_id":server, "name":"draft", "arguments":{}}),
        ),
    ] {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tools.execute(agent_runtime::ToolCall {
                id: "full".into(),
                name: name.into(),
                arguments: arguments.to_string(),
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }
    let other = control_task(host.clone(), "different-root", server, "write", json!({}));
    let pending = pending_control(&host, "different-root").await;
    host.approvals
        .decide("different-root", &pending.id, false)
        .unwrap();
    assert!(complete_control(other).await.is_err());
    assert!(host.permissions(None).is_empty());
    host.shutdown().await;
    drop(tools);
    drop(host);

    // Only the root's saved execution policy may restore full access. Reopening
    // the extension host by itself never restores MCP access from its store.
    let host = ExtensionHost::open(&state).unwrap();
    let pending_call = control_task(host.clone(), "root", server, "write", json!({}));
    let pending = pending_control(&host, "root").await;
    host.approvals.decide("root", &pending.id, false).unwrap();
    assert!(complete_control(pending_call).await.is_err());
    host.shutdown().await;
}

#[tokio::test]
async fn full_access_controls_select_targets_without_remembering_them() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let browser_marker = fixture.0.join("browser-arguments");
    let browser = control_package(&bundled, "browser-use", "record_args", &browser_marker);
    let chrome = control_package(
        &bundled,
        "chrome-use",
        "open_dynamic",
        &fixture.0.join("chrome"),
    );
    let computer = control_package(&bundled, "computer-use", "ok", &fixture.0.join("computer"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    host.install_plugin(&browser, None).unwrap();
    host.install_plugin(&chrome, None).unwrap();
    host.install_plugin(&computer, None).unwrap();
    let server = |plugin: &str| {
        host.settings()
            .plugins
            .into_iter()
            .find(|p| p.id == plugin)
            .unwrap()
            .mcp_servers[0]
            .id
            .clone()
    };
    let browser = server("browser-use");
    let chrome = server("chrome-use");
    let computer = server("computer-use");
    host.set_approval_mode("root", ApprovalMode::FullAccess);
    for url in ["https://example.com", "https://another.example"] {
        complete_control(control_task(
            host.clone(),
            "root",
            &chrome,
            "chrome_open",
            json!({"url":url}),
        ))
        .await
        .unwrap();
    }
    complete_control(control_task(
        host.clone(),
        "root",
        &browser,
        "browser_open",
        json!({"url":"https://example.com"}),
    ))
    .await
    .unwrap();
    let arguments: serde_json::Value =
        serde_json::from_slice(&fs::read(browser_marker).unwrap()).unwrap();
    assert_eq!(arguments["_host_permission"]["origins"], json!(["*"]));
    for app in ["first.application", "another.application"] {
        complete_control(control_task(
            host.clone(),
            "root",
            &computer,
            "computer_select_app",
            json!({"bundle_id":app,"pid":1234}),
        ))
        .await
        .unwrap();
    }
    assert!(host.approvals.list("root").is_empty());
    assert!(host.permissions(Some("root")).is_empty());
    assert!(host.permissions(None).is_empty());
    assert_eq!(host.controls.lock().unwrap().list("root").len(), 3);

    // Full access still cannot forge bridge authorization or dispatch to an
    // arbitrary Chrome tab instead of the explicitly selected target.
    for (name, arguments) in [
        ("chrome_snapshot", json!({"tab_id":9999})),
        ("chrome_open", json!({"url":"chrome://settings"})),
        (
            "chrome_open",
            json!({"url":"https://example.com", "_host_permission":{"origins":["*"]}}),
        ),
    ] {
        assert!(
            complete_control(control_task(host.clone(), "root", &chrome, name, arguments))
                .await
                .is_err()
        );
    }
    host.set_approval_mode("root", ApprovalMode::Ask);
    assert!(host.controls.lock().unwrap().list("root").is_empty());
    let call = control_task(
        host.clone(),
        "root",
        &chrome,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let pending = pending_control(&host, "root").await;
    host.approvals.decide("root", &pending.id, false).unwrap();
    assert!(complete_control(call).await.is_err());
    host.shutdown().await;
}

#[tokio::test]
async fn downgrade_revokes_inflight_full_access_and_stale_child_cannot_restore_it() {
    let fixture = Fixture::new();
    let marker = fixture.0.join("called");
    let source = control_package(&fixture.0.join("packages"), "ordinary-mcp", "hang", &marker);
    let host = ExtensionHost::open(&fixture.0.join("state")).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let child = ExtensionTools::new(host.clone(), "child".into(), None, None)
        .with_authorization_scope("root");
    host.set_approval_mode("root", ApprovalMode::FullAccess);
    let operation = control_task(host.clone(), "child", server, "write", json!({}));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    host.set_approval_mode("root", ApprovalMode::AutoReview);
    assert!(complete_control(operation).await.is_err());
    // The already-created child's executor and a restored FullAccess snapshot
    // must both observe the new root mode rather than revive full access.
    let child = child.with_approval_mode(ApprovalMode::FullAccess);
    assert_eq!(
        host.controls.lock().unwrap().approval_mode("child"),
        ApprovalMode::AutoReview
    );
    let server = server.clone();
    let operation = tokio::spawn(async move {
        child
            .execute(agent_runtime::ToolCall {
                id: "after-downgrade".into(),
                name: "call_mcp_tool".into(),
                arguments: json!({"server_id":server,"name":"write","arguments":{}}).to_string(),
            })
            .await
    });
    let pending = pending_control(&host, "child").await;
    host.approvals.decide("child", &pending.id, false).unwrap();
    assert!(complete_control(operation).await.is_err());
    assert!(host.permissions(None).is_empty());
    host.shutdown().await;
}
