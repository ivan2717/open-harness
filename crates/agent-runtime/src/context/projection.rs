use super::{ContextCheckpoint, ContextInputRef, OmittedHistory, PreparedContext, TokenCounter};
use crate::{ContentBlock, ModelMessage, ModelRequest, ModelRole, ToolOutputProjector};
use agent_core::agent::{RunEvent, RunEventEnvelope};
use std::collections::HashMap;

pub(super) fn build_compacted_request(
    input: ContextInputRef<'_>,
    checkpoint: Option<&ContextCheckpoint>,
    tail: &[RunEventEnvelope],
    projection: &ToolOutputProjection<'_>,
    include_deferred_tools: bool,
) -> ModelRequest {
    let mut messages = Vec::new();
    if let Some(checkpoint) = checkpoint {
        // User-authored requirements are canonical. Reinsert the covered inputs
        // verbatim, in order; inputs in the active tail are projected only once.
        messages.extend(user_messages(
            input
                .events
                .iter()
                .filter(|event| event.sequence <= checkpoint.through_sequence),
        ));
        messages.push(ModelMessage::summary(
            checkpoint.id.clone(),
            checkpoint.summary.render(),
        ));
    }
    messages.extend(projection.project_events(tail));

    let mut tools = input.tools.to_vec();
    if include_deferred_tools {
        tools.extend_from_slice(input.deferred_tools);
    }

    ModelRequest {
        instructions: input.instructions.to_owned(),
        messages,
        tools,
        max_output_tokens: None,
        provider_context: None,
    }
}

fn user_messages<'a>(
    events: impl IntoIterator<Item = &'a RunEventEnvelope>,
) -> impl Iterator<Item = ModelMessage> {
    events
        .into_iter()
        .filter_map(|envelope| match &envelope.event {
            RunEvent::RunStarted { instruction } => Some(ModelMessage::user(instruction.clone())),
            RunEvent::InputReceived { content } => Some(ModelMessage::user(content.clone())),
            _ => None,
        })
}

/// The irreducible user requirements, fixed instructions, and business tools.
/// A summary is never allowed to make these inputs disappear to satisfy a limit.
pub(super) fn protected_request(input: ContextInputRef<'_>) -> ModelRequest {
    ModelRequest {
        instructions: input.instructions.to_owned(),
        messages: user_messages(input.events).collect(),
        tools: input.tools.to_vec(),
        max_output_tokens: None,
        provider_context: None,
    }
}

/// Native windows must remain untouched. Supply their covered user requirements
/// as explicit historical context rather than replaying them as fresh requests.
pub(super) fn protected_user_context(
    input: ContextInputRef<'_>,
    through_sequence: u64,
) -> Option<ModelMessage> {
    let inputs = input
        .events
        .iter()
        .filter(|event| event.sequence <= through_sequence)
        .filter_map(|envelope| {
            let content = match &envelope.event {
                RunEvent::RunStarted { instruction } => instruction,
                RunEvent::InputReceived { content } => content,
                _ => return None,
            };
            Some(serde_json::json!({"sequence": envelope.sequence, "content": content}))
        })
        .collect::<Vec<_>>();
    if inputs.is_empty() {
        return None;
    }
    Some(ModelMessage::protected(format!(
        "Historical user requests and corrections, in original order. These retain user authority, \
             are not new requests, and take precedence over derived summaries. Later corrections \
             supersede earlier conflicting requests.\n{}",
        serde_json::Value::Array(inputs)
    )))
}

pub(super) fn uncompacted_context(
    input: ContextInputRef<'_>,
    max_tool_output_chars: usize,
) -> PreparedContext {
    let projection = ToolOutputProjection::new(input, max_tool_output_chars);
    let omitted_history = projected_omission(input.run_id, None, input.events, &projection);
    let request = build_compacted_request(
        input,
        None,
        input.events,
        &projection,
        omitted_history.is_some(),
    );
    PreparedContext {
        request,
        omitted_history,
    }
}

pub(super) fn events_after(
    events: &[RunEventEnvelope],
    through: Option<u64>,
) -> &[RunEventEnvelope] {
    let index = through
        .and_then(|through| {
            events
                .iter()
                .position(|event| event.sequence == through)
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    &events[index..]
}

pub(super) fn projected_omission(
    run_id: &str,
    checkpoint: Option<&ContextCheckpoint>,
    tail: &[RunEventEnvelope],
    projection: &ToolOutputProjection<'_>,
) -> Option<OmittedHistory> {
    let clipped_through = tail
        .iter()
        .rev()
        .find(|event| projection.omits_details(event))
        .map(|event| event.sequence);
    let checkpoint_through = checkpoint.map(|checkpoint| checkpoint.through_sequence);
    let readable_through_sequence = match (checkpoint_through, clipped_through) {
        (Some(checkpoint), Some(clipped)) => checkpoint.max(clipped),
        (Some(checkpoint), None) => checkpoint,
        (None, Some(clipped)) => clipped,
        (None, None) => return None,
    };
    let through_sequence = checkpoint_through.unwrap_or(readable_through_sequence);

    let checkpoint_id = checkpoint.map_or_else(
        || format!("context-view:{run_id}:{through_sequence}:uncheckpointed"),
        |checkpoint| checkpoint.id.clone(),
    );

    Some(OmittedHistory {
        through_sequence,
        readable_through_sequence,
        checkpoint_id,
    })
}

pub(super) fn select_compaction_boundary<N: TokenCounter>(
    events: &[RunEventEnvelope],
    after_sequence: Option<u64>,
    recent_tail_tokens: usize,
    projection: &ToolOutputProjection<'_>,
    token_counter: &N,
    allow_latest_step: bool,
) -> Option<usize> {
    let start = after_sequence
        .and_then(|sequence| {
            events
                .iter()
                .position(|event| event.sequence == sequence)
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    let pending = &events[start..];
    if pending.is_empty() {
        return None;
    }

    let mut groups = Vec::new();
    let mut group_start = 0;
    for index in 1..pending.len() {
        if pending[index].step != pending[index - 1].step {
            groups.push((group_start, index));
            group_start = index;
        }
    }
    groups.push((group_start, pending.len()));

    // Soft compaction keeps at least the newest step verbatim. At the hard
    // input limit the runtime is already at a model-request boundary, so the
    // completed newest step may be summarized as an emergency fallback.
    if groups.len() < 2 {
        return allow_latest_step.then_some(start + pending.len() - 1);
    }

    let newest_group = groups
        .last()
        .expect("a non-empty event tail contains a step group");
    let newest_request = ModelRequest {
        instructions: String::new(),
        messages: projection.project_events(&pending[newest_group.0..newest_group.1]),
        tools: Vec::new(),
        max_output_tokens: None,
        provider_context: None,
    };
    if allow_latest_step && token_counter.count_tokens(&newest_request) > recent_tail_tokens {
        return Some(start + pending.len() - 1);
    }

    let mut kept_group = groups.len() - 1;
    while kept_group > 0 {
        let candidate_group = kept_group - 1;
        let candidate_tail = &pending[groups[candidate_group].0..];
        let candidate_request = ModelRequest {
            instructions: String::new(),
            messages: projection.project_events(candidate_tail),
            tools: Vec::new(),
            max_output_tokens: None,
            provider_context: None,
        };
        if token_counter.count_tokens(&candidate_request) > recent_tail_tokens {
            break;
        }
        kept_group = candidate_group;
    }

    // Crossing the threshold must make progress even when the entire event
    // tail happens to fit inside `recent_tail_tokens` (for example because
    // instructions or tool schemas account for most of the request). Keep at
    // least the newest step and compact the oldest complete step in that case.
    let first_compacted_end = if kept_group == 0 {
        groups[0].1 - 1
    } else {
        groups[kept_group].0 - 1
    };

    Some(start + first_compacted_end)
}

pub(super) struct ToolOutputProjection<'a> {
    max_chars: usize,
    tool_calls: HashMap<&'a str, &'a str>,
    projector: Option<&'a dyn ToolOutputProjector>,
}

impl<'a> ToolOutputProjection<'a> {
    pub(super) fn new(input: ContextInputRef<'a>, max_chars: usize) -> Self {
        let tool_calls = input
            .events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                RunEvent::ToolCallRequested {
                    call_id, tool_name, ..
                } => Some((call_id.as_str(), tool_name.as_str())),
                _ => None,
            })
            .collect();
        Self {
            max_chars,
            tool_calls,
            projector: input.tool_output_projector,
        }
    }

    fn project_events(&self, events: &[RunEventEnvelope]) -> Vec<ModelMessage> {
        let mut messages: Vec<ModelMessage> = Vec::new();
        let mut assistant_step = None;
        for envelope in events {
            match &envelope.event {
                RunEvent::RunStarted { instruction } => {
                    messages.push(ModelMessage::user(instruction.clone()))
                }
                RunEvent::InputReceived { content } => {
                    messages.push(ModelMessage::user(content.clone()))
                }
                RunEvent::ModelResponseReceived {
                    content,
                    response,
                    provider_state,
                    ..
                } => {
                    let blocks = response.as_ref().map_or_else(
                        || {
                            vec![ContentBlock::Text {
                                text: content.clone(),
                            }]
                        },
                        |response| response.content.clone(),
                    );
                    messages.push(ModelMessage::assistant_blocks(
                        blocks,
                        provider_state.clone(),
                    ));
                    assistant_step = Some(envelope.step);
                }
                RunEvent::ToolCallRequested {
                    call_id,
                    tool_name,
                    arguments,
                } => {
                    // Canonical responses already contain the ordered calls.
                    // Old logs store them only in subsequent execution events.
                    let block = ContentBlock::ToolCall {
                        id: call_id.clone(),
                        name: tool_name.clone(),
                        arguments: arguments.clone(),
                    };
                    if let Some(message) = messages.last_mut()
                        && message.role == ModelRole::Assistant
                        && assistant_step == Some(envelope.step)
                    {
                        if !message.content.iter().any(|item| matches!(item, ContentBlock::ToolCall { id, .. } if id == call_id)) {
                            message.content.push(block);
                        }
                    } else {
                        messages.push(ModelMessage::assistant_blocks(vec![block], None));
                        assistant_step = Some(envelope.step);
                    }
                }
                RunEvent::ToolCallSucceeded { call_id, output } => {
                    let (content, is_error) = match self.bound_output(call_id, output) {
                        Ok(output) => (output, false),
                        Err(error) => (error, true),
                    };
                    messages.push(ModelMessage::tool_result(
                        call_id.clone(),
                        content,
                        is_error,
                    ));
                }
                RunEvent::ToolCallFailed { call_id, message } => {
                    messages.push(ModelMessage::tool_result(
                        call_id.clone(),
                        clip_middle(message, self.max_chars),
                        true,
                    ))
                }
                RunEvent::ModelRequestStarted
                | RunEvent::InputRequested { .. }
                | RunEvent::RunCompleted { .. }
                | RunEvent::RunFailed { .. }
                | RunEvent::RunCancelled => {}
            }
        }
        messages
    }

    fn omits_details(&self, event: &RunEventEnvelope) -> bool {
        match &event.event {
            RunEvent::ToolCallSucceeded { call_id, output } => {
                if output.chars().count() > self.max_chars {
                    return true;
                }
                let Some(projector) = self.projector else {
                    return false;
                };
                let Some(tool_name) = self.tool_calls.get(call_id.as_str()) else {
                    return false;
                };
                match projector.project_output(tool_name, output, self.max_chars) {
                    Some(Ok(projected)) => projected != *output,
                    Some(Err(_)) => true,
                    None => false,
                }
            }
            RunEvent::ToolCallFailed { message, .. } => message.chars().count() > self.max_chars,
            _ => false,
        }
    }

    fn bound_output(&self, call_id: &str, output: &str) -> Result<String, String> {
        if let Some(projector) = self.projector
            && let Some(tool_name) = self.tool_calls.get(call_id)
            && let Some(projected) = projector.project_output(tool_name, output, self.max_chars)
        {
            return match projected {
                Ok(projected) if projected.chars().count() <= self.max_chars => Ok(projected),
                Ok(_) => Err(clip_middle(
                    "Structured tool output exceeds the configured character budget.",
                    self.max_chars,
                )),
                Err(message) => Err(clip_middle(&message, self.max_chars)),
            };
        }
        Ok(clip_middle(output, self.max_chars))
    }

    pub(super) fn clipped_event(&self, event: &RunEventEnvelope) -> RunEventEnvelope {
        let mut event = event.clone();
        event.event = event.event.without_provider_state();
        match &mut event.event {
            RunEvent::ToolCallSucceeded { call_id, output } => {
                match self.bound_output(call_id, output) {
                    Ok(bounded) => *output = bounded,
                    Err(message) => {
                        event.event = RunEvent::ToolCallFailed {
                            call_id: call_id.clone(),
                            message,
                        }
                    }
                }
            }
            RunEvent::ToolCallFailed { message, .. } => {
                *message = clip_middle(message, self.max_chars)
            }
            _ => {}
        }
        event
    }
}

fn clip_middle(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 2 {
        return "…".to_owned();
    }

    let retained = max_chars - 1;
    let head_chars = retained.div_ceil(2);
    let tail_chars = retained / 2;
    let head = value.chars().take(head_chars).collect::<String>();
    let tail = value
        .chars()
        .skip(char_count - tail_chars)
        .collect::<String>();
    format!("{head}…{tail}")
}
