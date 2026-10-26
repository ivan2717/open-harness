//! Host integration checks use only a loopback model fixture, never a live API.
use super::*;
use crate::{
    contracts::{ConfigureModel, RunDetail, RunStatus, RunSummary},
    extensions::ExtensionHost,
    memory::WikiMemory,
    model::ServiceModel,
};
use agent_core::agent::RunEvent;
use agent_providers::ProviderProtocol;
use agent_runtime::{ContextInput, ModelResponse};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tokio::sync::Notify;

const MIDDLE: &str = "CANONICAL-MIDDLE-EVIDENCE";
const FRESH: &str = "Fresh wiki preserves CANONICAL-MIDDLE-EVIDENCE";
static SERIAL: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct ServerState {
    requests: Mutex<Vec<Value>>,
    fail: AtomicBool,
    gated: AtomicBool,
    saw_middle: AtomicBool,
    started: Notify,
    release: Notify,
    finished: Notify,
}

struct Fixture {
    directory: PathBuf,
    endpoint: String,
    state: Arc<ServerState>,
    server: tokio::task::JoinHandle<()>,
}

type CompactionCalls = Arc<Mutex<Vec<Vec<RunEventEnvelope>>>>;

struct Session {
    context: ObservedContext<RecordingCompactor>,
    memory: Arc<WikiMemory>,
    host: Arc<ExtensionHost>,
    calls: CompactionCalls,
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "open-harness-wiki-hook-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let state = Arc::new(ServerState::default());
        let app = Router::new()
            .route("/v1/chat/completions", post(respond))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            directory,
            endpoint,
            state,
            server,
        }
    }

    fn context(
        &self,
        run_id: &str,
        limit: usize,
        threshold: usize,
        require_memory: bool,
    ) -> Session {
        let directory = self.directory.join(run_id);
        fs::create_dir_all(&directory).unwrap();
        let host = ExtensionHost::open(&directory.join("host")).unwrap();
        let configured = crate::model::configure(ConfigureModel {
            supports_images: false,
            protocol: ProviderProtocol::ChatCompletions,
            base_url: self.endpoint.clone(),
            api_key: "local-fixture-only".into(),
            model: "wiki-hook-fixture".into(),
            context_window: 128_000,
            max_output_tokens: 4_096,
            reasoning_mode: "off".into(),
            reasoning_effort: "medium".into(),
            reasoning_capability: None,
            reasoning_default: None,
        })
        .unwrap();
        let memory = Arc::new(WikiMemory::new(
            host.clone(),
            ServiceModel::Http(Arc::new(configured.model)),
        ));
        let snapshot = Snapshot::new(
            RunDetail {
                summary: RunSummary {
                    reasoning_settings_version: 1,
                    reasoning_effort: None,
                    model_history_through: None,
                    delegation: None,
                    execution_policy: Default::default(),
                    compacting: false,
                    id: run_id.into(),
                    revision: 0,
                    title: "Wiki hook test".into(),
                    prompt: "Preserve original evidence".into(),
                    status: RunStatus::Running,
                    model_id: "fixture".into(),
                    workspace_path: None,
                    permission_mode: Default::default(),
                    created_at: String::new(),
                    updated_at: String::new(),
                },
                messages: vec![],
                events: vec![],
                context: None,
                error: None,
            },
            directory.join("snapshot.json"),
        )
        .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let context = ObservedContext::with_compactor(
            directory.join("checkpoints.json"),
            snapshot,
            ContextPolicy::builder()
                .input_token_limit(limit)
                .compaction_threshold(threshold)
                .compaction_target(threshold / 2)
                .recent_tail_tokens(1)
                .max_tool_output_chars(1_000)
                .estimation_margin_percent(0)
                .build()
                .unwrap(),
            RecordingCompactor {
                host: host.clone(),
                run_id: run_id.into(),
                calls: calls.clone(),
                require_memory,
            },
        )
        .unwrap()
        .with_memory(memory.clone());
        Session {
            context,
            memory,
            host,
            calls,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn respond(State(state): State<Arc<ServerState>>, Json(request): Json<Value>) -> Response {
    state.requests.lock().unwrap().push(request.clone());
    state.started.notify_one();
    if state.gated.load(Ordering::Relaxed) {
        state.release.notified().await;
    }
    if state.fail.load(Ordering::Relaxed) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"message":"fixture failure"}})),
        )
            .into_response();
    }
    let payload = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|message| {
            let payload: Value = serde_json::from_str(message["content"].as_str()?).ok()?;
            payload.get("new_source_events")?;
            Some(payload)
        })
        .unwrap();
    if payload["new_source_events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["text"].as_str().unwrap().contains(MIDDLE))
    {
        state.saw_middle.store(true, Ordering::Relaxed);
    }
    let content = json!({
        "overview": if state.saw_middle.load(Ordering::Relaxed) { FRESH } else { "Fresh session notes" },
        "pages": [], "tasks": [],
    }).to_string();
    state.finished.notify_one();
    Json(json!({"id":"wiki-hook", "choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]})).into_response()
}

struct RecordingCompactor {
    host: Arc<ExtensionHost>,
    run_id: String,
    calls: CompactionCalls,
    require_memory: bool,
}

impl ContextCompactor for RecordingCompactor {
    type Error = Infallible;
    fn estimate_input_tokens(
        &self,
        _: Option<&ContextSummary>,
        _: &[RunEventEnvelope],
        _: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        Ok(0)
    }
    async fn compact(
        &self,
        _: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        if self.require_memory {
            let state = self
                .host
                .memory
                .load(&self.run_id)
                .unwrap()
                .expect("Wiki must be committed before summarizing");
            assert_eq!(state.overview, FRESH);
            assert!(state.source_cursor.unwrap().sequence > events.last().unwrap().sequence);
        }
        self.calls.lock().unwrap().push(events);
        Ok(ContextSummary {
            objective: "Continue from the preserved source evidence".into(),
            ..Default::default()
        })
    }
}

fn push(events: &mut Vec<RunEventEnvelope>, step: u32, event: RunEvent) {
    events.push(RunEventEnvelope {
        sequence: events.len() as u64,
        step,
        occurred_at: SystemTime::UNIX_EPOCH,
        event,
    });
}

fn history(run_id: &str, assistant_chars: usize, with_tool: bool) -> ContextInput {
    let mut events = Vec::new();
    push(
        &mut events,
        0,
        RunEvent::RunStarted {
            instruction: "Preserve the original evidence.".into(),
            files: vec![],
        },
    );
    if with_tool {
        push(
            &mut events,
            1,
            RunEvent::ToolCallRequested {
                call_id: "read".into(),
                tool_name: "read_file".into(),
                arguments: "{\"path\":\"source.txt\"}".into(),
            },
        );
        push(
            &mut events,
            1,
            RunEvent::ToolCallSucceeded {
                images: vec![],
                call_id: "read".into(),
                output: format!("{}{MIDDLE}{}", "a".repeat(6_000), "z".repeat(6_000)),
            },
        );
    }
    push(
        &mut events,
        1,
        RunEvent::ModelResponseReceived {
            response: None,
            provider_continuation: None,
            token_usage: None,
            provider_state: None,
            content: "x".repeat(assistant_chars),
        },
    );
    push(
        &mut events,
        2,
        RunEvent::InputReceived {
            content: "Continue the same task.".into(),
            files: vec![],
        },
    );
    ContextInput::new(
        run_id,
        "Keep facts backed by original evidence.",
        events,
        vec![],
    )
}

#[tokio::test]
async fn automatic_compaction_commits_original_evidence_first_and_uses_fresh_notes() {
    let fixture = Fixture::new().await;
    let Session {
        context,
        memory,
        host,
        calls,
    } = fixture.context("canonical", 6_000, 4_000, true);
    let input = history("canonical", 20_000, true);
    let prepared = context.prepare(input.as_ref()).await.unwrap();
    assert!(!calls.lock().unwrap().is_empty());
    assert!(prepared.omitted_history.is_some());
    assert!(
        prepared
            .request
            .messages
            .iter()
            .any(|message| message.text().contains(FRESH))
    );
    assert!(fixture.state.saw_middle.load(Ordering::Relaxed));
    assert!(
        fixture.state.requests.lock().unwrap().len() >= 2,
        "coverage spans multiple bounded batches"
    );
    assert_eq!(
        host.memory
            .validate_source("canonical", &input.events)
            .unwrap()
            .unwrap()
            .overview,
        FRESH
    );
    // The ordinary compaction projection has clipped the tool payload, but the
    // Wiki compiler received its original middle rather than the clipped copy.
    assert!(!calls.lock().unwrap().iter().flatten().any(|event| matches!(&event.event, RunEvent::ToolCallSucceeded { output, .. } if output.contains(MIDDLE))));
    memory.stop_background().await;
}

#[tokio::test]
async fn ordinary_prepare_and_response_do_not_wait_for_background_memory() {
    let fixture = Fixture::new().await;
    fixture.state.gated.store(true, Ordering::Relaxed);
    let Session {
        context,
        memory,
        host,
        calls,
    } = fixture.context("background", 64_000, 50_000, false);
    let input = history("background", 6_000, false);
    let output = ModelResponse::final_answer("Answer is complete".into()).into();
    tokio::time::timeout(Duration::from_millis(250), context.prepare(input.as_ref()))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_millis(250),
        context.observe_response(input.as_ref(), &output),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), fixture.state.started.notified())
        .await
        .unwrap();
    // A running compiler must not hold the normal reply path behind its lock.
    tokio::time::timeout(Duration::from_millis(250), context.prepare(input.as_ref()))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_millis(250),
        context.observe_response(input.as_ref(), &output),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(calls.lock().unwrap().is_empty());
    memory.stop_background().await;
    fixture.state.release.notify_one();
    // The loopback handler may be dropped when cancellation closes its socket.
    // Either outcome must leave the committed memory unchanged.
    let _ = tokio::time::timeout(
        Duration::from_millis(100),
        fixture.state.finished.notified(),
    )
    .await;
    assert!(
        host.memory.load("background").unwrap().is_none(),
        "stopped background request cannot publish its response"
    );
}

#[tokio::test]
async fn failed_wiki_defers_optional_compression_but_not_required_or_manual_compression() {
    let fixture = Fixture::new().await;
    fixture.state.fail.store(true, Ordering::Relaxed);
    for (name, limit, manual, should_compact) in [
        ("optional", 8_000, false, false),
        ("required", 3_000, false, true),
        ("manual", 8_000, true, true),
    ] {
        let Session {
            context,
            memory,
            host,
            calls,
        } = fixture.context(name, limit, 2_500, false);
        let input = history(name, 14_000, false);
        let prepared = if manual {
            context.compact_now(input.as_ref()).await.unwrap().unwrap()
        } else {
            context.prepare(input.as_ref()).await.unwrap()
        };
        assert_eq!(!calls.lock().unwrap().is_empty(), should_compact, "{name}");
        assert_eq!(prepared.omitted_history.is_some(), should_compact, "{name}");
        assert!(host.memory.load(name).unwrap().is_none());
        memory.stop_background().await;
    }
}

#[derive(Clone, Copy)]
enum DisabledCompaction {
    Automatic,
    Manual,
    Overflow,
}

async fn assert_compaction_without_wiki(trigger: DisabledCompaction) {
    let fixture = Fixture::new().await;
    // Any unexpected Wiki request fails rather than manufacturing a useful
    // archive that could hide an accidental dependency on the plugin.
    fixture.state.fail.store(true, Ordering::Relaxed);
    let (run_id, threshold) = match trigger {
        DisabledCompaction::Automatic => ("disabled-automatic", 2_500),
        DisabledCompaction::Manual => ("disabled-manual", 6_000),
        DisabledCompaction::Overflow => ("disabled-overflow", 6_000),
    };
    let Session {
        context,
        memory,
        host,
        calls,
    } = fixture.context(run_id, 8_000, threshold, false);
    // This history exceeds the soft automatic threshold, but is comfortably
    // below the hard input limit. Disabling Wiki must not defer that soft pass.
    let input = history(run_id, 14_000, false);
    let original_events = input.events.clone();
    host.memory
        .apply(
            run_id,
            &input.events,
            0,
            agent_memory::WikiUpdate {
                overview: "Previously saved wiki evidence".into(),
                pages: vec![],
                tasks: vec![],
            },
        )
        .unwrap();
    let original_archive = host.memory.load(run_id).unwrap().unwrap();
    assert!(!memory.messages(input.as_ref()).is_empty());
    host.set_plugin_enabled(crate::memory::PLUGIN_ID, false)
        .unwrap();
    assert!(memory.messages(input.as_ref()).is_empty());

    let prepared = match trigger {
        DisabledCompaction::Automatic => context.prepare(input.as_ref()).await.unwrap(),
        DisabledCompaction::Manual | DisabledCompaction::Overflow => {
            let original = context.prepare(input.as_ref()).await.unwrap();
            assert!(calls.lock().unwrap().is_empty());
            assert!(original.omitted_history.is_none());
            assert!(
                original
                    .request
                    .messages
                    .iter()
                    .all(|message| !message.text().contains("Session memory:"))
            );
            let compacted = match trigger {
                DisabledCompaction::Manual => {
                    context.compact_now(input.as_ref()).await.unwrap().unwrap()
                }
                DisabledCompaction::Overflow => context
                    .recover_overflow(input.as_ref(), &original.request)
                    .await
                    .unwrap()
                    .expect("overflow recovery must produce a smaller request"),
                DisabledCompaction::Automatic => unreachable!(),
            };
            assert!(
                context.estimate_request_tokens(&compacted.request)
                    < context.estimate_request_tokens(&original.request)
            );
            compacted
        }
    };
    assert!(!calls.lock().unwrap().is_empty());
    assert!(prepared.omitted_history.is_some());
    assert!(prepared.request.messages.iter().any(|message| {
        message
            .text()
            .contains("Continue from the preserved source evidence")
    }));
    assert!(prepared.request.messages.iter().all(|message| {
        let text = message.text();
        !text.contains("Session memory:") && !text.contains("Previously saved wiki evidence")
    }));
    let checkpoint_bytes = fs::read(fixture.directory.join(run_id).join("checkpoints.json"))
        .expect("Compact must persist its own checkpoint without Wiki");
    let checkpoints: Vec<ContextCheckpoint> = serde_json::from_slice(&checkpoint_bytes).unwrap();
    assert!(!checkpoints.is_empty());

    // Exercise the normal response scheduling path as well as pre-compaction
    // catch-up. Neither may call the compiler after the plugin is disabled.
    let output = ModelResponse::final_answer("Continue from the compact summary".into()).into();
    context
        .observe_response(input.as_ref(), &output)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(700), fixture.state.started.notified())
            .await
            .is_err(),
        "a disabled Wiki must not issue foreground or background requests"
    );
    memory.stop_background().await;
    assert!(fixture.state.requests.lock().unwrap().is_empty());
    assert_eq!(input.events, original_events);
    assert_eq!(
        host.memory.validate_source(run_id, &input.events).unwrap(),
        Some(original_archive),
        "disabling Wiki preserves the archive and its original source binding"
    );
}

#[tokio::test]
async fn disabled_wiki_does_not_defer_soft_automatic_compaction() {
    assert_compaction_without_wiki(DisabledCompaction::Automatic).await;
}

#[tokio::test]
async fn disabled_wiki_does_not_block_manual_compaction() {
    assert_compaction_without_wiki(DisabledCompaction::Manual).await;
}

#[tokio::test]
async fn disabled_wiki_does_not_block_provider_overflow_recovery() {
    assert_compaction_without_wiki(DisabledCompaction::Overflow).await;
}
