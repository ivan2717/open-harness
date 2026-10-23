use super::{
    ContextCheckpoint, ContextInputRef, ContextPruning, OmittedHistory, PreparedContext,
    PrunedToolOutput, TokenCounter,
};
use crate::{ContentBlock, ModelMessage, ModelRequest, ModelRole, ToolOutputProjector};
use agent_core::agent::{RunEvent, RunEventEnvelope};
use std::collections::HashMap;

pub(super) fn build_compacted_request(
    input: ContextInputRef<'_>,
    checkpoint: Option<&ContextCheckpoint>,
    tail: &[RunEventEnvelope],
    projection: &ToolOutputProjection<'_>,
    include_deferred_tools: bool,
    preserve_all_users: bool,
) -> ModelRequest {
    let mut messages = input.derived_messages.to_vec();
    if let Some(checkpoint) = checkpoint.filter(|checkpoint| !checkpoint.summary.is_empty()) {
        messages.extend(user_messages(
            protected_user_events(input, preserve_all_users)
                .into_iter()
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
        .filter_map(|envelope| envelope.event.user_input().map(ModelMessage::user))
}

fn protected_user_events(input: ContextInputRef<'_>, preserve_all: bool) -> Vec<&RunEventEnvelope> {
    let mut users = input.events.iter().filter(|event| {
        matches!(
            event.event,
            RunEvent::RunStarted { .. }
                | RunEvent::InputReceived { .. }
                | RunEvent::RunResumed { .. }
        )
    });
    if preserve_all {
        users.collect()
    } else {
        users.next_back().into_iter().collect()
    }
}

/// Derived context, the latest user input (or all inputs when opted in), fixed
/// instructions, and tools. Compaction cannot discard them to satisfy a limit.
pub(super) fn protected_request(
    input: ContextInputRef<'_>,
    preserve_all_users: bool,
) -> ModelRequest {
    ModelRequest {
        instructions: input.instructions.to_owned(),
        messages: input
            .derived_messages
            .iter()
            .cloned()
            .chain(user_messages(protected_user_events(
                input,
                preserve_all_users,
            )))
            .collect(),
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
    preserve_all_users: bool,
) -> Option<ModelMessage> {
    let inputs = protected_user_events(input, preserve_all_users)
        .into_iter()
        .filter(|event| event.sequence <= through_sequence)
        .filter_map(|envelope| {
            let content = envelope.event.user_input()?;
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

pub(super) fn uncompacted_context(input: ContextInputRef<'_>) -> PreparedContext {
    let projection = ToolOutputProjection::restored(input, None);
    let omitted_history = projected_omission(input.run_id, None, input.events, &projection);
    let request = build_compacted_request(
        input,
        None,
        input.events,
        &projection,
        omitted_history.is_some(),
        false,
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
    let checkpoint_through = checkpoint.and_then(ContextCheckpoint::summary_through_sequence);
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
    prune_new: bool,
    replacements: HashMap<u64, PrunedToolOutput>,
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
            prune_new: true,
            replacements: HashMap::new(),
            tool_calls,
            projector: input.tool_output_projector,
        }
    }

    pub(super) fn restored(input: ContextInputRef<'a>, pruning: Option<&ContextPruning>) -> Self {
        let mut projection = Self::new(input, usize::MAX);
        projection.prune_new = false;
        projection.replacements = pruning
            .into_iter()
            .flat_map(|pruning| &pruning.replacements)
            .map(|replacement| (replacement.sequence, replacement.clone()))
            .collect();
        projection
    }

    pub(super) fn enable_pruning(&mut self, max_chars: usize) {
        self.max_chars = max_chars;
        self.prune_new = true;
    }

    pub(super) fn replacements(&self, events: &[RunEventEnvelope]) -> Vec<PrunedToolOutput> {
        events
            .iter()
            .filter(|event| self.omits_details(event))
            .filter_map(|event| {
                let (content, is_error) = self.output(event)?;
                Some(PrunedToolOutput {
                    sequence: event.sequence,
                    content,
                    is_error,
                })
            })
            .collect()
    }

    fn output(&self, event: &RunEventEnvelope) -> Option<(String, bool)> {
        if let Some(replacement) = self.replacements.get(&event.sequence) {
            return Some((replacement.content.clone(), replacement.is_error));
        }
        match &event.event {
            RunEvent::ToolCallSucceeded { call_id, output } => {
                Some(match self.bound_output(call_id, output) {
                    Ok(output) => (output, false),
                    Err(error) => (error, true),
                })
            }
            RunEvent::ToolCallFailed { message, .. } => {
                Some((clip_middle(message, self.max_chars), true))
            }
            _ => None,
        }
    }

    fn project_events(&self, events: &[RunEventEnvelope]) -> Vec<ModelMessage> {
        let mut messages: Vec<ModelMessage> = Vec::new();
        let mut assistant_step = None;
        for envelope in events {
            match &envelope.event {
                RunEvent::RunStarted { .. } | RunEvent::InputReceived { .. } => messages.push(
                    ModelMessage::user(envelope.event.user_input().expect("user event")),
                ),
                RunEvent::RunResumed {
                    interrupted_calls, ..
                } => {
                    // Native windows may already contain the assistant calls;
                    // their missing results must still appear in this tail.
                    for call in interrupted_calls {
                        messages.push(ModelMessage::tool_result(
                            call.call_id.clone(),
                            call.outcome.explanation().into(),
                            true,
                        ));
                    }
                    messages.push(ModelMessage::user(
                        envelope.event.user_input().expect("user event"),
                    ));
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
                RunEvent::ToolCallSucceeded { call_id, .. } => {
                    let (content, is_error) = self.output(envelope).expect("tool result");
                    messages.push(ModelMessage::tool_result(
                        call_id.clone(),
                        content,
                        is_error,
                    ));
                }
                RunEvent::ToolCallFailed { call_id, .. } => {
                    messages.push(ModelMessage::tool_result(
                        call_id.clone(),
                        self.output(envelope).expect("tool failure").0,
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
        if self.replacements.contains_key(&event.sequence) {
            return true;
        }
        if !self.prune_new {
            return false;
        }
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
        if !self.prune_new {
            return Ok(output.to_owned());
        }
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
        let output = self.output(event);
        let mut event = event.clone();
        event.event = event.event.without_provider_state();
        if let Some((content, is_error)) = output {
            let call_id = match &event.event {
                RunEvent::ToolCallSucceeded { call_id, .. }
                | RunEvent::ToolCallFailed { call_id, .. } => call_id.clone(),
                _ => unreachable!(),
            };
            event.event = if is_error {
                RunEvent::ToolCallFailed {
                    call_id,
                    message: content,
                }
            } else {
                RunEvent::ToolCallSucceeded {
                    call_id,
                    output: content,
                }
            };
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
