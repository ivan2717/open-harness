#![cfg(unix)]

use std::{path::PathBuf, time::Duration};

use agent_mcp::{McpClient, McpError, McpServerConfig, McpTransport};
use serde_json::{Map, json};

fn client(mode: &str, pid_file: Option<&PathBuf>) -> McpClient {
    let mut args = vec![
        "-u".into(),
        format!("{}/tests/fixtures/server.py", env!("CARGO_MANIFEST_DIR")),
        mode.into(),
    ];
    if let Some(pid_file) = pid_file {
        args.push(pid_file.to_string_lossy().into());
    }
    McpClient::new(McpServerConfig {
        id: "fixture".into(),
        name: None,
        enabled: true,
        transport: McpTransport::Stdio {
            command: "python3".into(),
            args,
            env_vars: vec![],
        },
    })
    .unwrap()
    .with_timeout(Duration::from_secs(5))
}

#[tokio::test]
async fn negotiates_pages_and_preserves_protocol_results() {
    let client = client("normal", None);
    let catalog = client.discover().await.unwrap();
    assert_eq!(catalog.protocol_version, "2025-11-25");
    assert_eq!(
        catalog
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(catalog.resources.len(), 1);
    assert_eq!(catalog.prompts.len(), 1);
    assert!(client.list_resource_templates().await.unwrap().is_empty());
    let result = client
        .call_tool(
            "tool_error",
            json!({"input": 7}).as_object().unwrap().clone(),
        )
        .await
        .unwrap();
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["arguments"]["input"], 7);
    assert_eq!(result["content"][0]["text"], "tool result");
    assert_eq!(result["structuredContent"]["calls"], 1);
    let second = client
        .clone()
        .call_tool("second", Map::new())
        .await
        .unwrap();
    assert_eq!(second["structuredContent"]["calls"], 2);
    let environment = result["structuredContent"]["environment"]
        .as_array()
        .unwrap();
    assert!(environment.iter().all(|name| {
        [
            "PATH",
            "HOME",
            "USERPROFILE",
            "SYSTEMROOT",
            "WINDIR",
            "TMPDIR",
            "TEMP",
            "TMP",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            // Apple's /usr/bin/python3 launcher adds SDK discovery variables.
            "__CF_USER_TEXT_ENCODING",
            "SDKROOT",
            "CPATH",
            "LIBRARY_PATH",
            "MANPATH",
        ]
        .contains(&name.as_str().unwrap())
    }));
    assert_eq!(
        client.read_resource("fixture:///document").await.unwrap()["contents"][0]["text"],
        "resource body"
    );
    assert_eq!(
        client.get_prompt("review", Map::new()).await.unwrap()["messages"][0]["role"],
        "user"
    );
    let error = client
        .call_tool("rpc_error", Map::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("secret-token"));
    let after_error = client.call_tool("echo", Map::new()).await.unwrap();
    assert_eq!(after_error["structuredContent"]["calls"], 4);
}

#[tokio::test]
async fn rejects_cyclic_pagination() {
    assert!(matches!(
        client("repeat_cursor", None).discover().await,
        Err(McpError::Pagination)
    ));
}

fn pid_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "open-harness-mcp-{name}-{}.pid",
        std::process::id()
    ))
}

async fn read_pid(path: &PathBuf) -> i32 {
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.parse()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("fixture did not start")
}

async fn assert_stopped(pid: i32) {
    for _ in 0..100 {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("MCP child survived session cancellation")
}

#[tokio::test]
async fn deadline_and_task_cancellation_reap_children() {
    let path = pid_file("deadline");
    let result = client("normal", Some(&path))
        .with_timeout(Duration::from_millis(400))
        .call_tool("hang", Map::new())
        .await;
    assert!(matches!(result, Err(McpError::Timeout)));
    assert_stopped(read_pid(&path).await).await;
    std::fs::remove_file(path).unwrap();

    let path = pid_file("cancel");
    let client = client("normal", Some(&path));
    client.discover().await.unwrap();
    let active = client.clone();
    let task = tokio::spawn(async move { active.call_tool("hang", Map::new()).await });
    let pid = read_pid(&path).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    task.abort();
    let _ = task.await;
    assert_stopped(pid).await;
    // An explicitly submitted subsequent call creates a new session; the
    // cancelled call itself was never replayed and no uncertain session survives.
    let result = client.call_tool("echo", Map::new()).await.unwrap();
    assert_eq!(result["structuredContent"]["calls"], 1);
    let restarted = read_pid(&path).await;
    client.shutdown().await;
    assert_stopped(restarted).await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn dropping_last_client_stops_idle_session() {
    let path = pid_file("drop");
    let client = client("normal", Some(&path));
    client.test_connection().await.unwrap();
    let pid = read_pid(&path).await;
    let other = client.clone();
    drop(client);
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    drop(other);
    assert_stopped(pid).await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn shutdown_drop_and_cancel_allow_server_to_reap_detached_resources() {
    struct DetachedChild(Option<i32>);
    impl Drop for DetachedChild {
        fn drop(&mut self) {
            // A failing regression must not itself leak the detached fixture.
            if let Some(pid) = self.0 {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    for mode in ["cleanup-close", "cleanup-drop", "cleanup-cancel"] {
        let path = pid_file(mode);
        let child_path = PathBuf::from(format!("{}.child", path.display()));
        let marker = PathBuf::from(format!("{}.cleaned", path.display()));
        let client = client("detached_cleanup", Some(&path));
        client.test_connection().await.unwrap();
        let server_pid = read_pid(&path).await;
        let detached_pid = read_pid(&child_path).await;
        let mut detached = DetachedChild(Some(detached_pid));
        if mode == "cleanup-close" {
            client.shutdown().await;
        } else if mode == "cleanup-cancel" {
            let active = client.clone();
            let task = tokio::spawn(async move { active.call_tool("hang", Map::new()).await });
            tokio::time::sleep(Duration::from_millis(50)).await;
            task.abort();
            let _ = task.await;
        }
        drop(client);
        assert_stopped(server_pid).await;
        assert_stopped(detached_pid).await;
        detached.0 = None;
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "closed");
        for file in [path, child_path, marker] {
            std::fs::remove_file(file).unwrap();
        }
    }
}

#[tokio::test]
async fn revocation_blocks_old_handles_and_cancels_running_calls() {
    let path = pid_file("revoked-before-launch");
    let client = client("normal", Some(&path));
    let old_handle = client.clone();
    client.revoke();
    assert!(matches!(
        old_handle.call_tool("echo", Map::new()).await,
        Err(McpError::Revoked)
    ));
    assert!(!path.exists());

    let path = pid_file("revoked-running");
    let client = self::client("normal", Some(&path));
    client.discover().await.unwrap();
    let pid = read_pid(&path).await;
    let old_handle = client.clone();
    let task = tokio::spawn(async move { old_handle.call_tool("hang", Map::new()).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    client.revoke();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap(),
        Err(McpError::Revoked)
    ));
    assert_stopped(pid).await;
    assert!(matches!(
        client.test_connection().await,
        Err(McpError::Revoked)
    ));
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn initialization_timeout_and_oversize_are_bounded() {
    let path = pid_file("initialize");
    assert!(matches!(
        client("hang_initialize", Some(&path))
            .with_timeout(Duration::from_millis(400))
            .discover()
            .await,
        Err(McpError::Timeout)
    ));
    assert_stopped(read_pid(&path).await).await;
    std::fs::remove_file(path).unwrap();
    assert!(
        client("normal", None)
            .with_timeout(Duration::from_millis(600))
            .call_tool("oversize", Map::new())
            .await
            .is_err()
    );
}
