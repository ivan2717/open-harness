use super::*;

#[tokio::test]
async fn idle_check_rejects_active_sessions_and_outstanding_handles() {
    let client = McpClient::new(McpServerConfig {
        id: "fixture".into(),
        name: None,
        enabled: true,
        transport: McpTransport::Stdio {
            command: "not-launched".into(),
            args: vec![],
            env_vars: vec![],
        },
    })
    .unwrap();
    assert!(client.is_idle());
    let pending = client.clone();
    assert!(!client.is_idle());
    drop(pending);
    let guard = client.session.lock().await;
    assert!(!client.is_idle());
    drop(guard);
    assert!(client.is_idle());
}
