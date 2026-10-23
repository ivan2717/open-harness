use agent_runtime::ContentBlock;
#[path = "support/scripted_model.rs"]
mod scripted_model;

use std::{convert::Infallible, io, sync::Mutex};

use agent_core::agent::{Agent, RunEvent, RunStatus};
use agent_runtime::{
    AgentRuntime, ContextInputRef, ContextManager, EventStore, HistoryAccess,
    HistoryConfigurationError, HistoryContext, HistoryLimits, InMemoryEventStore, ModelMessage,
    ModelRequest, ModelResponse, OmittedHistory, PreparedContext, ToggleHistoryAccess,
    ToggleHistoryAccessError, ToolCall, ToolDefinition, ToolExecutor, ToolOutputProjector,
};
use scripted_model::ScriptedModel;
use serde::{Deserialize, Serialize};

/// A custom name and wire format exercise the public contract independently of
/// the built-in search/read tools.
struct ArchiveAccess {
    definitions: [ToolDefinition; 1],
    limits_seen: Mutex<Vec<Option<usize>>>,
}

impl ArchiveAccess {
    fn new() -> Self {
        Self {
            definitions: [ToolDefinition {
                name: "archive_page".into(),
                description: "Read archived text".into(),
                input_schema: "{}".into(),
            }],
            limits_seen: Mutex::new(Vec::new()),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ArchivePage {
    start: usize,
    text: String,
    next: Option<usize>,
}

impl ArchivePage {
    fn encode_bounded(mut self, max_chars: usize) -> Result<String, String> {
        loop {
            let encoded = serde_json::to_string(&self).unwrap();
            if encoded.chars().count() <= max_chars {
                return Ok(encoded);
            }
            if self.text.pop().is_none() {
                return Err("page does not fit".chars().take(max_chars).collect());
            }
            self.next = Some(self.start + self.text.chars().count());
        }
    }
}

impl ToolOutputProjector for ArchiveAccess {
    fn project_output(
        &self,
        tool_name: &str,
        output: &str,
        max_chars: usize,
    ) -> Option<Result<String, String>> {
        if tool_name != "archive_page" {
            return None;
        }
        Some(match serde_json::from_str::<ArchivePage>(output) {
            Ok(page) => page.encode_bounded(max_chars),
            Err(_) => Err("invalid archive page".chars().take(max_chars).collect()),
        })
    }
}

impl HistoryAccess<InMemoryEventStore> for ArchiveAccess {
    type Error = io::Error;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    fn owns_tool(&self, name: &str) -> bool {
        name == "archive_page"
    }

    async fn execute_bounded(
        &self,
        _event_store: &InMemoryEventStore,
        _context: HistoryContext,
        call: ToolCall,
        max_output_chars: Option<usize>,
    ) -> Result<String, Self::Error> {
        assert!(self.owns_tool(&call.name));
        self.limits_seen.lock().unwrap().push(max_output_chars);
        ArchivePage {
            start: 7,
            text: "你好🦀".repeat(80),
            next: None,
        }
        .encode_bounded(max_output_chars.unwrap_or(1_000))
        .map_err(io::Error::other)
    }
}

fn context() -> HistoryContext {
    HistoryContext {
        run_id: "run".into(),
        through_sequence: 10,
        checkpoint_id: "checkpoint".into(),
    }
}

fn call() -> ToolCall {
    ToolCall {
        id: "archive-call".into(),
        name: "archive_page".into(),
        arguments: "{}".into(),
    }
}

#[tokio::test]
async fn custom_history_receives_budget_and_reprojects_its_own_cursor_format() {
    let access = ArchiveAccess::new();
    let store = InMemoryEventStore::new();
    let output = access
        .execute_bounded(&store, context(), call(), Some(100))
        .await
        .unwrap();
    assert!(output.chars().count() <= 100);

    let page: ArchivePage = serde_json::from_str(&output).unwrap();
    assert_eq!(page.next, Some(page.start + page.text.chars().count()));

    let projected = access
        .project_output("archive_page", &output, 60)
        .unwrap()
        .unwrap();
    assert!(projected.chars().count() <= 60);
    let smaller_page: ArchivePage = serde_json::from_str(&projected).unwrap();
    assert!(page.text.starts_with(&smaller_page.text));
    assert_eq!(smaller_page.start, 7);
    assert_eq!(
        smaller_page.next,
        Some(smaller_page.start + smaller_page.text.chars().count())
    );

    access.execute(&store, context(), call()).await.unwrap();
    assert_eq!(*access.limits_seen.lock().unwrap(), [Some(100), None]);
    assert_eq!(
        access.project_output("ordinary_tool", "plain text", 5),
        None
    );
}

#[tokio::test]
async fn disabled_history_still_projects_saved_custom_responses() {
    let access = ToggleHistoryAccess::disabled(ArchiveAccess::new());
    let saved = serde_json::to_string(&ArchivePage {
        start: 7,
        text: "你好🦀".repeat(80),
        next: None,
    })
    .unwrap();
    let projected = access
        .project_output("archive_page", &saved, 60)
        .unwrap()
        .unwrap();
    assert!(projected.chars().count() <= 60);
    assert!(serde_json::from_str::<ArchivePage>(&projected).is_ok());
    assert!(matches!(
        access
            .execute_bounded(&InMemoryEventStore::new(), context(), call(), Some(60))
            .await,
        Err(ToggleHistoryAccessError::Disabled)
    ));
}

#[test]
fn named_history_limits_reject_invalid_units_and_preserve_defaults() {
    assert_eq!(
        HistoryLimits::builder().build().unwrap(),
        HistoryLimits::default()
    );
    assert_eq!(
        HistoryLimits::builder().max_events(0).build(),
        Err(HistoryConfigurationError::ZeroLimit { name: "max_events" })
    );
    assert_eq!(
        HistoryLimits::builder()
            .max_output_chars(30)
            .preview_chars(31)
            .build(),
        Err(HistoryConfigurationError::PreviewExceedsOutput {
            preview_chars: 31,
            max_output_chars: 30,
        })
    );
    let limits = HistoryLimits::builder()
        .max_results(3)
        .max_events(4)
        .max_output_chars(80)
        .preview_chars(20)
        .build()
        .unwrap();
    assert_eq!(limits.max_results(), 3);
    assert_eq!(limits.max_events(), 4);
    assert_eq!(limits.max_output_chars(), 80);
    assert_eq!(limits.preview_chars(), 20);
}

/// Deliberately violates the execution budget to test the runtime boundary.
struct BudgetIgnoringArchiveAccess(ArchiveAccess);

impl ToolOutputProjector for BudgetIgnoringArchiveAccess {
    fn project_output(
        &self,
        tool_name: &str,
        output: &str,
        max_chars: usize,
    ) -> Option<Result<String, String>> {
        self.0.project_output(tool_name, output, max_chars)
    }
}

impl HistoryAccess<InMemoryEventStore> for BudgetIgnoringArchiveAccess {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.0.definitions
    }

    fn owns_tool(&self, name: &str) -> bool {
        name == "archive_page"
    }

    async fn execute_bounded(
        &self,
        _event_store: &InMemoryEventStore,
        _context: HistoryContext,
        _call: ToolCall,
        max_output_chars: Option<usize>,
    ) -> Result<String, Self::Error> {
        self.0.limits_seen.lock().unwrap().push(max_output_chars);
        Ok(serde_json::to_string(&ArchivePage {
            start: 7,
            text: "oversized historical content ".repeat(100),
            next: None,
        })
        .unwrap())
    }
}

/// Exposes tool results verbatim so context clipping cannot hide a runtime
/// enforcement failure. Other history is intentionally omitted.
struct ToolResultsContext;

impl ContextManager for ToolResultsContext {
    type Error = Infallible;

    fn max_tool_output_chars(&self) -> Option<usize> {
        Some(100)
    }

    async fn prepare(&self, input: ContextInputRef<'_>) -> Result<PreparedContext, Self::Error> {
        let messages =
            input
                .events
                .iter()
                .filter_map(|envelope| match &envelope.event {
                    RunEvent::ToolCallSucceeded {
                        call_id, output, ..
                    } => Some(ModelMessage::tool_result(
                        call_id.clone(),
                        output.clone(),
                        false,
                    )),
                    RunEvent::ToolCallFailed { call_id, message } => Some(
                        ModelMessage::tool_result(call_id.clone(), message.clone(), true),
                    ),
                    _ => None,
                })
                .collect();
        Ok(PreparedContext {
            request: ModelRequest {
                instructions: input.instructions.into(),
                messages,
                tools: input.deferred_tools.to_vec(),
                max_output_tokens: None,
                provider_context: None,
            },
            omitted_history: Some(OmittedHistory {
                through_sequence: 0,
                readable_through_sequence: 0,
                checkpoint_id: "test-checkpoint".into(),
            }),
        })
    }
}

struct NoTools;

impl ToolExecutor for NoTools {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }

    async fn execute(&self, _call: ToolCall) -> Result<String, Self::Error> {
        unreachable!("the model only requests the custom history tool")
    }
}

#[tokio::test]
async fn runtime_rejects_oversized_history_response_before_persistence_or_model_context() {
    let model = ScriptedModel::new(vec![
        ModelResponse::tool_calls("Read the archive".into(), vec![call()]),
        ModelResponse::final_answer("The history provider failed".into()),
    ]);
    let runtime = AgentRuntime::new(model, NoTools, ToolResultsContext, Some(3))
        .with_history_access(BudgetIgnoringArchiveAccess(ArchiveAccess::new()));
    let agent = Agent::new("agent", "Archive agent", "", "Read archived text");
    let mut run = runtime
        .start_run(&agent, "oversized-history", "Find prior details")
        .await
        .unwrap();

    assert_eq!(
        runtime.run_until_blocked(&agent, &mut run).await.unwrap(),
        RunStatus::Completed
    );
    assert_eq!(
        *runtime.history_access().0.limits_seen.lock().unwrap(),
        [Some(100)]
    );

    let stored = runtime.event_store().load(run.id()).await.unwrap().unwrap();
    assert!(!stored.events.iter().any(|envelope| matches!(
        &envelope.event,
        RunEvent::ToolCallSucceeded { call_id, .. } if call_id == "archive-call"
    )));
    assert!(stored.events.iter().any(|envelope| matches!(
        &envelope.event,
        RunEvent::ToolCallFailed { call_id, message }
            if call_id == "archive-call" && message.contains("exceeded")
    )));

    let requests = runtime.model().requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "archive_page")
    );
    assert_eq!(requests[1].messages.len(), 1);
    assert!(matches!(requests[1].messages[0].content.as_slice(),
        [ContentBlock::ToolResult { call_id, content, is_error: true , .. }]
            if call_id == "archive-call" && content.contains("exceeded")));
}
