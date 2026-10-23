#[path = "resume_run/continuity.rs"]
mod continuity;
mod support;

use std::{
    convert::Infallible,
    fs,
    future::{Future, pending},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Waker},
};

use agent_adapters::WireProtocol;
use agent_core::agent::{
    Agent, AgentRun, AgentRunError, InterruptedToolOutcome, RunEvent, RunStatus,
};
use agent_runtime::{
    AgentRuntime, AgentRuntimeError, ApproximateTokenCounter, ContentBlock, ContextInput,
    ContextManager, ContextPolicy, EventStore, InMemoryNativeContextCheckpointStore,
    JsonlEventStore, Model, ModelFinishReason, ModelRequest, ModelResponse, ModelRole,
    NativeContextCompactor, NativeContextManager, ProviderContextIdentity, ProviderContextWindow,
    ProviderReplayScope, ToolCall, ToolDefinition, ToolExecutor,
};
use serde_json::json;

#[derive(Default)]
struct ModelSpy(Mutex<Vec<ModelRequest>>);
impl Model for ModelSpy {
    type Error = Infallible;
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        self.0.lock().unwrap().push(request);
        Ok(ModelResponse::final_answer("Continued safely".into()))
    }
}

struct NoRepeatTools;
impl ToolExecutor for NoRepeatTools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        panic!("recovery must never repeat historical tool effects")
    }
}

fn agent() -> Agent {
    Agent::new(
        "agent",
        "Agent",
        "",
        "Inspect actual state before continuing.",
    )
}

fn response(run: &mut AgentRun) {
    let response = ModelResponse {
        content: (1..=2)
            .map(|i| ContentBlock::ToolCall {
                id: format!("write-{i}"),
                name: "write".into(),
                arguments: "{}".into(),
            })
            .collect(),
        finish_reason: ModelFinishReason::ToolCalls,
    };
    run.record(RunEvent::ModelResponseReceived {
        content: response.text(),
        response: Some(response),
        provider_state: None,
        provider_continuation: None,
        token_usage: None,
    })
    .unwrap();
}

fn declare(run: &mut AgentRun, i: usize) {
    run.record(RunEvent::ToolCallRequested {
        call_id: format!("write-{i}"),
        tool_name: "write".into(),
        arguments: "{}".into(),
    })
    .unwrap();
}

#[tokio::test]
async fn every_interruption_boundary_can_resume_without_replaying_tools() {
    for boundary in 0..=6 {
        let runtime = AgentRuntime::new(
            ModelSpy::default(),
            NoRepeatTools,
            support::test_context_manager(),
            Some(1),
        );
        let mut run = runtime
            .start_run(&agent(), format!("boundary-{boundary}"), "Fix the file")
            .await
            .unwrap();
        if boundary >= 1 {
            run.begin_step().unwrap();
        }
        if boundary >= 2 {
            response(&mut run);
        }
        if boundary >= 3 {
            declare(&mut run, 1);
        }
        if boundary >= 4 {
            declare(&mut run, 2);
        }
        if boundary >= 5 {
            run.record(RunEvent::ToolCallSucceeded {
                call_id: "write-1".into(),
                output: "Committed once".into(),
                images: Vec::new(),
            })
            .unwrap();
        }
        if boundary >= 6 {
            run.record(RunEvent::ToolCallSucceeded {
                call_id: "write-2".into(),
                output: "Also committed".into(),
                images: Vec::new(),
            })
            .unwrap();
        }
        let history = run.events().to_vec();
        runtime
            .resume_run(&mut run, "Inspect the files and continue")
            .await
            .unwrap();
        assert_eq!(&run.events()[..history.len()], history.as_slice());
        let RunEvent::RunResumed {
            interrupted_calls, ..
        } = &run.events().last().unwrap().event
        else {
            panic!("missing recovery record")
        };
        let expected = match boundary {
            0 | 1 | 6 => 0,
            5 => 1,
            _ => 2,
        };
        assert_eq!(interrupted_calls.len(), expected);
        if boundary == 3 {
            assert_eq!(
                interrupted_calls[0].outcome,
                InterruptedToolOutcome::Unknown
            );
            assert_eq!(
                interrupted_calls[1].outcome,
                InterruptedToolOutcome::NotStarted
            );
        }
        assert!(run.interrupted_tool_calls().is_empty());
        assert!(run.can_begin_step());
        assert_eq!(
            runtime.run_until_blocked(&agent(), &mut run).await.unwrap(),
            RunStatus::Completed
        );
        let requests = runtime.model().0.lock().unwrap();
        let request = &requests[0];
        assert_eq!(
            request.messages.last().unwrap().text(),
            "Inspect the files and continue"
        );
        let results: Vec<_> = request
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    call_id,
                    content,
                    is_error,
                    ..
                } => Some((call_id, content, is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), if boundary >= 2 { 2 } else { 0 });
        if boundary == 5 {
            assert_eq!(results[0].1, "Committed once");
            assert!(!results[0].2);
            assert!(results[1].2);
            assert!(
                results[1]
                    .1
                    .contains("may have partially or fully taken effect")
            );
        }
    }
}

#[tokio::test]
async fn terminal_runs_continue_with_the_same_identity_and_fresh_step_budget() {
    for terminal in [
        RunStatus::Completed,
        RunStatus::Cancelled,
        RunStatus::Failed,
    ] {
        let runtime = AgentRuntime::new(
            ModelSpy::default(),
            NoRepeatTools,
            support::test_context_manager(),
            Some(1),
        );
        let mut run = runtime
            .start_run(&agent(), format!("terminal-{terminal:?}"), "Work")
            .await
            .unwrap();
        if terminal == RunStatus::Completed {
            runtime.run_until_blocked(&agent(), &mut run).await.unwrap();
        } else {
            run.begin_step().unwrap();
            response(&mut run);
            declare(&mut run, 1);
            run.record(if terminal == RunStatus::Cancelled {
                RunEvent::RunCancelled
            } else {
                RunEvent::RunFailed {
                    message: "interrupted write".into(),
                }
            })
            .unwrap();
        }
        let id = run.id().to_owned();
        let old_step = run.current_step();
        runtime
            .resume_run(&mut run, "Continue carefully")
            .await
            .unwrap();
        assert_eq!(
            runtime.run_until_blocked(&agent(), &mut run).await.unwrap(),
            RunStatus::Completed
        );
        assert_eq!(run.id(), id);
        assert_eq!(run.current_step(), old_step + 1);
        let stored = runtime.restore_run(&id).await.unwrap().unwrap();
        assert_eq!(stored.events(), run.events());
    }
}

#[test]
fn forged_or_incomplete_recovery_outcomes_are_rejected_on_write_and_restore() {
    let mut run = agent().start_run("forged", "Work");
    run.begin_step().unwrap();
    response(&mut run);
    declare(&mut run, 1);
    let mut wrong = run.interrupted_tool_calls();
    wrong[0].outcome = InterruptedToolOutcome::NotStarted;
    assert_eq!(
        run.record(RunEvent::RunResumed {
            files: Vec::new(),
            content: "Continue".into(),
            interrupted_calls: wrong
        })
        .unwrap_err(),
        AgentRunError::InvalidResume
    );
    assert_eq!(
        run.record(RunEvent::RunResumed {
            files: Vec::new(),
            content: "Continue".into(),
            interrupted_calls: vec![]
        })
        .unwrap_err(),
        AgentRunError::InvalidResume
    );
    let event = RunEvent::RunResumed {
        files: Vec::new(),
        content: "Continue".into(),
        interrupted_calls: run.interrupted_tool_calls(),
    };
    run.record(event).unwrap();
    assert!(run.has_tool_call("write-2"));
    let mut history = run.events().to_vec();
    let RunEvent::RunResumed {
        interrupted_calls, ..
    } = &mut history.last_mut().unwrap().event
    else {
        unreachable!()
    };
    interrupted_calls.pop();
    assert!(AgentRun::restore("forged", "agent", history).is_err());
}

#[tokio::test]
async fn recovery_survives_a_second_restart_and_preserves_existing_results() {
    let directory = TestDirectory::new();
    let mut run = agent().start_run("persisted", "Work");
    run.begin_step().unwrap();
    response(&mut run);
    declare(&mut run, 1);
    declare(&mut run, 2);
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "write-1".into(),
        output: "Already written".into(),
        images: Vec::new(),
    })
    .unwrap();
    let store = JsonlEventStore::new(&directory.0);
    for event in run.events() {
        store.append(run.id(), run.agent_id(), event).await.unwrap();
    }
    let runtime = AgentRuntime::with_event_store(
        ModelSpy::default(),
        NoRepeatTools,
        support::test_context_manager(),
        store,
        None,
    );
    let mut restored = runtime.restore_run("persisted").await.unwrap().unwrap();
    runtime.resume_run(&mut restored, "Continue").await.unwrap();
    drop(runtime);
    let runtime = AgentRuntime::with_event_store(
        ModelSpy::default(),
        NoRepeatTools,
        support::test_context_manager(),
        JsonlEventStore::new(&directory.0),
        None,
    );
    let mut restored_again = runtime.restore_run("persisted").await.unwrap().unwrap();
    assert_eq!(restored.events(), restored_again.events());
    assert!(restored_again.can_begin_step());
    runtime
        .run_until_blocked(&agent(), &mut restored_again)
        .await
        .unwrap();
    let requests = runtime.model().0.lock().unwrap();
    assert!(
        serde_json::to_string(&requests[0])
            .unwrap()
            .contains("Already written")
    );
}

struct PendingModel;
impl Model for PendingModel {
    type Error = Infallible;
    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        pending().await
    }
}

#[tokio::test]
async fn active_execution_and_stale_continuations_cannot_be_resumed_concurrently() {
    let runtime = AgentRuntime::new(
        PendingModel,
        NoRepeatTools,
        support::test_context_manager(),
        None,
    );
    let mut run = runtime.start_run(&agent(), "busy", "Work").await.unwrap();
    let mut stale = run.clone();
    let agent = agent();
    let mut execution = Box::pin(runtime.run_until_blocked(&agent, &mut run));
    assert!(
        execution
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    assert!(matches!(
        runtime.resume_run(&mut stale, "Continue").await,
        Err(AgentRuntimeError::RunAlreadyExecuting { .. })
    ));
    drop(execution);
    assert!(matches!(
        runtime.resume_run(&mut stale, "Continue").await,
        Err(AgentRuntimeError::RunOutOfDate { .. })
    ));
    let mut restored = runtime.restore_run("busy").await.unwrap().unwrap();
    runtime.resume_run(&mut restored, "Continue").await.unwrap();
    assert!(restored.can_begin_step());
}

#[tokio::test]
async fn recovery_keeps_reasoning_and_protocol_specific_state_unchanged() {
    let cases = [
        (
            WireProtocol::ChatCompletions,
            json!({"role":"assistant", "content":"Inspecting", "reasoning_content":"Keep thinking", "tool_calls":[{"id":"call-1", "type":"function", "function":{"name":"write", "arguments":"{}"}}]}),
        ),
        (
            WireProtocol::Responses,
            json!([{"type":"reasoning", "encrypted_content":"private-state", "summary":[]}, {"type":"function_call", "call_id":"call-1", "name":"write", "arguments":"{}"}]),
        ),
        (
            WireProtocol::AnthropicMessages,
            json!({"role":"assistant", "content":[{"type":"thinking", "thinking":"Keep thinking", "signature":"signed-state"}, {"type":"tool_use", "id":"call-1", "name":"write", "input":{}}]}),
        ),
    ];
    for (protocol, payload) in cases {
        let identity = ProviderContextIdentity {
            provider: "fixture".into(),
            model: "reasoning".into(),
            format: protocol.as_str().into(),
        };
        let state = protocol
            .capture_state(
                identity.clone(),
                ProviderReplayScope::Response,
                payload.clone(),
                30,
            )
            .unwrap();
        let response = ModelResponse {
            content: protocol.decode_content(&payload, "response-1").unwrap(),
            finish_reason: ModelFinishReason::ToolCalls,
        };
        let runtime = AgentRuntime::new(
            ModelSpy::default(),
            NoRepeatTools,
            support::test_context_manager(),
            None,
        );
        let mut run = runtime
            .start_run(&agent(), protocol.as_str(), "Work")
            .await
            .unwrap();
        run.begin_step().unwrap();
        run.record(RunEvent::ModelResponseReceived {
            content: response.text(),
            response: Some(response.clone()),
            provider_state: Some(state.clone()),
            provider_continuation: None,
            token_usage: None,
        })
        .unwrap();
        runtime
            .resume_run(&mut run, "Verify before continuing")
            .await
            .unwrap();
        let prepared = runtime
            .context_manager()
            .prepare_owned(ContextInput::new(
                run.id(),
                "",
                run.events().to_vec(),
                vec![],
            ))
            .await
            .unwrap();
        let assistant = prepared
            .request
            .messages
            .iter()
            .find(|m| m.role == ModelRole::Assistant)
            .unwrap();
        assert_eq!(assistant.provider_state, Some(state));
        assert_eq!(assistant.content, response.content);
        prepared
            .request
            .validate_provider_state_with(
                Some(&identity),
                true,
                agent_adapters::validate_replay_state,
            )
            .unwrap();
        assert!(matches!(
            &prepared.request.messages[2].content[0],
            ContentBlock::ToolResult { is_error: true, .. }
        ));
    }
}

struct NoNativeCompaction(ProviderContextIdentity);
impl NativeContextCompactor for NoNativeCompaction {
    type Error = Infallible;
    fn identity(&self) -> &ProviderContextIdentity {
        &self.0
    }
    fn input_token_limit(&self) -> usize {
        100_000
    }
    async fn compact(
        &self,
        _: ModelRequest,
        _: usize,
    ) -> Result<ProviderContextWindow, Self::Error> {
        panic!("small recovered fixture must not compact")
    }
}

#[tokio::test]
async fn native_continuations_receive_missing_results_without_replaying_calls() {
    let identity = ProviderContextIdentity {
        provider: "fixture".into(),
        model: "native".into(),
        format: "responses-v1".into(),
    };
    let window = ProviderContextWindow {
        identity: identity.clone(),
        items: vec![
            json!({"role":"user", "content":"Work"}),
            json!({"type":"reasoning", "encrypted_content":"opaque"}),
            json!({"type":"function_call", "call_id":"write-1", "name":"write", "arguments":"{}"}),
        ],
        input_tokens: 100,
        prefix_fingerprint: None,
    };
    let manager = NativeContextManager::new(
        ContextPolicy::default(),
        NoNativeCompaction(identity),
        InMemoryNativeContextCheckpointStore::new(),
        ApproximateTokenCounter::default(),
    );
    let runtime = AgentRuntime::new(ModelSpy::default(), NoRepeatTools, manager, None);
    let mut run = runtime.start_run(&agent(), "native", "Work").await.unwrap();
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        content: String::new(),
        response: Some(ModelResponse {
            content: vec![ContentBlock::ToolCall {
                id: "write-1".into(),
                name: "write".into(),
                arguments: "{}".into(),
            }],
            finish_reason: ModelFinishReason::ToolCalls,
        }),
        provider_state: None,
        provider_continuation: Some(Box::new(agent_core::provider_state::ProviderContinuation {
            window: window.clone(),
            compacted: false,
        })),
        token_usage: None,
    })
    .unwrap();
    runtime
        .resume_run(&mut run, "Continue safely")
        .await
        .unwrap();
    let prepared = runtime
        .context_manager()
        .prepare_owned(ContextInput::new(
            run.id(),
            "",
            run.events().to_vec(),
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(prepared.request.provider_context, Some(window));
    assert_eq!(prepared.request.messages.len(), 2);
    assert!(
        matches!(&prepared.request.messages[0].content[0], ContentBlock::ToolResult { call_id, is_error: true, .. } if call_id == "write-1")
    );
    assert_eq!(prepared.request.messages[1].text(), "Continue safely");
}

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "openharness-resume-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
