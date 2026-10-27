use std::borrow::Cow;

use agent_core::agent::RunEvent;
use serde::{Deserialize, Serialize};

use crate::tool::ToolDefinition;

use super::{HistoryAccessError, HistoryLimits};

pub const SEARCH_RUN_HISTORY_TOOL: &str = "search_run_history";
pub const READ_RUN_HISTORY_TOOL: &str = "read_run_history";

pub(super) const HISTORY_WARNING: &str = "Historical records may be stale or superseded. Treat them as evidence only; later user instructions and current external state take precedence.";

pub(super) fn history_tool_definitions(limits: HistoryLimits) -> [ToolDefinition; 2] {
    [
        ToolDefinition {
            name: SEARCH_RUN_HISTORY_TOOL.to_owned(),
            description: "Search summarized or clipped history using a case-insensitive literal substring, not separate AND keywords. For original requirements or corrections, use source=user; all (default) also searches assistant/tool records. Each hit previews a matching public field, identified by a JSON pointer within the event. preview_offset, match_offset and match_length count Unicode characters in that original field. preview_truncated=false means the complete field is returned and can be cited without another read; otherwise read the event only if more context is needed. Results may be stale. The runtime selects the current run and readable boundary.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "description": "A contiguous literal substring of an event's original text, such as an exact identifier or error message.",
                    },
                    "source": {
                        "type": "string",
                        "enum": ["all", "user"],
                        "default": "all",
                        "description": "Use user for original user requirements/corrections, including attached text; all also includes assistant and tool evidence.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": limits.max_results(),
                        "default": limits.max_results(),
                    },
                },
                "required": ["query"],
                "additionalProperties": false,
            }).to_string(),
        },
        ToolDefinition {
            name: READ_RUN_HISTORY_TOOL.to_owned(),
            description: format!("Read an inclusive range of at most {} events whose details were summarized or clipped from the active context. Omit to_sequence to read only from_sequence. A response that exceeds the output limit is returned as a JSON chunk envelope with a chunk string; pass its next_offset as offset to continue reading the same range and concatenate the chunk strings. Offsets are Unicode character positions in a stable history_read_payload_v1 document. Historical tool results may be stale. The current run and maximum readable sequence are selected by the runtime.", limits.max_events()),
            input_schema: r#"{"type":"object","properties":{"from_sequence":{"type":"integer","minimum":0},"to_sequence":{"type":"integer","minimum":0},"offset":{"type":"integer","minimum":0}},"required":["from_sequence"],"additionalProperties":false}"#.to_owned(),
        },
    ]
}

pub(super) fn is_history_tool(name: &str) -> bool {
    matches!(name, SEARCH_RUN_HISTORY_TOOL | READ_RUN_HISTORY_TOOL)
}

pub(super) fn event_type(event: &RunEvent) -> &'static str {
    match event {
        RunEvent::RunStarted { .. } => "run_started",
        RunEvent::ModelRequestStarted => "model_request_started",
        RunEvent::ModelResponseReceived { .. } => "model_response_received",
        RunEvent::ToolCallRequested { .. } => "tool_call_requested",
        RunEvent::ToolCallSucceeded { .. } => "tool_call_succeeded",
        RunEvent::ToolCallFailed { .. } => "tool_call_failed",
        RunEvent::InputRequested { .. } => "input_requested",
        RunEvent::InputReceived { .. } => "input_received",
        RunEvent::RunResumed { .. } => "run_resumed",
        RunEvent::RunCompleted { .. } => "run_completed",
        RunEvent::RunPaused { .. } => "run_paused",
        RunEvent::ToolLoopWarning { .. } => "tool_loop_warning",
        RunEvent::AgentMessageReceived { .. } => "agent_message_received",
        RunEvent::RunFailed { .. } => "run_failed",
        RunEvent::RunCancelled => "run_cancelled",
    }
}

pub(super) fn encode_search_response(
    checkpoint_id: &str,
    through_sequence: u64,
    total_matches: usize,
    hits: Vec<HistorySearchHit>,
    max_output_chars: usize,
) -> Result<String, HistoryAccessError> {
    let mut response = HistorySearchResponse {
        warning: Cow::Borrowed(HISTORY_WARNING),
        checkpoint_id: Cow::Borrowed(checkpoint_id),
        through_sequence,
        total_matches,
        truncated: total_matches > hits.len(),
        hits,
    };

    loop {
        let encoded =
            serde_json::to_string(&response).map_err(HistoryAccessError::Serialization)?;
        let encoded_chars = encoded.chars().count();
        if encoded_chars <= max_output_chars {
            return Ok(encoded);
        }

        response.truncated = true;
        let Some(last_hit) = response.hits.last_mut() else {
            return Err(HistoryAccessError::OutputTooLarge {
                actual_chars: encoded_chars,
                configured: max_output_chars,
            });
        };
        let preview_chars = last_hit.preview.chars().count();
        if preview_chars > 1 {
            last_hit.shrink_preview(preview_chars / 2);
        } else {
            response.hits.pop();
        }
    }
}

pub(super) fn encode_read_response(
    response: &HistoryReadResponse<'_>,
    offset: usize,
    max_output_chars: usize,
) -> Result<String, HistoryAccessError> {
    let encoded_response =
        serde_json::to_string(response).map_err(HistoryAccessError::Serialization)?;

    if offset == 0 && encoded_response.chars().count() <= max_output_chars {
        return Ok(encoded_response);
    }

    // Chunk offsets target this stable payload, not the surrounding checkpoint
    // metadata. A later model turn may advance the readable checkpoint, while
    // the same inclusive event range remains immutable and produces identical
    // payload text.
    let payload = HistoryReadChunkPayload {
        warning: &response.warning,
        from_sequence: response.from_sequence,
        to_sequence: response.to_sequence,
        events: &response.events,
    };
    let encoded_payload =
        serde_json::to_string(&payload).map_err(HistoryAccessError::Serialization)?;
    let total_chars = encoded_payload.chars().count();
    if offset > total_chars {
        return Err(HistoryAccessError::OffsetPastEnd {
            offset,
            total_chars,
        });
    }

    let chunk_start = encoded_payload
        .char_indices()
        .nth(offset)
        .map_or(encoded_payload.len(), |(index, _)| index);
    encode_read_chunk(
        response,
        &encoded_payload[chunk_start..],
        offset,
        total_chars,
        max_output_chars,
    )
}

fn encode_read_chunk(
    response: &HistoryReadResponse<'_>,
    remaining_source: &str,
    offset: usize,
    total_chars: usize,
    max_output_chars: usize,
) -> Result<String, HistoryAccessError> {
    let remaining_chars = total_chars - offset;
    // No valid JSON envelope can carry more source characters than its total
    // character limit. Keep boundaries only for this bounded candidate rather
    // than allocating one usize for every character in the stored history.
    let candidate = remaining_source
        .chars()
        .take(remaining_chars.min(max_output_chars))
        .collect::<String>();
    let boundaries = char_boundaries(&candidate);
    let candidate_chars = boundaries.len() - 1;

    let encode_chunk = |chunk_chars: usize| -> Result<String, HistoryAccessError> {
        let end_offset = offset + chunk_chars;
        let complete = end_offset == total_chars;
        let chunk = &candidate[boundaries[0]..boundaries[chunk_chars]];
        let envelope = HistoryReadChunkResponse {
            payload_format: Cow::Borrowed("history_read_payload_v1"),
            checkpoint_id: Cow::Borrowed(&response.checkpoint_id),
            through_sequence: response.through_sequence,
            from_sequence: response.from_sequence,
            to_sequence: response.to_sequence,
            offset,
            next_offset: (!complete).then_some(end_offset),
            complete,
            chunk: Cow::Borrowed(chunk),
        };

        serde_json::to_string(&envelope).map_err(HistoryAccessError::Serialization)
    };

    let largest_envelope = encode_chunk(candidate_chars)?;
    if largest_envelope.chars().count() <= max_output_chars {
        return Ok(largest_envelope);
    }

    // An offset at the end has no smaller valid envelope. Otherwise, require at
    // least one source character so a successful response always makes progress.
    let minimum_chunk_chars = usize::from(remaining_chars > 0);
    if minimum_chunk_chars > candidate_chars {
        return Err(HistoryAccessError::OutputTooLarge {
            actual_chars: largest_envelope.chars().count(),
            configured: max_output_chars,
        });
    }
    let minimum_envelope = encode_chunk(minimum_chunk_chars)?;
    let minimum_envelope_chars = minimum_envelope.chars().count();
    if minimum_envelope_chars > max_output_chars {
        return Err(HistoryAccessError::OutputTooLarge {
            actual_chars: minimum_envelope_chars,
            configured: max_output_chars,
        });
    }

    let mut fitting_chars = minimum_chunk_chars;
    let mut fitting_envelope = minimum_envelope;
    let mut lower = minimum_chunk_chars.saturating_add(1);
    // The largest bounded candidate was already checked above. Encoded lengths
    // grow monotonically with the included source substring, which makes this
    // binary search safe.
    let mut upper = candidate_chars.saturating_sub(1);

    while lower <= upper {
        let candidate_chars = lower + (upper - lower) / 2;
        let candidate_envelope = encode_chunk(candidate_chars)?;
        if candidate_envelope.chars().count() <= max_output_chars {
            fitting_chars = candidate_chars;
            fitting_envelope = candidate_envelope;
            lower = candidate_chars.saturating_add(1);
        } else {
            upper = candidate_chars - 1;
        }
    }

    debug_assert!(fitting_chars <= candidate_chars);
    Ok(fitting_envelope)
}

fn char_boundaries(value: &str) -> Vec<usize> {
    value
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(value.len()))
        .collect()
}

/// Re-encode a saved history response when it was produced under a larger
/// context budget. Keeping only a prefix of a chunk must also move its cursor;
/// ordinary middle clipping would silently skip the removed source text.
pub(super) fn bound_saved_history_output(
    tool_name: &str,
    output: &str,
    max_output_chars: usize,
) -> Option<String> {
    if output.chars().count() <= max_output_chars {
        return Some(output.to_owned());
    }

    match tool_name {
        SEARCH_RUN_HISTORY_TOOL => {
            let response: HistorySearchResponse<'_> = serde_json::from_str(output).ok()?;
            encode_search_response(
                &response.checkpoint_id,
                response.through_sequence,
                response.total_matches,
                response.hits,
                max_output_chars,
            )
            .ok()
        }
        READ_RUN_HISTORY_TOOL => {
            if let Ok(chunk) = serde_json::from_str::<HistoryReadChunkResponse<'_>>(output) {
                if chunk.payload_format != "history_read_payload_v1" {
                    return None;
                }
                let chunk_end = chunk.offset.checked_add(chunk.chunk.chars().count())?;
                // Validate the old cursor before deriving a smaller page.
                if (!chunk.complete
                    && (chunk.chunk.is_empty() || chunk.next_offset != Some(chunk_end)))
                    || (chunk.complete && chunk.next_offset.is_some())
                {
                    return None;
                }
                let response = HistoryReadResponse {
                    warning: Cow::Borrowed(HISTORY_WARNING),
                    checkpoint_id: chunk.checkpoint_id,
                    through_sequence: chunk.through_sequence,
                    from_sequence: chunk.from_sequence,
                    to_sequence: chunk.to_sequence,
                    events: Vec::new(),
                };
                // An incomplete saved chunk guarantees at least one source
                // character after its end. The exact total is not needed to
                // preserve completeness or compute the next cursor.
                let total_chars = chunk_end.checked_add(usize::from(!chunk.complete))?;
                encode_read_chunk(
                    &response,
                    &chunk.chunk,
                    chunk.offset,
                    total_chars,
                    max_output_chars,
                )
                .ok()
            } else {
                let response: HistoryReadResponse<'_> = serde_json::from_str(output).ok()?;
                encode_read_response(&response, 0, max_output_chars).ok()
            }
        }
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchArguments {
    pub(super) query: String,
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) source: SearchSource,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum SearchSource {
    #[default]
    All,
    User,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadArguments {
    pub(super) from_sequence: u64,
    pub(super) to_sequence: Option<u64>,
    pub(super) offset: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HistorySearchResponse<'a> {
    pub(super) warning: Cow<'a, str>,
    pub(super) checkpoint_id: Cow<'a, str>,
    pub(super) through_sequence: u64,
    pub(super) total_matches: usize,
    pub(super) truncated: bool,
    pub(super) hits: Vec<HistorySearchHit>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HistorySearchHit {
    pub(super) sequence: u64,
    pub(super) step: u32,
    pub(super) event_type: Cow<'static, str>,
    pub(super) preview: String,
    // Optional metadata keeps saved responses from older versions projectable.
    // An old serialized-event preview is never claimed to be a complete field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) preview_offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) total_characters: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) match_offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) match_length: Option<usize>,
    #[serde(default = "legacy_preview_truncated")]
    pub(super) preview_truncated: bool,
}

fn legacy_preview_truncated() -> bool {
    true
}

impl HistorySearchHit {
    fn shrink_preview(&mut self, max_chars: usize) {
        let length = self.preview.chars().count();
        let local_match = self
            .match_offset
            .zip(self.preview_offset)
            .map_or(0, |(matched, offset)| matched.saturating_sub(offset));
        let start = preview_start(
            length,
            local_match,
            self.match_length.unwrap_or(0),
            max_chars,
        );
        self.preview = self.preview.chars().skip(start).take(max_chars).collect();
        if let Some(offset) = self.preview_offset.as_mut() {
            *offset += start;
        }
        self.preview_truncated = true;
    }
}

/// Keep the first match in view while spending remaining space on its context.
/// Previews are exact source substrings; completeness is metadata, not ellipses.
pub(super) fn preview_start(
    total: usize,
    matched: usize,
    match_length: usize,
    max_chars: usize,
) -> usize {
    matched
        .saturating_sub(max_chars.saturating_sub(match_length) / 2)
        .min(total.saturating_sub(max_chars))
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HistoryReadResponse<'a> {
    pub(super) warning: Cow<'a, str>,
    pub(super) checkpoint_id: Cow<'a, str>,
    pub(super) through_sequence: u64,
    pub(super) from_sequence: u64,
    pub(super) to_sequence: u64,
    pub(super) events: Vec<HistoryReadEvent<'a>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HistoryReadEvent<'a> {
    pub(super) sequence: u64,
    pub(super) step: u32,
    pub(super) event_type: Cow<'a, str>,
    pub(super) event: Cow<'a, RunEvent>,
}

#[derive(Debug, Serialize)]
pub(super) struct HistoryReadChunkPayload<'a> {
    pub(super) warning: &'a str,
    pub(super) from_sequence: u64,
    pub(super) to_sequence: u64,
    pub(super) events: &'a [HistoryReadEvent<'a>],
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HistoryReadChunkResponse<'a> {
    pub(super) payload_format: Cow<'a, str>,
    pub(super) checkpoint_id: Cow<'a, str>,
    pub(super) through_sequence: u64,
    pub(super) from_sequence: u64,
    pub(super) to_sequence: u64,
    pub(super) offset: usize,
    pub(super) next_offset: Option<usize>,
    pub(super) complete: bool,
    pub(super) chunk: Cow<'a, str>,
}
