mod support;

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, ContentBlock, Model, ModelBlockKind, ModelFinishReason, ModelRequest,
    ModelResponse, ModelStreamEvent, ModelStreamSink, PendingUserInput, ToolCall, ToolDefinition,
    ToolExecutor, UserInputQueue,
};
use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Notify, watch};

#[derive(Debug)]
struct Queue {
    inputs: Mutex<Vec<PendingUserInput>>,
    changed: watch::Sender<bool>,
}
impl Queue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inputs: Mutex::new(Vec::new()),
            changed: watch::channel(false).0,
        })
    }
    fn push(&self, steer: bool) {
        self.inputs.lock().unwrap().push(PendingUserInput {
            id: "correction".into(),
            content: "Use the revised plan".into(),
            files: vec![],
            steer,
            delivering: false,
            revision: 0,
        });
        self.changed.send_replace(steer);
    }
}
impl UserInputQueue for Queue {
    fn claim(&self) -> std::io::Result<Option<PendingUserInput>> {
        let mut inputs = self.inputs.lock().unwrap();
        let input = inputs
            .iter_mut()
            .find(|input| !input.delivering && input.steer);
        Ok(input.map(|input| {
            input.delivering = true;
            input.clone()
        }))
    }
    fn acknowledge(&self, id: &str) -> std::io::Result<()> {
        self.inputs.lock().unwrap().retain(|input| input.id != id);
        self.changed.send_replace(false);
        Ok(())
    }
    fn subscribe(&self) -> watch::Receiver<bool> {
        self.changed.subscribe()
    }
}

struct ModelFixture {
    calls: AtomicUsize,
    started: Notify,
    release: Notify,
    requests: Mutex<Vec<ModelRequest>>,
    tools_first: bool,
}
impl ModelFixture {
    fn new(tools_first: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            started: Notify::new(),
            release: Notify::new(),
            requests: Mutex::new(vec![]),
            tools_first,
        }
    }
}
impl Model for ModelFixture {
    type Error = Infallible;
    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Infallible> {
        unreachable!()
    }
    async fn stream(
        &self,
        request: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Infallible> {
        self.requests.lock().unwrap().push(request);
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first && self.tools_first {
            for i in 0..2 {
                sink.emit(ModelStreamEvent::BlockStart {
                    index: i,
                    kind: ModelBlockKind::ToolCall,
                });
                sink.emit(ModelStreamEvent::BlockEnd {
                    index: i,
                    block: ContentBlock::ToolCall {
                        id: format!("tool-{i}"),
                        name: "write".into(),
                        arguments: "{}".into(),
                    },
                });
            }
            sink.emit(ModelStreamEvent::Finish {
                finish_reason: ModelFinishReason::ToolCalls,
                provider_state: None,
                provider_context: None,
                context_compacted: false,
            });
            return Ok(());
        }
        sink.emit(ModelStreamEvent::BlockStart {
            index: 0,
            kind: ModelBlockKind::Text,
        });
        let text = if first {
            "Unfinished public reply"
        } else {
            "Revised answer"
        };
        sink.emit(ModelStreamEvent::TextDelta {
            index: 0,
            text: text.into(),
        });
        if first {
            self.started.notify_one();
            self.release.notified().await;
        }
        sink.emit(ModelStreamEvent::BlockEnd {
            index: 0,
            block: ContentBlock::Text { text: text.into() },
        });
        sink.emit(ModelStreamEvent::Finish {
            finish_reason: ModelFinishReason::Stop,
            provider_state: None,
            provider_context: None,
            context_compacted: false,
        });
        Ok(())
    }
}

struct Tools {
    definitions: Vec<ToolDefinition>,
    calls: AtomicUsize,
    started: Notify,
    release: Notify,
}
impl Tools {
    fn new() -> Self {
        Self {
            definitions: vec![ToolDefinition {
                name: "write".into(),
                description: "fixture".into(),
                input_schema: "{}".into(),
            }],
            calls: AtomicUsize::new(0),
            started: Notify::new(),
            release: Notify::new(),
        }
    }
}
impl ToolExecutor for Tools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Infallible> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        Ok("effect completed".into())
    }
}

#[tokio::test]
async fn steering_interrupts_only_the_incomplete_response_and_delivers_once() {
    let queue = Queue::new();
    let runtime = AgentRuntime::new(
        ModelFixture::new(false),
        Tools::new(),
        support::test_context_manager(),
        None,
    )
    .with_user_inputs(queue.clone());
    let agent = Agent::new("a", "a", "", "");
    let mut run = runtime
        .start_run(&agent, "steer", "original plan")
        .await
        .unwrap();
    let (result, _) = tokio::join!(runtime.run_until_blocked(&agent, &mut run), async {
        runtime.model().started.notified().await;
        queue.push(true);
    });
    assert_eq!(result.unwrap(), RunStatus::Completed);
    assert_eq!(runtime.model().calls.load(Ordering::SeqCst), 2);
    assert!(run.events().iter().any(|entry| matches!(&entry.event, RunEvent::ModelResponseInterrupted { content } if content == "Unfinished public reply")));
    assert_eq!(
        run.events()
            .iter()
            .filter(|entry| matches!(entry.event, RunEvent::UserInputReceived { .. }))
            .count(),
        1
    );
    assert!(
        !run.events()
            .iter()
            .any(|entry| matches!(entry.event, RunEvent::RunCancelled))
    );
    let requests = runtime.model().requests.lock().unwrap();
    let replay = serde_json::to_string(&requests[1]).unwrap();
    assert!(replay.contains("Use the revised plan"));
    assert!(replay.contains("incomplete historical output"));
    assert!(queue.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unselected_input_remains_staged_after_the_current_answer() {
    let queue = Queue::new();
    let runtime = AgentRuntime::new(
        ModelFixture::new(false),
        Tools::new(),
        support::test_context_manager(),
        None,
    )
    .with_user_inputs(queue.clone());
    let agent = Agent::new("a", "a", "", "");
    let mut run = runtime
        .start_run(&agent, "queue", "original")
        .await
        .unwrap();
    let (result, _) = tokio::join!(runtime.run_until_blocked(&agent, &mut run), async {
        runtime.model().started.notified().await;
        queue.push(false);
        tokio::task::yield_now().await;
        assert_eq!(runtime.model().calls.load(Ordering::SeqCst), 1);
        runtime.model().release.notify_one();
    });
    assert_eq!(result.unwrap(), RunStatus::Completed);
    assert!(
        !run.events()
            .iter()
            .any(|entry| matches!(entry.event, RunEvent::ModelResponseInterrupted { .. }))
    );
    assert_eq!(runtime.model().calls.load(Ordering::SeqCst), 1);
    assert!(
        !run.events()
            .iter()
            .any(|entry| matches!(entry.event, RunEvent::UserInputReceived { .. }))
    );
    assert_eq!(queue.inputs.lock().unwrap().len(), 1);
    assert!(!runtime.start_queued_turn(&mut run).await.unwrap());

    // Selecting a different message must not send the untouched draft with it.
    queue.inputs.lock().unwrap().push(PendingUserInput {
        id: "selected".into(),
        content: "Only send this correction".into(),
        files: vec![],
        steer: true,
        delivering: false,
        revision: 0,
    });
    assert!(runtime.start_queued_turn(&mut run).await.unwrap());
    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(runtime.model().calls.load(Ordering::SeqCst), 2);
    let replay = serde_json::to_string(&runtime.model().requests.lock().unwrap()[1]).unwrap();
    assert!(replay.contains("Only send this correction"));
    assert!(!replay.contains("Use the revised plan"));
    assert_eq!(queue.inputs.lock().unwrap()[0].id, "correction");
    assert_eq!(queue.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn steering_lets_the_inflight_tool_settle_and_skips_remaining_old_calls() {
    let queue = Queue::new();
    let runtime = AgentRuntime::new(
        ModelFixture::new(true),
        Tools::new(),
        support::test_context_manager(),
        None,
    )
    .with_user_inputs(queue.clone());
    let agent = Agent::new("a", "a", "", "");
    let mut run = runtime
        .start_run(&agent, "tool-steer", "original")
        .await
        .unwrap();
    let (result, _) = tokio::join!(runtime.run_until_blocked(&agent, &mut run), async {
        runtime.tools().started.notified().await;
        queue.push(true);
        tokio::task::yield_now().await;
        assert_eq!(runtime.model().calls.load(Ordering::SeqCst), 1);
        runtime.tools().release.notify_one();
    });
    assert_eq!(result.unwrap(), RunStatus::Completed);
    assert_eq!(runtime.tools().calls.load(Ordering::SeqCst), 1);
    assert!(run.events().iter().any(|entry| matches!(&entry.event, RunEvent::ToolCallSucceeded { call_id, .. } if call_id == "tool-0")));
    assert!(run.events().iter().any(|entry| matches!(&entry.event, RunEvent::UserInputReceived { interrupted_calls, .. } if interrupted_calls.len() == 1 && interrupted_calls[0].call_id == "tool-1")));
}

#[tokio::test]
async fn stop_takes_precedence_over_pending_steering_and_keeps_the_message() {
    let queue = Queue::new();
    let runtime = AgentRuntime::new(
        ModelFixture::new(false),
        Tools::new(),
        support::test_context_manager(),
        None,
    )
    .with_user_inputs(queue.clone());
    let agent = Agent::new("a", "a", "", "");
    let mut run = runtime
        .start_run(&agent, "stop-steer", "original")
        .await
        .unwrap();
    let mut handle = run.clone();
    let (result, cancelled) = tokio::join!(runtime.run_until_blocked(&agent, &mut run), async {
        runtime.model().started.notified().await;
        queue.push(true);
        runtime.cancel_run(&mut handle).await
    });
    cancelled.unwrap();
    assert_eq!(result.unwrap(), RunStatus::Cancelled);
    assert_eq!(runtime.model().calls.load(Ordering::SeqCst), 1);
    assert_eq!(queue.inputs.lock().unwrap().len(), 1);
    assert!(
        !run.events()
            .iter()
            .any(|entry| matches!(entry.event, RunEvent::UserInputReceived { .. }))
    );
    assert!(run.events().iter().any(|entry| matches!(&entry.event, RunEvent::ModelResponseInterrupted { content } if content == "Unfinished public reply")));
}
