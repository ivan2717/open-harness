use super::*;
use crate::extensions::{ApprovalExtent, PermissionTarget};

#[tokio::test]
async fn website_permission_covers_family_tabs_but_not_other_origins() {
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
        json!({"url":"https://example.com/start"}),
    );
    let approval = pending_control(&host, "root").await;
    assert_eq!(
        approval.permission_target,
        Some(PermissionTarget::Origin {
            origin: "https://example.com".into()
        })
    );
    assert!(approval.can_allow_capability);
    let child = control_task(
        host.clone(),
        "child",
        server,
        "chrome_open",
        json!({"url":"https://example.com/child"}),
    );
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(host.approvals.list("child").is_empty());
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Allow)
        .unwrap();
    complete_control(first).await.unwrap();
    complete_control(child).await.unwrap();
    let other = control_task(
        host.clone(),
        "child",
        server,
        "chrome_open",
        json!({"url":"https://sub.example.com"}),
    );
    let approval = pending_control(&host, "child").await;
    host.approvals
        .decide_permission("child", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(other).await.is_err());
    let permissions = host.permissions(Some("root"));
    assert_eq!(permissions.len(), 1);
    host.revoke_permission(Some("root"), &permissions[0])
        .unwrap();
    assert!(host.permissions(Some("root")).is_empty());
    let again = control_task(
        host.clone(),
        "root",
        server,
        "chrome_open",
        json!({"url":"https://example.com"}),
    );
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(again).await.is_err());
    host.shutdown().await;
}

#[tokio::test]
async fn application_permission_survives_pid_changes_but_not_switching_apps() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(&bundled, "computer-use", "ok", &fixture.0.join("called"));
    let host = ExtensionHost::open_with_bundled(&fixture.0.join("state"), Some(bundled)).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    let first = select_task(host.clone(), "root", server);
    let approval = pending_control(&host, "root").await;
    let bundle_id = match &approval.permission_target {
        Some(PermissionTarget::Application { bundle_id }) => bundle_id.clone(),
        _ => panic!("expected app target"),
    };
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Allow)
        .unwrap();
    complete_control(first).await.unwrap();
    complete_control(control_task(
        host.clone(),
        "root",
        server,
        "computer_select_app",
        json!({"bundle_id": bundle_id,"pid":12345}),
    ))
    .await
    .unwrap();
    let other = control_task(
        host.clone(),
        "root",
        server,
        "computer_select_app",
        json!({"bundle_id":"different.application","pid":12345}),
    );
    let approval = pending_control(&host, "root").await;
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(other).await.is_err());
    host.shutdown().await;
}

#[tokio::test]
async fn ordinary_mcp_tool_consent_is_reusable_but_never_the_entire_server() {
    let fixture = Fixture::new();
    let source = control_package(
        &fixture.0.join("packages"),
        "third-party",
        "ok",
        &fixture.0.join("called"),
    );
    let state = fixture.0.join("state");
    let host = ExtensionHost::open(&state).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = installed.plugins[0].mcp_servers[0].id.clone();
    let first = control_task(
        host.clone(),
        "root",
        &server,
        "lookup",
        json!({"query":"first"}),
    );
    let approval = pending_control(&host, "root").await;
    assert_eq!(
        approval.permission_target,
        Some(PermissionTarget::McpTool {
            name: "lookup".into()
        })
    );
    assert!(!approval.can_allow_capability);
    assert!(
        host.approvals
            .decide_permission_with_extent(
                "root",
                &approval.id,
                ApprovalDecision::Always,
                ApprovalExtent::Capability
            )
            .is_err()
    );
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Always)
        .unwrap();
    complete_control(first).await.unwrap();
    ExtensionTools::new(host.clone(), "child".into(), None, None).with_authorization_scope("root");
    complete_control(control_task(
        host.clone(),
        "child",
        &server,
        "lookup",
        json!({"query":"second"}),
    ))
    .await
    .unwrap();
    let write = control_task(host.clone(), "child", &server, "write", json!({}));
    let approval = pending_control(&host, "child").await;
    host.approvals
        .decide_permission("child", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(write).await.is_err());
    host.shutdown().await;
    drop(host);
    let host = ExtensionHost::open(&state).unwrap();
    complete_control(control_task(
        host.clone(),
        "restored",
        &server,
        "lookup",
        json!({}),
    ))
    .await
    .unwrap();
    let permissions = host.permissions(None);
    assert_eq!(permissions.len(), 1);
    host.revoke_permission(None, &permissions[0]).unwrap();
    assert!(host.permissions(None).is_empty());
    host.shutdown().await;
}

#[tokio::test]
async fn automatic_once_approval_does_not_create_session_or_persistent_grants() {
    let fixture = Fixture::new();
    let source = control_package(
        &fixture.0.join("packages"),
        "third-party",
        "ok",
        &fixture.0.join("called"),
    );
    let host = ExtensionHost::open(&fixture.0.join("state")).unwrap();
    let installed = host.install_plugin(&source, None).unwrap();
    let server = &installed.plugins[0].mcp_servers[0].id;
    for _ in 0..2 {
        let call = control_task(host.clone(), "root", server, "lookup", json!({}));
        let approval = pending_control(&host, "root").await;
        host.approvals.decide("root", &approval.id, true).unwrap();
        complete_control(call).await.unwrap();
        assert!(host.permissions(Some("root")).is_empty());
        assert!(host.permissions(None).is_empty());
    }
    host.shutdown().await;
}

#[tokio::test]
async fn explicit_whole_consent_satisfies_other_pending_sites_in_the_same_family() {
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
    let root = control_task(
        host.clone(),
        "root",
        server,
        "chrome_open",
        json!({"url":"https://first.example"}),
    );
    let approval = pending_control(&host, "root").await;
    let child = control_task(
        host.clone(),
        "child",
        server,
        "chrome_open",
        json!({"url":"https://second.example"}),
    );
    pending_control(&host, "child").await;
    host.approvals
        .decide_permission_with_extent(
            "root",
            &approval.id,
            ApprovalDecision::Allow,
            ApprovalExtent::Capability,
        )
        .unwrap();
    complete_control(root).await.unwrap();
    complete_control(child).await.unwrap();
    assert!(host.approvals.list("child").is_empty());
    assert!(host.permissions(Some("root"))[0].target.is_none());
    host.shutdown().await;
}

#[tokio::test]
async fn browser_host_probe_uses_actual_origin_and_model_cannot_forge_authorization() {
    let fixture = Fixture::new();
    let bundled = fixture.0.join("bundled");
    let source = control_package(
        &bundled,
        "browser-use",
        "browser_redirect",
        &fixture.0.join("called"),
    );
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
    let read = control_task(host.clone(), "root", server, "browser_snapshot", json!({}));
    let approval = pending_control(&host, "root").await;
    assert_eq!(
        approval.permission_target,
        Some(PermissionTarget::Origin {
            origin: "https://redirect.example".into()
        })
    );
    host.approvals
        .decide_permission("root", &approval.id, ApprovalDecision::Cancel)
        .unwrap();
    assert!(complete_control(read).await.is_err());
    for (name, arguments) in [
        (
            "browser_open",
            json!({"url":"https://example.com", "_host_permission":{"origins":["*"]}}),
        ),
        ("__openharness_browser_target", json!({})),
    ] {
        assert!(
            complete_control(control_task(host.clone(), "root", server, name, arguments))
                .await
                .unwrap_err()
                .to_string()
                .contains("not a model tool")
        );
        assert!(host.approvals.list("root").is_empty());
    }
    host.shutdown().await;
}
