use agent_local::{CommandRecord, NetworkAccess, PendingApproval};
use serde_json::json;

#[test]
fn old_command_records_do_not_gain_network_permission() {
    let record: CommandRecord = serde_json::from_value(json!({
        "id":"old", "command":"echo hello", "status":"completed", "output":"hello", "exit_code":0
    }))
    .unwrap();
    assert_eq!(record.network_access, NetworkAccess::Denied);
    let approval: PendingApproval = serde_json::from_value(json!({
        "id":"old", "command":"echo hello", "cwd":"/project"
    }))
    .unwrap();
    assert_eq!(approval.network_access, NetworkAccess::Denied);
    assert!(serde_json::from_str::<NetworkAccess>("\"full_access\"").is_err());
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use agent_local::{LocalTools, PermissionMode};
    use agent_runtime::{ToolCall, ToolExecutor};
    use serde_json::Value;
    use std::{
        fs,
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    async fn run(
        tools: &LocalTools,
        command: String,
        network: Option<&str>,
        approve: bool,
    ) -> String {
        let mut input = json!({"command":command, "timeout_seconds":5});
        if let Some(network) = network {
            input["network_access"] = network.into();
        }
        let worker = tools.clone();
        let task = tokio::spawn(async move {
            worker
                .execute(ToolCall {
                    id: "network-test".into(),
                    name: "run_command".into(),
                    arguments: input.to_string(),
                })
                .await
        });
        for _ in 0..200 {
            let review = tools.review().unwrap();
            if let Some(approval) = review.approvals.first() {
                assert_eq!(
                    approval.network_access,
                    if network == Some("allowed") {
                        NetworkAccess::Allowed
                    } else {
                        NetworkAccess::Denied
                    }
                );
                tools.approve(&approval.id, approve).unwrap();
                return task
                    .await
                    .unwrap()
                    .map_err(|e| e.to_string())
                    .unwrap_or_else(|e| e);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("command did not ask for approval");
    }

    #[tokio::test]
    async fn network_grants_are_approved_scoped_and_keep_file_restrictions() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "openharness-network-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("workspace");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".env"), "secret").unwrap();
        let tools = LocalTools::open(
            root.clone(),
            base.join("state"),
            PermissionMode::WorkspaceWrite,
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let curl = format!(
            "/usr/bin/curl --silent --show-error --max-time 2 http://{}/",
            listener.local_addr().unwrap()
        );

        // A request for network access is not itself a grant.
        let denied = run(&tools, curl.clone(), Some("allowed"), false).await;
        assert!(denied.contains("denied"));
        assert!(listener.accept().is_err());

        let response =
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nNETWORK_OK";
        let server = listener.try_clone().unwrap();
        let serve = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            loop {
                if let Ok((mut stream, _)) = server.accept() {
                    stream.write_all(response.as_bytes()).unwrap();
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "sandbox could not connect after approval"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let command = format!(
            "if cat .env >/dev/null 2>&1; then exit 81; fi; if echo escape > ../escaped 2>/dev/null; then exit 82; fi; /bin/sh -c '{}'",
            curl
        );
        let result: Value =
            serde_json::from_str(&run(&tools, command, Some("allowed"), true).await).unwrap();
        serve.join().unwrap();
        assert_eq!(result["status"], "completed", "{result}");
        assert!(result["output"].as_str().unwrap().contains("NETWORK_OK"));
        assert!(!base.join("escaped").exists());

        // A later command with no field must not inherit the previous grant.
        let result: Value = serde_json::from_str(&run(&tools, curl, None, true).await).unwrap();
        assert_eq!(result["status"], "failed", "{result}");
        assert!(listener.accept().is_err());
        let review = tools.review().unwrap();
        assert_eq!(review.commands[1].network_access, NetworkAccess::Allowed);
        assert_eq!(review.commands[2].network_access, NetworkAccess::Denied);
        drop(tools);
        let restored =
            LocalTools::open(root, base.join("state"), PermissionMode::WorkspaceWrite).unwrap();
        assert_eq!(
            restored.review().unwrap().commands[1].network_access,
            NetworkAccess::Allowed
        );
        drop(restored);
        fs::remove_dir_all(base).unwrap();
    }
}
