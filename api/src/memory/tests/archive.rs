use super::*;
use agent_memory::{WikiState, WikiTaskStatus};
use std::collections::VecDeque;

const CHOICE: &str = "选择 SQLite，因为本地部署不需要额外服务；暂不使用 PostgreSQL，以免增加安装和维护成本。保留原始事件日志。";
const FAILURE: &str = "并发写入 4 个连接时 database is locked；导出未完成。";
const CORRECTION: &str =
    "继续使用 SQLite，启用 WAL 并串行写入以处理锁冲突；PostgreSQL 仍是多人云端部署的备选。";
const VERIFIED: &str =
    "4 个连接的恢复测试通过，50 行；报告 artifacts/recovery.json；未测试多设备写入。";

// Scripted responses test preservation, provenance and retrieval plumbing.
// They do not measure whether a real model extracts every important fact.
#[derive(Clone)]
struct ArchiveFixture {
    requests: Arc<StdMutex<Vec<ModelRequest>>>,
    responses: Arc<StdMutex<VecDeque<Value>>>,
}

impl ArchiveFixture {
    fn new(responses: Vec<Value>) -> Self {
        Self {
            requests: Arc::default(),
            responses: Arc::new(StdMutex::new(responses.into())),
        }
    }
}

impl Model for ArchiveFixture {
    type Error = io::Error;

    async fn respond(&self, request: ModelRequest) -> io::Result<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected archive compilation");
        Ok(ModelResponse::final_answer(response.to_string()))
    }
}

fn command(events: &mut Vec<RunEventEnvelope>, call: &str, exit_code: i32, output: &str) {
    append(
        events,
        RunEvent::ToolCallRequested {
            call_id: call.into(),
            tool_name: "run_command".into(),
            arguments: json!({"command":"run-storage-recovery-test"}).to_string(),
        },
    );
    append(
        events,
        RunEvent::ToolCallSucceeded {
            call_id: call.into(),
            output: json!({"exit_code":exit_code,"output":output}).to_string(),
        },
    );
}

fn first_update() -> Value {
    json!({
        "overview":"存储选型及取舍见 storage-decision；锁冲突条件与验证见 storage-recovery。",
        "pages":[
            {"id":"storage-decision","title":"存储选型与取舍",
             "content":format!("初次决定：{CHOICE} [event #0]"),"source_sequences":[0]},
            {"id":"storage-recovery","title":"锁冲突的失败条件与恢复",
             "content":format!("失败记录：{FAILURE} 尚未验证修复。 [event #2]"),"source_sequences":[2]}
        ],
        "tasks":[
            {"id":"retain-journal","kind":"constraint","status":"active","text":"保留原始事件日志。",
             "evidence":[{"sequence":0,"quote":"保留原始事件日志。"}]},
            {"id":"storage-recovery","kind":"work","status":"failed","text":FAILURE,
             "evidence":[{"sequence":2,"quote":FAILURE}]}
        ]
    })
}

fn corrected_update() -> Value {
    json!({
        "overview":"存储选型及取舍见 storage-decision；锁冲突与恢复证据见 storage-recovery；验证产物见 recovery-artifact。",
        "pages":[
            {"id":"storage-decision","title":"存储选型与取舍",
             "content":format!("初次决定：{CHOICE} [event #0]\n后续修订：{CORRECTION} [event #3]"),"source_sequences":[0,3]},
            {"id":"storage-recovery","title":"锁冲突的失败条件与恢复",
             "content":format!("历史失败：{FAILURE} [event #2]\n处理方式：{CORRECTION} [event #3]\n验证结果：{VERIFIED} [event #5]"),"source_sequences":[2,3,5]},
            {"id":"recovery-artifact","title":"恢复测试产物及范围",
             "content":format!("{VERIFIED} [event #5]"),"source_sequences":[5]}
        ],
        "tasks":[
            {"id":"storage-recovery","kind":"work","status":"completed","text":VERIFIED,
             "evidence":[{"sequence":5,"quote":VERIFIED}]}
        ]
    })
}

fn query(host: &ExtensionHost, events: &[RunEventEnvelope], args: Value) -> Value {
    serde_json::from_str(
        &host
            .memory
            .query_validated("archive", events, &args.to_string())
            .unwrap(),
    )
    .unwrap()
}

fn page<'a>(state: &'a WikiState, id: &str) -> &'a agent_memory::WikiPage {
    state.pages.iter().find(|page| page.id == id).unwrap()
}

#[tokio::test]
async fn decisions_failures_and_exact_results_survive_incremental_updates_restart_and_retrieval() {
    let dir = Directory::new();
    let mut events = source_history(CHOICE);
    command(&mut events, "first", 1, FAILURE);
    let fixture = ArchiveFixture::new(vec![
        first_update(),
        corrected_update(),
        json!({
            "overview":"存储历史见 storage-decision、storage-recovery、recovery-artifact；排版要求见 layout。",
            "pages":[{"id":"layout","title":"报告排版要求","content":"报告使用统一字体。 [event #6]","source_sequences":[6]}],
            "tasks":[]
        }),
    ]);
    let host = ExtensionHost::open(&dir.0).unwrap();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    memory
        .update(input("archive", &events).as_ref(), UpdateReason::Compaction)
        .await;
    let first = host.memory.load("archive").unwrap().unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(first.tasks[1].status, WikiTaskStatus::Failed);
    drop(memory);
    drop(host);

    // Recover from disk before a later batch corrects the failure. The compiler
    // receives full existing pages, while source ingestion resumes after #2.
    let host = ExtensionHost::open(&dir.0).unwrap();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    user(&mut events, CORRECTION);
    command(&mut events, "retry", 0, VERIFIED);
    memory
        .update(input("archive", &events).as_ref(), UpdateReason::Compaction)
        .await;
    let second = host.memory.load("archive").unwrap().unwrap();
    assert_eq!(second.revision, 2);
    let request = fixture.requests.lock().unwrap()[1].clone();
    let request = payload(&request);
    assert_eq!(request["new_source_events"][0]["sequence"], 3);
    for old in &first.pages {
        let supplied = request["editable_pages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|supplied| supplied["id"] == old.id)
            .unwrap();
        assert_eq!(supplied["content"], old.content);
        assert_eq!(supplied["source_sequences"], json!(old.source_sequences));
    }
    assert!(page(&second, "storage-decision").content.contains(CHOICE));
    assert!(
        page(&second, "storage-decision")
            .content
            .contains(CORRECTION)
    );
    assert!(page(&second, "storage-recovery").content.contains(FAILURE));
    assert!(page(&second, "storage-recovery").content.contains(VERIFIED));
    assert_eq!(second.tasks[1].status, WikiTaskStatus::Completed);
    assert_eq!(second.tasks[0], first.tasks[0]);

    // Moving to another topic must not remove old decision rationales, failure
    // conditions, outcome scope or citations, even when those pages are omitted.
    user(&mut events, "报告使用统一字体。");
    memory
        .update(input("archive", &events).as_ref(), UpdateReason::Compaction)
        .await;
    drop(memory);
    drop(host);
    let host = ExtensionHost::open(&dir.0).unwrap();
    let restored = host.memory.load("archive").unwrap().unwrap();
    assert_eq!(restored.revision, 3);
    for old in &second.pages {
        assert_eq!(page(&restored, &old.id), old);
    }
    assert_eq!(restored.tasks, second.tasks);

    for (keyword, id, original) in [
        ("维护成本", "storage-decision", CHOICE),
        ("database is locked", "storage-recovery", FAILURE),
        ("artifacts/recovery.json", "recovery-artifact", VERIFIED),
    ] {
        let matches = query(&host, &events, json!({"action":"search","query":keyword}));
        assert!(
            matches["pages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|page| page["id"] == id)
        );
        let read = query(&host, &events, json!({"action":"read","page_id":id}));
        assert!(read["content"].as_str().unwrap().contains(original));
    }
    for sequence in [0, 2, 3, 5] {
        let original = agent_memory::evidence_text(&events, sequence).unwrap();
        let read = query(
            &host,
            &events,
            json!({"action":"source","sequence":sequence}),
        );
        assert_eq!(read["content"], original);
    }
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        3,
        "retrieval makes no model calls"
    );
}

#[tokio::test]
async fn unsupported_new_evidence_cannot_overwrite_an_archived_failure_or_advance_its_cursor() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let mut events = source_history(CHOICE);
    command(&mut events, "first", 1, FAILURE);
    let fixture = ArchiveFixture::new(vec![
        first_update(),
        json!({
            "overview":"声称全部通过",
            "pages":[{"id":"storage-recovery","title":"验证完成","content":"声称所有条件都已验证。 [event #3]","source_sequences":[3]}],
            "tasks":[{"id":"storage-recovery","kind":"work","status":"completed","text":"全部通过",
                "evidence":[{"sequence":3,"quote":"所有测试已通过"}]}]
        }),
    ]);
    let memory = WikiMemory::new(host.clone(), fixture);
    memory
        .update(input("archive", &events).as_ref(), UpdateReason::Compaction)
        .await;
    let before = host.memory.load("archive").unwrap().unwrap();
    user(&mut events, "现在准备修复，尚未运行验证。");
    memory
        .update(input("archive", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(host.memory.load("archive").unwrap().unwrap(), before);
    let read = query(
        &host,
        &events,
        json!({"action":"read","page_id":"storage-recovery"}),
    );
    assert!(read["content"].as_str().unwrap().contains(FAILURE));
    assert!(RunMemoryView::load(&host, "archive", &events).pending);
}
