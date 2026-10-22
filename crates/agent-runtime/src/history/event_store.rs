use std::{borrow::Cow, collections::HashSet, error::Error, fmt};

use agent_core::agent::{RunEvent, RunEventEnvelope};

use crate::{
    storage::{EventStore, EventStoreError},
    tool::{ToolCall, ToolDefinition, ToolOutputProjector},
};

use super::{
    HistoryAccess, HistoryContext, HistoryLimits,
    protocol::{
        HISTORY_WARNING, HistoryReadEvent, HistoryReadResponse, HistorySearchHit,
        READ_RUN_HISTORY_TOOL, ReadArguments, SEARCH_RUN_HISTORY_TOOL, SearchArguments,
        bound_saved_history_output, encode_read_response, encode_search_response, event_type,
        history_tool_definitions, is_history_tool, truncate_chars,
    },
};

#[derive(Debug, Clone)]
pub struct EventStoreHistoryAccess {
    limits: HistoryLimits,
    definitions: [ToolDefinition; 2],
}

impl EventStoreHistoryAccess {
    pub fn new() -> Self {
        Self::with_limits(HistoryLimits::default())
    }

    pub fn with_limits(limits: HistoryLimits) -> Self {
        Self {
            limits,
            definitions: history_tool_definitions(),
        }
    }

    pub fn limits(&self) -> HistoryLimits {
        self.limits
    }
}

impl Default for EventStoreHistoryAccess {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: EventStore> HistoryAccess<S> for EventStoreHistoryAccess {
    type Error = HistoryAccessError;

    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    fn owns_tool(&self, name: &str) -> bool {
        is_history_tool(name)
    }

    async fn execute_bounded(
        &self,
        event_store: &S,
        context: HistoryContext,
        call: ToolCall,
        max_output_chars: Option<usize>,
    ) -> Result<String, Self::Error> {
        let max_output_chars = max_output_chars
            .unwrap_or(self.limits.max_output_chars())
            .min(self.limits.max_output_chars());
        match call.name.as_str() {
            SEARCH_RUN_HISTORY_TOOL => {
                self.search(event_store, &context, &call.arguments, max_output_chars)
                    .await
            }
            READ_RUN_HISTORY_TOOL => {
                self.read(event_store, &context, &call.arguments, max_output_chars)
                    .await
            }
            _ => Err(HistoryAccessError::UnsupportedTool { name: call.name }),
        }
    }
}

impl EventStoreHistoryAccess {
    async fn search<S: EventStore>(
        &self,
        event_store: &S,
        context: &HistoryContext,
        arguments: &str,
        max_output_chars: usize,
    ) -> Result<String, HistoryAccessError> {
        let arguments: SearchArguments = serde_json::from_str(arguments).map_err(|source| {
            HistoryAccessError::InvalidArguments {
                tool: SEARCH_RUN_HISTORY_TOOL,
                source,
            }
        })?;

        if arguments.query.trim().is_empty() {
            return Err(HistoryAccessError::EmptyQuery);
        }

        let limit = arguments.limit.unwrap_or(self.limits.max_results());
        if limit == 0 {
            return Err(HistoryAccessError::ZeroRequestedLimit);
        }
        if limit > self.limits.max_results() {
            return Err(HistoryAccessError::RequestedLimitExceeded {
                requested: limit,
                configured: self.limits.max_results(),
            });
        }

        let stored = load_history_prefix(event_store, context)
            .await
            .map_err(HistoryAccessError::EventStore)?
            .ok_or_else(|| HistoryAccessError::RunNotFound {
                run_id: context.run_id.clone(),
            })?;
        let visible_events = visible_history_events(&stored.events, context.through_sequence, &[]);
        let folded_query = arguments.query.to_lowercase();
        let mut hits = Vec::with_capacity(limit);
        let mut total_matches = 0_usize;

        for envelope in visible_events {
            if !event_matches_query(&envelope.event, &folded_query) {
                continue;
            }

            total_matches = total_matches.saturating_add(1);
            if hits.len() < limit {
                let serialized_event =
                    serde_json::to_string(&envelope.event.without_provider_state())
                        .map_err(HistoryAccessError::Serialization)?;
                hits.push(HistorySearchHit {
                    sequence: envelope.sequence,
                    step: envelope.step,
                    event_type: Cow::Borrowed(event_type(&envelope.event)),
                    preview: truncate_chars(&serialized_event, self.limits.preview_chars()),
                });
            }
        }

        encode_search_response(
            &context.checkpoint_id,
            context.through_sequence,
            total_matches,
            hits,
            max_output_chars,
        )
    }

    async fn read<S: EventStore>(
        &self,
        event_store: &S,
        context: &HistoryContext,
        arguments: &str,
        max_output_chars: usize,
    ) -> Result<String, HistoryAccessError> {
        let arguments: ReadArguments = serde_json::from_str(arguments).map_err(|source| {
            HistoryAccessError::InvalidArguments {
                tool: READ_RUN_HISTORY_TOOL,
                source,
            }
        })?;
        let to_sequence = arguments.to_sequence.unwrap_or(arguments.from_sequence);

        if to_sequence < arguments.from_sequence {
            return Err(HistoryAccessError::InvalidRange {
                from_sequence: arguments.from_sequence,
                to_sequence,
            });
        }
        if to_sequence > context.through_sequence {
            return Err(HistoryAccessError::RangePastCheckpoint {
                to_sequence,
                through_sequence: context.through_sequence,
            });
        }

        let requested_events = to_sequence
            .checked_sub(arguments.from_sequence)
            .and_then(|difference| difference.checked_add(1))
            .ok_or(HistoryAccessError::RangeSizeOverflow {
                from_sequence: arguments.from_sequence,
                to_sequence,
            })?;
        let configured_max = u64::try_from(self.limits.max_events()).unwrap_or(u64::MAX);
        if requested_events > configured_max {
            return Err(HistoryAccessError::TooManyEvents {
                requested: requested_events,
                configured: self.limits.max_events(),
            });
        }

        let stored = match to_sequence.checked_add(1) {
            Some(end) => {
                event_store
                    .load_range(&context.run_id, arguments.from_sequence..end)
                    .await
            }
            None => event_store.load(&context.run_id).await,
        }
        .map_err(HistoryAccessError::EventStore)?
        .ok_or_else(|| HistoryAccessError::RunNotFound {
            run_id: context.run_id.clone(),
        })?;
        // A requested range may contain a result but not its preceding request.
        // Fetch only the matching requests so history-tool results stay hidden
        // without loading every earlier event and its potentially large output.
        let result_call_ids = stored
            .events
            .iter()
            .filter(|envelope| {
                envelope.sequence >= arguments.from_sequence && envelope.sequence <= to_sequence
            })
            .filter_map(|envelope| match &envelope.event {
                RunEvent::ToolCallSucceeded { call_id, .. }
                | RunEvent::ToolCallFailed { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let matching_calls = if result_call_ids.is_empty() {
            Vec::new()
        } else {
            event_store
                .load_tool_calls(&context.run_id, &result_call_ids)
                .await
                .map_err(HistoryAccessError::EventStore)?
        };
        let events =
            visible_history_events(&stored.events, context.through_sequence, &matching_calls)
                .into_iter()
                .filter(|envelope| {
                    envelope.sequence >= arguments.from_sequence && envelope.sequence <= to_sequence
                })
                .map(|envelope| HistoryReadEvent {
                    sequence: envelope.sequence,
                    step: envelope.step,
                    event_type: Cow::Borrowed(event_type(&envelope.event)),
                    event: Cow::Owned(envelope.event.without_provider_state()),
                })
                .collect();
        let response = HistoryReadResponse {
            warning: Cow::Borrowed(HISTORY_WARNING),
            checkpoint_id: Cow::Borrowed(&context.checkpoint_id),
            through_sequence: context.through_sequence,
            from_sequence: arguments.from_sequence,
            to_sequence,
            events,
        };
        encode_read_response(&response, arguments.offset.unwrap_or(0), max_output_chars)
    }
}

fn event_matches_query(event: &RunEvent, folded_query: &str) -> bool {
    // Search the original values, not their JSON-escaped representation. Keep
    // event types and tool identifiers searchable without matching JSON syntax
    // or unrelated envelope metadata.
    let matches = |value: &str| value.to_lowercase().contains(folded_query);
    if matches(event_type(event)) {
        return true;
    }
    let user_matches = |content: &str, files: &[crate::TextFileReference]| {
        matches(content)
            || files.iter().any(|file| {
                matches(&file.name)
                    || file.path.as_deref().is_some_and(matches)
                    || matches(&file.text)
            })
    };
    match event {
        RunEvent::RunStarted { instruction, files } => user_matches(instruction, files),
        RunEvent::InputReceived { content, files } => user_matches(content, files),
        RunEvent::ModelResponseReceived { content, .. } => matches(content),
        RunEvent::RunResumed {
            content,
            files,
            interrupted_calls,
        } => {
            user_matches(content, files)
                || interrupted_calls.iter().any(|call| matches(&call.call_id))
        }
        RunEvent::ToolCallRequested {
            call_id,
            tool_name,
            arguments,
        } => matches(call_id) || matches(tool_name) || matches(arguments),
        RunEvent::ToolCallSucceeded { call_id, output } => matches(call_id) || matches(output),
        RunEvent::ToolCallFailed { call_id, message } => matches(call_id) || matches(message),
        RunEvent::InputRequested { prompt } => matches(prompt),
        RunEvent::RunCompleted { answer } => matches(answer),
        RunEvent::RunFailed { message } => matches(message),
        RunEvent::ModelRequestStarted | RunEvent::RunCancelled => false,
    }
}

async fn load_history_prefix<S: EventStore>(
    event_store: &S,
    context: &HistoryContext,
) -> Result<Option<crate::storage::StoredRun>, EventStoreError> {
    match context.through_sequence.checked_add(1) {
        Some(end) => event_store.load_range(&context.run_id, 0..end).await,
        None => event_store.load(&context.run_id).await,
    }
}

fn visible_history_events<'a>(
    events: &'a [RunEventEnvelope],
    through_sequence: u64,
    matching_calls: &[RunEventEnvelope],
) -> Vec<&'a RunEventEnvelope> {
    let history_call_ids: HashSet<&str> = events
        .iter()
        .chain(matching_calls)
        .filter(|envelope| envelope.sequence <= through_sequence)
        .filter_map(|envelope| match &envelope.event {
            RunEvent::ToolCallRequested {
                call_id, tool_name, ..
            } if is_history_tool(tool_name) => Some(call_id.as_str()),
            _ => None,
        })
        .collect();

    events
        .iter()
        .filter(|envelope| envelope.sequence <= through_sequence)
        .filter(|envelope| match &envelope.event {
            RunEvent::ToolCallRequested { tool_name, .. } => !is_history_tool(tool_name),
            RunEvent::ToolCallSucceeded { call_id, .. }
            | RunEvent::ToolCallFailed { call_id, .. } => {
                !history_call_ids.contains(call_id.as_str())
            }
            _ => true,
        })
        .collect()
}

#[derive(Debug)]
pub enum HistoryAccessError {
    UnsupportedTool {
        name: String,
    },
    InvalidArguments {
        tool: &'static str,
        source: serde_json::Error,
    },
    EmptyQuery,
    ZeroRequestedLimit,
    RequestedLimitExceeded {
        requested: usize,
        configured: usize,
    },
    InvalidRange {
        from_sequence: u64,
        to_sequence: u64,
    },
    RangePastCheckpoint {
        to_sequence: u64,
        through_sequence: u64,
    },
    RangeSizeOverflow {
        from_sequence: u64,
        to_sequence: u64,
    },
    TooManyEvents {
        requested: u64,
        configured: usize,
    },
    RunNotFound {
        run_id: String,
    },
    OffsetPastEnd {
        offset: usize,
        total_chars: usize,
    },
    EventStore(EventStoreError),
    Serialization(serde_json::Error),
    OutputTooLarge {
        actual_chars: usize,
        configured: usize,
    },
}

impl fmt::Display for HistoryAccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTool { name } => {
                write!(formatter, "history access does not own tool {name:?}")
            }
            Self::InvalidArguments { tool, source } => {
                write!(
                    formatter,
                    "invalid arguments for history tool {tool}: {source}"
                )
            }
            Self::EmptyQuery => write!(formatter, "history search query must not be empty"),
            Self::ZeroRequestedLimit => {
                write!(formatter, "history search result limit must be non-zero")
            }
            Self::RequestedLimitExceeded {
                requested,
                configured,
            } => write!(
                formatter,
                "history search requested {requested} results, exceeding configured limit {configured}"
            ),
            Self::InvalidRange {
                from_sequence,
                to_sequence,
            } => write!(
                formatter,
                "invalid history range: from_sequence {from_sequence} is after to_sequence {to_sequence}"
            ),
            Self::RangePastCheckpoint {
                to_sequence,
                through_sequence,
            } => write!(
                formatter,
                "history range ends at sequence {to_sequence}, past readable checkpoint sequence {through_sequence}"
            ),
            Self::RangeSizeOverflow {
                from_sequence,
                to_sequence,
            } => write!(
                formatter,
                "history range size overflows: {from_sequence}..={to_sequence}"
            ),
            Self::TooManyEvents {
                requested,
                configured,
            } => write!(
                formatter,
                "history range requests {requested} events, exceeding configured limit {configured}"
            ),
            Self::RunNotFound { run_id } => write!(formatter, "run {run_id:?} was not found"),
            Self::OffsetPastEnd {
                offset,
                total_chars,
            } => write!(
                formatter,
                "history read offset {offset} is past the serialized response length of {total_chars} characters"
            ),
            Self::EventStore(source) => write!(formatter, "failed to load run history: {source}"),
            Self::Serialization(source) => {
                write!(formatter, "failed to serialize run history: {source}")
            }
            Self::OutputTooLarge {
                actual_chars,
                configured,
            } => write!(
                formatter,
                "history output contains {actual_chars} characters, exceeding configured limit {configured}"
            ),
        }
    }
}

impl Error for HistoryAccessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidArguments { source, .. } | Self::Serialization(source) => Some(source),
            Self::EventStore(source) => Some(source),
            _ => None,
        }
    }
}

impl ToolOutputProjector for EventStoreHistoryAccess {
    fn project_output(
        &self,
        tool_name: &str,
        output: &str,
        max_chars: usize,
    ) -> Option<Result<String, String>> {
        if !is_history_tool(tool_name) {
            return None;
        }
        Some(bound_saved_history_output(tool_name, output, max_chars).ok_or_else(|| {
            "History response cannot fit the active output limit without corrupting its protocol"
                .chars()
                .take(max_chars)
                .collect()
        }))
    }
}
