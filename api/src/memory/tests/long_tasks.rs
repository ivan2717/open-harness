use super::*;
use agent_memory::{
    WikiEvidence, WikiPageUpdate, WikiTaskItem, WikiTaskKind, WikiTaskStatus, WikiUpdate,
};

#[tokio::test]
async fn long_event_survives_restart_and_failed_batch_without_missing_middle() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let original = format!(
        "{}不要引入 tokenizer，也不要调用计数 API。{}",
        "前期资料🙂".repeat(12_000),
        "后续记录🙂".repeat(12_000)
    );
    let events = source_history(&original);
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    memory
        .update(input("long", &events).as_ref(), UpdateReason::Compaction)
        .await;
    let first = host.memory.load("long").unwrap().unwrap();
    assert_eq!(first.source_cursor.unwrap().sequence, 0);
    assert!(first.source_cursor.unwrap().offset > 0);
    fixture.mode.store(1, Ordering::Relaxed);
    memory
        .update(input("long", &events).as_ref(), UpdateReason::Compaction)
        .await;
    assert_eq!(host.memory.load("long").unwrap().unwrap(), first);
    // A failed generation consumed no source. Retry from the same durable
    // cursor using a recreated host, as after process recovery.
    fixture.requests.lock().unwrap().pop();
    fixture.mode.store(0, Ordering::Relaxed);
    let restored = ExtensionHost::open(&dir.0).unwrap();
    let memory = WikiMemory::new(restored.clone(), fixture.clone());
    for _ in 0..128 {
        memory
            .update(input("long", &events).as_ref(), UpdateReason::Progress)
            .await;
        if restored
            .memory
            .load("long")
            .unwrap()
            .unwrap()
            .source_cursor
            .unwrap()
            .sequence
            == 1
        {
            break;
        }
    }
    let state = restored.memory.load("long").unwrap().unwrap();
    assert_eq!(
        state.source_cursor,
        Some(agent_memory::WikiSourceCursor {
            sequence: 1,
            offset: 0
        })
    );
    let mut recovered = String::new();
    for request in fixture.requests() {
        for source in payload(&request)["new_source_events"].as_array().unwrap() {
            assert_eq!(source["offset"], recovered.chars().count());
            recovered.push_str(source["text"].as_str().unwrap());
        }
    }
    assert_eq!(recovered, original);
    assert!(recovered.contains("不要引入 tokenizer"));
}

#[tokio::test]
async fn long_history_recall_prioritizes_active_constraints_and_relevant_old_pages() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let mut events = source_history("不要引入 tokenizer。允许使用本地 Wiki。");
    for _ in 0..200 {
        user(&mut events, "处理其他任务，与文档排版有关。");
    }
    assistant_work(&mut events);
    user(
        &mut events,
        "继续优化 token 估算，之前对 tokenizer 有什么约束？",
    );
    let mut pages = vec![WikiPageUpdate {
        id: "token-policy".into(),
        title: "tokenizer 选择".into(),
        content: "曾决定不用 tokenizer，使用轻量估算。".into(),
        source_sequences: vec![0],
    }];
    for index in 0..7 {
        pages.push(WikiPageUpdate {
            id: format!("layout-{index}"),
            title: format!("文档排版 {index}"),
            content: "文档采用统一样式。".into(),
            source_sequences: vec![200],
        });
    }
    host.memory
        .apply(
            "recall",
            &events,
            200,
            WikiUpdate {
                overview: "长期任务正在继续。".into(),
                pages,
                tasks: vec![WikiTaskItem {
                    id: "token-policy".into(),
                    kind: WikiTaskKind::Constraint,
                    status: WikiTaskStatus::Active,
                    text: "不要引入 tokenizer。".into(),
                    evidence: vec![WikiEvidence {
                        sequence: 0,
                        quote: "不要引入 tokenizer。".into(),
                    }],
                }],
            },
        )
        .unwrap();
    let memory = WikiMemory::new(host.clone(), Fixture::new());
    let text = memory.messages(input("recall", &events).as_ref())[0].text();
    assert!(text.contains("不要引入 tokenizer。"));
    assert!(text.contains("Related page token-policy"));
    assert!(text.contains("\"status\":\"active\""));
    assert!(text.chars().count() <= 4_000);
    assert!(RunMemoryView::load(&host, "recall", &events).pending);
    assert!(
        memory
            .messages(input("another-session", &events).as_ref())
            .is_empty()
    );

    use agent_runtime::{
        CompactingContextManager, ContextCheckpointStore, ContextManager, ContextPolicy,
        InMemoryContextCheckpointStore,
    };
    let checkpoints = InMemoryContextCheckpointStore::new();
    let mut previous_through = None;
    for cycle in 0..2 {
        let manager = CompactingContextManager::new(
            ContextPolicy::builder()
                .input_token_limit(32_000)
                .compaction_threshold(30_000)
                .recent_tail_tokens(1)
                .estimation_margin_percent(0)
                .build()
                .unwrap(),
            LossyCompactor,
            checkpoints.clone(),
            ApproximateTokenCounter::new(1).unwrap(),
        );
        let original = input("recall", &events);
        let recalled = memory.messages(original.as_ref());
        let prepared = manager
            .prepare(original.as_ref().with_derived_messages(&recalled))
            .await
            .unwrap();
        assert!(prepared.omitted_history.is_some());
        assert!(
            prepared.request.messages[0]
                .text()
                .contains("不要引入 tokenizer。")
        );
        assert!(
            prepared.request.messages[0]
                .text()
                .contains("session_memory source")
        );
        let checkpoint = checkpoints.latest("recall".into()).await.unwrap().unwrap();
        let through = checkpoint.summary_through_sequence().unwrap();
        assert!(previous_through.is_none_or(|previous| through > previous));
        previous_through = Some(through);
        // The next manager reuses saved checkpoints, as after recovery, while
        // another large stretch of work causes a second compression.
        if cycle == 0 {
            assistant_work(&mut events);
            user(
                &mut events,
                "继续优化 token 估算，之前对 tokenizer 有什么约束？",
            );
        }
    }
}

fn assistant_work(events: &mut Vec<RunEventEnvelope>) {
    append(
        events,
        RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "Detailed processing result. ".repeat(4_000),
        },
    );
}

struct LossyCompactor;
impl agent_runtime::ContextCompactor for LossyCompactor {
    type Error = std::convert::Infallible;
    fn estimate_input_tokens(
        &self,
        _: Option<&agent_runtime::ContextSummary>,
        _: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(0)
    }
    async fn compact(
        &self,
        _: Option<agent_runtime::ContextSummary>,
        _: Vec<RunEventEnvelope>,
    ) -> Result<agent_runtime::ContextSummary, Self::Error> {
        Ok(agent_runtime::ContextSummary {
            objective: "Continue the task".into(),
            ..Default::default()
        })
    }
}

#[derive(Clone)]
struct IncorrectCompletion;
impl Model for IncorrectCompletion {
    type Error = io::Error;
    async fn respond(&self, request: ModelRequest) -> io::Result<ModelResponse> {
        let sources = payload(&request);
        let source = &sources["new_source_events"][0];
        Ok(ModelResponse::final_answer(
            json!({
                "overview":"测试通过", "pages":[], "tasks":[{
                    "id":"test", "kind":"work", "status":"completed", "text":"测试通过",
                    "evidence":[{"sequence":source["sequence"],"quote":source["text"]}]
                }]
            })
            .to_string(),
        ))
    }
}

#[tokio::test]
async fn compiler_cannot_turn_a_plan_into_completed_work_or_advance_progress() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let events = source_history("准备运行测试，并不表示已经通过。");
    let memory = WikiMemory::new(host.clone(), IncorrectCompletion);
    memory
        .update(
            input("unsupported", &events).as_ref(),
            UpdateReason::Compaction,
        )
        .await;
    assert!(host.memory.load("unsupported").unwrap().is_none());
    assert_eq!(
        RunMemoryView::load(&host, "unsupported", &events).status,
        "error"
    );
    assert!(
        memory
            .messages(input("unsupported", &events).as_ref())
            .is_empty()
    );
}
