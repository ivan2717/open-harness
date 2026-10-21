use agent_runtime::{ContentBlock, MessageContext, ModelRole};
use std::{convert::Infallible, sync::Mutex};

use agent_core::agent::{AgentRun, RunEvent, RunEventEnvelope};
use agent_runtime::{
    ApproximateTokenCounter, CompactingContextManager, ContextCompactor, ContextInput,
    ContextManager, ContextPolicy, ContextSummary, InMemoryContextCheckpointStore, ModelMessage,
    TokenCounter, ToolDefinition, ToolOutputProjector,
};

#[derive(Default)]
struct LocalCompactor {
    calls: Mutex<Vec<Vec<RunEventEnvelope>>>,
}

impl ContextCompactor for LocalCompactor {
    type Error = Infallible;

    fn estimate_input_tokens(
        &self,
        _previous_summary: Option<&ContextSummary>,
        _events: &[RunEventEnvelope],
        _token_counter: &dyn TokenCounter,
    ) -> Result<usize, Self::Error> {
        // This adapter performs local extraction and sends no model request.
        Ok(0)
    }

    async fn compact(
        &self,
        _previous_summary: Option<ContextSummary>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<ContextSummary, Self::Error> {
        self.calls.lock().unwrap().push(events);
        Ok(ContextSummary {
            objective: "Continue".to_owned(),
            ..ContextSummary::default()
        })
    }
}

fn manager(
    input_token_limit: usize,
) -> CompactingContextManager<LocalCompactor, InMemoryContextCheckpointStore, ApproximateTokenCounter>
{
    CompactingContextManager::new(
        ContextPolicy::builder()
            .input_token_limit(input_token_limit)
            .compaction_threshold(input_token_limit - 1)
            .recent_tail_tokens(1)
            .max_tool_output_chars(128)
            .build()
            .unwrap(),
        LocalCompactor::default(),
        InMemoryContextCheckpointStore::new(),
        ApproximateTokenCounter::new(1).unwrap(),
    )
}

fn input(run: &AgentRun) -> ContextInput {
    ContextInput::new(run.id(), "", run.events().to_vec(), Vec::new()).with_deferred_tools(vec![
        ToolDefinition {
            name: "archive_page".to_owned(),
            description: "Recover omitted details".to_owned(),
            input_schema: "{}".to_owned(),
        },
    ])
}

fn run_with_structured_output(instruction: &str) -> AgentRun {
    let mut run = AgentRun::new("projection", "context-agent", instruction);
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        provider_state: None,
        content: "Read the archive".repeat(120),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: "archive-call".to_owned(),
        tool_name: "archive_page".to_owned(),
        arguments: "{}".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "archive-call".to_owned(),
        output: r#"{"text":"秘密档案"}"#.to_owned(),
    })
    .unwrap();
    run
}

struct ArchiveProjector;

impl ToolOutputProjector for ArchiveProjector {
    fn project_output(
        &self,
        tool_name: &str,
        output: &str,
        max_chars: usize,
    ) -> Option<Result<String, String>> {
        if tool_name != "archive_page" {
            return None;
        }
        Some((|| {
            let value: serde_json::Value =
                serde_json::from_str(output).map_err(|e| e.to_string())?;
            let text = value["text"].as_str().ok_or("missing text")?;
            let prefix = text.chars().take(3).collect::<String>();
            let projected = serde_json::json!({
                "text": prefix,
                "next_offset": 3,
            })
            .to_string();
            if projected.chars().count() > max_chars {
                return Err("The structured envelope does not fit".to_owned());
            }
            Ok(projected)
        })())
    }
}

#[tokio::test]
async fn compaction_uses_the_adapters_input_cost_instead_of_assuming_a_model_payload() {
    let mut run = AgentRun::new("local-compaction", "context-agent", "Continue");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        provider_state: None,
        content: "x".repeat(2_000),
    })
    .unwrap();
    let manager = manager(200);

    let prepared = manager.prepare_owned(input(&run)).await.unwrap();

    assert!(matches!(
        prepared.request.messages.as_slice(),
        [
            ModelMessage {
                role: ModelRole::User,
                context: None,
                ..
            },
            ModelMessage {
                context: Some(MessageContext::Summary { .. }),
                ..
            }
        ]
    ));
    assert_eq!(manager.compactor().calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn custom_tool_protocol_is_projected_and_exposes_omitted_details_below_the_size_limit() {
    let run = run_with_structured_output("Read");
    let input = input(&run);
    let prepared = manager(10_000)
        .prepare(input.as_ref().with_tool_output_projector(&ArchiveProjector))
        .await
        .unwrap();

    let output = prepared
        .request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                content: output,
                is_error: false,
                ..
            } => Some(output),
            _ => None,
        })
        .unwrap();
    let decoded: serde_json::Value = serde_json::from_str(output).unwrap();
    assert_eq!(decoded["text"], "秘密档");
    assert_eq!(decoded["next_offset"], 3);
    assert_eq!(prepared.request.tools[0].name, "archive_page");
    assert_eq!(
        prepared.omitted_history.unwrap().readable_through_sequence,
        run.events().last().unwrap().sequence
    );
}

#[tokio::test]
async fn custom_tool_protocol_is_also_preserved_in_compaction_input() {
    let run = run_with_structured_output("Read");
    let input = input(&run);
    let manager = manager(200);

    manager
        .prepare(input.as_ref().with_tool_output_projector(&ArchiveProjector))
        .await
        .unwrap();

    let calls = manager.compactor().calls.lock().unwrap();
    let output = calls
        .iter()
        .flatten()
        .find_map(|envelope| match &envelope.event {
            RunEvent::ToolCallSucceeded { output, .. } => Some(output),
            _ => None,
        })
        .unwrap();
    let decoded: serde_json::Value = serde_json::from_str(output).unwrap();
    assert_eq!(decoded["text"], "秘密档");
    assert_eq!(decoded["next_offset"], 3);
}

struct InvalidProjector {
    response: Result<String, String>,
}

impl ToolOutputProjector for InvalidProjector {
    fn project_output(
        &self,
        _tool_name: &str,
        _output: &str,
        _max_chars: usize,
    ) -> Option<Result<String, String>> {
        Some(self.response.clone())
    }
}

#[tokio::test]
async fn projector_budget_violations_become_bounded_errors_and_recoverable_omissions() {
    let run = run_with_structured_output("Read");
    let input = input(&run);
    for response in [Ok("界".repeat(129)), Err("界".repeat(129))] {
        let projector = InvalidProjector { response };
        let prepared = manager(10_000)
            .prepare(input.as_ref().with_tool_output_projector(&projector))
            .await
            .unwrap();
        let message = prepared
            .request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .find_map(|block| match block {
                ContentBlock::ToolResult {
                    content: message,
                    is_error: true,
                    ..
                } => Some(message),
                _ => None,
            })
            .expect("an invalid structured projection must not reach the model as a result");
        assert!(message.chars().count() <= 128);
        assert!(prepared.omitted_history.is_some());
        assert_eq!(prepared.request.tools[0].name, "archive_page");
    }
}
