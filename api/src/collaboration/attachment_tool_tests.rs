use super::*;
use agent_runtime::ToolExecutor;

struct Fixture {
    state: AppState,
    directory: PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "attachment-tools-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        Self { state, directory }
    }
    async fn run(
        &self,
        id: &str,
        files: Vec<agent_core::FileReference>,
        parent: Option<&str>,
    ) -> super::super::tools::RunTools {
        let mut detail: RunDetail = serde_json::from_value(json!({
            "id":id,"revision":0,"title":"Attachment tool fixture","prompt":"Review supplied data",
            "status":"running","model_id":MODEL_ID,"created_at":"","updated_at":"",
            "messages":[],"events":[],"context":null,"error":null,
            "delegation":parent.map(|parent|json!({"parent_run_id":parent,"root_run_id":parent,"task_name":id,"spawn_call_id":format!("spawn-{id}")}))
        })).unwrap();
        let project = self.directory.join("project");
        fs::create_dir_all(&project).unwrap();
        detail.summary.workspace_path = Some(project.to_string_lossy().into_owned());
        let entry = self.state.make_entry(detail, 0).unwrap();
        assert!(
            entry.workspace_error.is_none(),
            "{:?}",
            entry.workspace_error
        );
        let run = entry
            .runtime
            .start_run_with_files(&entry.agent, id, "Review supplied data", files)
            .await
            .unwrap();
        for event in run.events() {
            entry.snapshot.event(event).unwrap();
        }
        let base = crate::extensions::ExtensionTools::new(
            self.state.inner.extensions.clone(),
            id.into(),
            None,
            entry.workspace.clone(),
        );
        self.state
            .inner
            .runs
            .write()
            .unwrap()
            .insert(id.into(), entry);
        super::super::tools::RunTools::new(&self.state, id.into(), base, parent.is_some())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "attachment-tool-call".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

#[tokio::test]
async fn original_is_read_and_searched_on_demand_without_inlining_body() {
    let fixture = Fixture::new().await;
    let content = format!(
        "{}\nUNIQUE_LATE_EVIDENCE\n",
        "A large document line.\n".repeat(40_000)
    );
    let file = fixture
        .state
        .store_attachment("large.txt".into(), content.as_bytes().to_vec())
        .await
        .unwrap();
    let path = format!("attachment:{}", file.attachment.as_ref().unwrap().id);
    let prompt = agent_core::format_user_input("Review", std::slice::from_ref(&file));
    assert!(prompt.len() < 2500);
    assert!(!prompt.contains("UNIQUE_LATE_EVIDENCE"));
    let tools = fixture.run("owner", vec![file], None).await;
    let page: Value = serde_json::from_str(
        &tools
            .execute(call("read_file", json!({"path":path})))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page["content"].as_str().unwrap().chars().count(), 8192);
    assert!(page["next_cursor"].is_object());
    let results: Value = serde_json::from_str(
        &tools
            .execute(call(
                "search_files",
                json!({"path":path,"query":"UNIQUE_LATE_EVIDENCE"}),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(results["matches"][0]["line"], 40002);
    assert!(results.to_string().len() < 2500);
    let source: Value = serde_json::from_str(
        &tools
            .execute(call("read_file", json!({"path":path,"mode":"original"})))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        fs::read(source["local_path"].as_str().unwrap()).unwrap(),
        content.as_bytes()
    );
    assert_eq!(source["read_only"], true);
    assert_eq!(
        tools
            .definitions()
            .iter()
            .filter(|d| d.name == "read_file")
            .count(),
        1
    );
    fixture.state.shutdown().await;
}

#[tokio::test]
async fn child_can_resolve_parent_original_but_unrelated_task_cannot() {
    let fixture = Fixture::new().await;
    let file = fixture
        .state
        .store_attachment("source.md".into(), b"Parent supplied evidence".to_vec())
        .await
        .unwrap();
    let path = format!("attachment:{}", file.attachment.as_ref().unwrap().id);
    let _owner = fixture.run("owner", vec![file], None).await;
    let child = fixture.run("child", vec![], Some("owner")).await;
    let stranger = fixture.run("stranger", vec![], None).await;
    let history: Value = serde_json::from_str(
        &child
            .execute(call(
                "agent_control",
                json!({"action":"read","target":"parent","message_id":"event-0","file_index":0}),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(history["file"]["path"], path);
    let page = child
        .execute(call("read_file", json!({"path":path})))
        .await
        .unwrap();
    assert!(page.contains("Parent supplied evidence"));
    for mode in ["text", "original"] {
        assert!(
            stranger
                .execute(call("read_file", json!({"path":path,"mode":mode})))
                .await
                .is_err()
        );
    }
    assert!(
        child
            .execute(call("read_file", json!({"path":"attachment:../../secret"})))
            .await
            .is_err()
    );
    assert!(
        child
            .execute(call(
                "read_file",
                json!({"path":path,"mode":"original","cursor":{"unit":1,"offset":0}})
            ))
            .await
            .is_err()
    );
    fixture.state.shutdown().await;
}
