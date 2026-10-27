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
        SearchSource, bound_saved_history_output, encode_read_response, encode_search_response,
        event_type, history_tool_definitions, is_history_tool, preview_start,
    },
};

#[derive(Debug, Clone)]
pub struct EventStoreHistoryAccess {
    limits: HistoryLimits,
    definitions: [ToolDefinition; 2],
    excluded_tools: HashSet<String>,
}

impl EventStoreHistoryAccess {
    pub fn new() -> Self {
        Self::with_limits(HistoryLimits::default())
    }

    pub fn with_limits(limits: HistoryLimits) -> Self {
        Self {
            limits,
            definitions: history_tool_definitions(limits),
            excluded_tools: HashSet::new(),
        }
    }

    /// Omit these tools' requests and results from historical evidence, in
    /// addition to this adapter's own lookup tools. Hosts can exclude derived
    /// memory/recall outputs without teaching the runtime about a plugin.
    /// Keep this configuration stable for a run so read cursors stay stable.
    /// Canonical events and ownership of executable tools remain unchanged.
    pub fn with_excluded_tools(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.excluded_tools = names.into_iter().collect();
        self
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
        let visible_events = visible_history_events(
            &stored.events,
            context.through_sequence,
            &[],
            &self.excluded_tools,
        );
        let folded_query = arguments.query.to_lowercase();
        let mut hits = Vec::with_capacity(limit);
        let mut total_matches = 0_usize;

        for envelope in visible_events {
            let Some(matched) = event_match(&envelope.event, &folded_query, &arguments.source)
            else {
                continue;
            };

            total_matches = total_matches.saturating_add(1);
            if hits.len() < limit {
                let total = matched.value.chars().count();
                let offset = preview_start(
                    total,
                    matched.offset,
                    matched.length,
                    self.limits.preview_chars(),
                );
                let preview: String = matched
                    .value
                    .chars()
                    .skip(offset)
                    .take(self.limits.preview_chars())
                    .collect();
                let preview_truncated = preview.chars().count() < total;
                hits.push(HistorySearchHit {
                    sequence: envelope.sequence,
                    step: envelope.step,
                    event_type: Cow::Borrowed(event_type(&envelope.event)),
                    preview,
                    field: Some(matched.field),
                    preview_offset: Some(offset),
                    total_characters: Some(total),
                    match_offset: Some(matched.offset),
                    match_length: Some(matched.length),
                    preview_truncated,
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
        // Fetch only the matching requests so excluded lookup results stay hidden
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
        let events = visible_history_events(
            &stored.events,
            context.through_sequence,
            &matching_calls,
            &self.excluded_tools,
        )
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

struct FieldMatch<'a> {
    field: String,
    value: &'a str,
    offset: usize,
    length: usize,
}

fn event_match<'a>(
    event: &'a RunEvent,
    folded_query: &str,
    source: &SearchSource,
) -> Option<FieldMatch<'a>> {
    let is_user = matches!(
        event,
        RunEvent::RunStarted { .. } | RunEvent::InputReceived { .. } | RunEvent::RunResumed { .. }
    );
    if *source == SearchSource::User && !is_user {
        return None;
    }
    // Explicit public fields avoid searching private reasoning, opaque provider
    // state, usage and envelope metadata. JSON pointers locate the original
    // string in read_run_history's event, including individual text attachments.
    let mut fields: Vec<(String, &str)> = Vec::new();
    match event {
        RunEvent::RunStarted { instruction, files } => {
            fields.push(("/instruction".into(), instruction));
            attachment_fields(&mut fields, files);
        }
        RunEvent::InputReceived { content, files }
        | RunEvent::RunResumed { content, files, .. } => {
            fields.push(("/content".into(), content));
            attachment_fields(&mut fields, files);
            if *source == SearchSource::All
                && let RunEvent::RunResumed {
                    interrupted_calls, ..
                } = event
            {
                for (index, call) in interrupted_calls.iter().enumerate() {
                    fields.push((format!("/interrupted_calls/{index}/call_id"), &call.call_id));
                }
            }
        }
        RunEvent::ModelResponseReceived { content, .. } => {
            fields.push(("/content".into(), content))
        }
        RunEvent::ToolCallRequested {
            call_id,
            tool_name,
            arguments,
        } => fields.extend([
            ("/arguments".into(), arguments.as_str()),
            ("/tool_name".into(), tool_name.as_str()),
            ("/call_id".into(), call_id.as_str()),
        ]),
        RunEvent::ToolCallSucceeded {
            call_id, output, ..
        } => fields.extend([
            ("/output".into(), output.as_str()),
            ("/call_id".into(), call_id.as_str()),
        ]),
        RunEvent::ToolCallFailed { call_id, message } => fields.extend([
            ("/message".into(), message.as_str()),
            ("/call_id".into(), call_id.as_str()),
        ]),
        RunEvent::InputRequested { prompt } => fields.push(("/prompt".into(), prompt)),
        RunEvent::RunCompleted { answer } => fields.push(("/answer".into(), answer)),
        RunEvent::RunFailed { message } => fields.push(("/message".into(), message)),
        RunEvent::AgentMessageReceived { message } => fields.extend([
            ("/message/text".into(), message.text.as_str()),
            ("/message/sender".into(), message.sender.as_str()),
        ]),
        RunEvent::ToolLoopWarning { tool_name, .. } => {
            fields.push(("/tool_name".into(), tool_name))
        }
        RunEvent::RunPaused { .. } | RunEvent::ModelRequestStarted | RunEvent::RunCancelled => {}
    }
    if *source == SearchSource::All {
        fields.push(("/type".into(), event_type(event)));
    }
    fields.into_iter().find_map(|(field, value)| {
        let (offset, length) = match_offsets(value, folded_query)?;
        Some(FieldMatch {
            field,
            value,
            offset,
            length,
        })
    })
}

fn attachment_fields<'a>(
    fields: &mut Vec<(String, &'a str)>,
    files: &'a [crate::TextFileReference],
) {
    for (index, file) in files.iter().enumerate() {
        fields.push((format!("/files/{index}/text"), &file.text));
        fields.push((format!("/files/{index}/name"), &file.name));
        if let Some(path) = &file.path {
            fields.push((format!("/files/{index}/path"), path));
        }
    }
}

fn match_offsets(value: &str, folded_query: &str) -> Option<(usize, usize)> {
    let folded = value.to_lowercase();
    let start_byte = folded.find(folded_query)?;
    let folded_start = folded[..start_byte].chars().count();
    let folded_end = folded_start + folded_query.chars().count();
    let mut folded_offset = 0;
    let mut original_start = None;
    for (index, character) in value.chars().enumerate() {
        // Lowercasing can expand one original character (e.g. İ -> i + dot).
        // Contextual sigma changes its value but not its character count.
        folded_offset += character.to_lowercase().count();
        if original_start.is_none() && folded_offset > folded_start {
            original_start = Some(index);
        }
        if folded_offset >= folded_end {
            let start = original_start?;
            return Some((start, index + 1 - start));
        }
    }
    None
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
    excluded_tools: &HashSet<String>,
) -> Vec<&'a RunEventEnvelope> {
    let excluded = |name: &str| is_history_tool(name) || excluded_tools.contains(name);
    let excluded_call_ids: HashSet<&str> = events
        .iter()
        .chain(matching_calls)
        .filter(|envelope| envelope.sequence <= through_sequence)
        .filter_map(|envelope| match &envelope.event {
            RunEvent::ToolCallRequested {
                call_id, tool_name, ..
            } if excluded(tool_name) => Some(call_id.as_str()),
            _ => None,
        })
        .collect();

    events
        .iter()
        .filter(|envelope| envelope.sequence <= through_sequence)
        .filter(|envelope| match &envelope.event {
            RunEvent::ToolCallRequested { tool_name, .. } => !excluded(tool_name),
            RunEvent::ToolCallSucceeded { call_id, .. }
            | RunEvent::ToolCallFailed { call_id, .. } => {
                !excluded_call_ids.contains(call_id.as_str())
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
