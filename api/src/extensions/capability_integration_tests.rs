//! Capability consent, persistence and revocation through the real MCP boundary.
use super::*;

#[tokio::test]
async fn capability_allow_reuses_new_tabs_and_origins_only_inside_conversation_family() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(
        &bundled,
        "chrome-use",
        "open_dynamic",
        &fixture.0.join("called"),
    );
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    ExtensionTools::new(host.clone(), "child".into(), None, None).with_authorization_scope("root");
    let first = control_task(
        host.clone(),
        "root",
        server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    // A concurrent child request waits for the same capability consent instead
    // of displaying a second approval card.
    let child = control_task(
        host.clone(),
        "child",
        server,
        "chrome_open",
        json!({"url":"https://other.example"}),
    );
    tokio::task::yield_now().await;
    assert!(host.approvals.list("child").is_empty());
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Allow)
        .unwrap();
    complete_control(first).await.unwrap();
    complete_control(child).await.unwrap();
    for url in [
        "https://example.com/history",
        "https://space.example.com/favorites",
    ] {
        complete_control(control_task(
            host.clone(),
            "root",
            server,
            "chrome_open",
            json!({"url":url}),
        ))
        .await
        .unwrap();
        assert!(host.approvals.list("root").is_empty());
    }
    assert!(host.settings().control_permissions.is_empty());
    let outside = control_task(
        host.clone(),
        "outside",
        server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "outside").await;
    host.approvals
        .decide_permission("outside", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(outside).await.is_err());
    host.revoke_run("root");
    let child = control_task(
        host.clone(),
        "child",
        server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "child").await;
    host.approvals
        .decide_permission("child", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(child).await.is_err());
    host.shutdown().await;
}

#[tokio::test]
async fn always_permission_survives_restart_but_stop_and_settings_revoke_take_effect() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(
        &bundled,
        "chrome-use",
        "open_dynamic",
        &fixture.0.join("called"),
    );
    let state = fixture.0.join("state");
    let host = ExtensionHost::open_with_bundled(&state, Some(bundled.clone())).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = installed.plugins[0].mcp_servers[0].id.clone();
    let first = control_task(
        host.clone(),
        "root",
        &server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Always)
        .unwrap();
    complete_control(first).await.unwrap();
    assert_eq!(host.settings().control_permissions.len(), 1);
    host.revoke_run("root");
    let stopped = control_task(
        host.clone(),
        "root",
        &server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(stopped).await.is_err());
    // Stopping one family does not silently erase the user's global setting.
    complete_control(control_task(
        host.clone(),
        "other",
        &server,
        "chrome_open",
        json!({"url":"https://other.example"}),
    ))
    .await
    .unwrap();
    host.shutdown().await;
    drop(host);
    let host = ExtensionHost::open_with_bundled(&state, Some(bundled)).unwrap();
    complete_control(control_task(
        host.clone(),
        "restored",
        &server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    ))
    .await
    .unwrap();
    let client = host
        .client("restored", &host.server(&server).unwrap())
        .unwrap();
    host.revoke_control_permission(&server).unwrap();
    assert!(host.settings().control_permissions.is_empty());
    assert!(host.controls.lock().unwrap().list("restored").is_empty());
    assert!(
        client
            .discover()
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked")
    );
    let next = control_task(
        host.clone(),
        "restored",
        &server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "restored").await;
    host.approvals
        .decide_permission("restored", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(next).await.is_err());
    host.shutdown().await;
    let restored = ExtensionHost::open(&state).unwrap();
    assert!(restored.settings().control_permissions.is_empty());
}

#[tokio::test]
async fn computer_capability_allows_other_apps_and_cancel_does_not_save_a_denial() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let marker = fixture.0.join("called");
    let source = control_package(&bundled, "computer-use", "ok", &marker);
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let cancelled = select_task(host.clone(), "root", server);
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(
        complete_control(cancelled)
            .await
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert!(!marker.exists());
    assert!(host.settings().control_permissions.is_empty());
    let first = select_task(host.clone(), "root", server);
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Allow)
        .unwrap();
    complete_control(first).await.unwrap();
    complete_control(control_task(
        host.clone(),
        "root",
        server,
        "computer_select_app",
        json!({"bundle_id":"another.application","pid":9876}),
    ))
    .await
    .unwrap();
    assert!(host.approvals.list("root").is_empty());
    host.shutdown().await;
}

#[tokio::test]
async fn persistent_permission_is_bound_to_plugin_content_and_third_party_cannot_opt_in() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "browser-use", "ok", &fixture.0.join("called"));
    let host =
        ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled.clone())).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = installed.plugins[0].mcp_servers[0].id.clone();
    let first = control_task(
        host.clone(),
        "root",
        &server,
        "browser_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Always)
        .unwrap();
    complete_control(first).await.unwrap();
    fs::write(
        source.join("fixture.py"),
        fs::read_to_string(source.join("fixture.py")).unwrap() + "\n# updated package\n",
    )
    .unwrap();
    host.install_plugin(&source, Some("browser-use")).unwrap();
    let changed = control_task(
        host.clone(),
        "other",
        &server,
        "browser_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "other").await;
    host.approvals
        .decide_permission("other", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(changed).await.is_err());
    host.shutdown().await;
    let external = ExtensionHost::open_with_bundled(&fixture.0.join("external"), None).unwrap();
    let installed = external.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let task = control_task(
        external.clone(),
        "r",
        server,
        "browser_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&external, "r").await;
    assert!(approval.session_scope.is_none());
    assert!(
        external
            .approvals
            .decide_permission("r", &approval.id, ApprovalDecision::Always)
            .is_err()
    );
    external
        .approvals
        .decide_permission("r", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(task).await.is_err());
    external.shutdown().await;
}

#[tokio::test]
async fn always_capability_is_saved_even_if_the_first_action_fails() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "computer-use", "error", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let first = select_task(host.clone(), "root", server);
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Always)
        .unwrap();
    assert!(complete_control(first).await.is_err());
    assert_eq!(host.settings().control_permissions.len(), 1);
    assert_eq!(host.run_state("root", None).control_permissions.len(), 1);
    assert!(host.controls.lock().unwrap().list("root").is_empty());
    assert!(
        complete_control(select_task(host.clone(), "root", server))
            .await
            .is_err()
    );
    assert!(host.approvals.list("root").is_empty());
    host.shutdown().await;
}

#[tokio::test]
async fn releasing_a_browser_keeps_capability_visible_until_the_user_stops_control() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "browser-use", "ok", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let first = control_task(
        host.clone(),
        "root",
        server,
        "browser_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Allow)
        .unwrap();
    complete_control(first).await.unwrap();
    complete_control(control_task(
        host.clone(),
        "root",
        server,
        "browser_close",
        json!({}),
    ))
    .await
    .unwrap();
    let state = host.run_state("root", None);
    assert!(state.controls.is_empty());
    assert_eq!(state.control_permissions.len(), 1);
    host.revoke_run("root");
    assert!(host.run_state("root", None).control_permissions.is_empty());
    host.shutdown().await;
}

#[tokio::test]
async fn settings_revocation_invalidates_new_tab_inflight_before_any_active_selection_exists() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let marker = fixture.0.join("called");
    let source = control_package(&bundled, "chrome-use", "open_pause", &marker);
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let first = control_task(
        host.clone(),
        "root",
        server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Always)
        .unwrap();
    complete_control(first).await.unwrap();
    fs::remove_file(&marker).unwrap();
    let pending = control_task(
        host.clone(),
        "new-root",
        server,
        "chrome_open",
        json!({"url":"https://example.com/pending"}),
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let epoch = host.controls.lock().unwrap().epoch("new-root");
    assert!(host.controls.lock().unwrap().list("new-root").is_empty());
    host.revoke_control_permission(server).unwrap();
    assert!(
        host.controls
            .lock()
            .unwrap()
            .check_epoch("new-root", epoch)
            .is_err()
    );
    assert!(complete_control(pending).await.is_err());
    assert!(
        host.run_state("new-root", None)
            .control_permissions
            .is_empty()
    );
    assert!(host.run_state("new-root", None).controls.is_empty());
    host.shutdown().await;
}

#[tokio::test]
async fn cancelling_capability_consent_dismisses_already_queued_sibling_requests() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "browser-use", "ok", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    ExtensionTools::new(host.clone(), "child".into(), None, None).with_authorization_scope("root");
    let first = control_task(
        host.clone(),
        "root",
        server,
        "browser_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    let child = control_task(
        host.clone(),
        "child",
        server,
        "browser_open",
        json!({"url":"https://other.example"}),
    );
    // Ensure the sibling has entered the consent queue before cancellation.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(first).await.is_err());
    assert!(complete_control(child).await.is_err());
    assert!(host.approvals.list("child").is_empty());
    assert!(host.run_state("root", None).control_permissions.is_empty());
    let retry = control_task(
        host.clone(),
        "root",
        server,
        "browser_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Allow)
        .unwrap();
    complete_control(retry).await.unwrap();
    host.shutdown().await;
}
