use super::UpdateReason;
use agent_core::agent::{RunEvent, RunEventEnvelope};
use agent_memory::WikiSourceCursor;
pub(super) use agent_memory::redact_evidence_text as redact_tokens;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Serialize)]
pub(super) struct Source {
    pub sequence: u64,
    pub kind: &'static str,
    pub text: String,
    /// Unicode offsets into the redacted visible event, not byte offsets.
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub total_chars: usize,
}
#[derive(Clone)]
pub(super) struct Batch {
    pub through: u64,
    pub cursor: WikiSourceCursor,
    pub sources: Vec<Source>,
}

impl Source {
    fn cursor(&self) -> WikiSourceCursor {
        match self.next_offset {
            Some(offset) => WikiSourceCursor {
                sequence: self.sequence,
                offset,
            },
            None => WikiSourceCursor {
                sequence: self.sequence + 1,
                offset: 0,
            },
        }
    }
}

impl Batch {
    pub(super) fn source_chars(&self) -> usize {
        self.sources
            .iter()
            .map(|source| source.text.chars().count())
            .sum()
    }

    /// Keep an exact nonempty prefix, returning every remaining character to
    /// the durable cursor. Used by measured-budget binary search.
    pub(super) fn retain_prefix(&mut self, chars: usize) {
        let mut total = self.source_chars();
        assert!(chars > 0 && chars <= total);
        while self.sources.len() > 1 {
            let last = self.sources.last().unwrap().text.chars().count();
            if total - last < chars {
                break;
            }
            self.pop_source();
            total -= last;
        }
        if total > chars {
            let last = self.sources.last().unwrap().text.chars().count();
            self.shorten_last(last - (total - chars));
        }
    }

    /// Budget reduction must return unconsumed text to the next compilation.
    pub(super) fn pop_source(&mut self) {
        assert!(self.sources.len() > 1);
        self.sources.pop();
        self.refresh_cursor();
    }

    pub(super) fn shorten_last(&mut self, chars: usize) {
        let source = self.sources.last_mut().expect("nonempty memory batch");
        let original = source.text.chars().count();
        assert!(chars > 0 && chars < original);
        source.text = source.text.chars().take(chars).collect();
        source.next_offset = Some(source.offset + chars);
        self.refresh_cursor();
    }

    fn refresh_cursor(&mut self) {
        let source = self.sources.last().expect("nonempty memory batch");
        self.through = source.sequence;
        self.cursor = source.cursor();
    }
}

pub(super) fn batch(
    events: &[RunEventEnvelope],
    start: Option<WikiSourceCursor>,
    reason: UpdateReason,
) -> Option<Batch> {
    let recall_calls: HashSet<&str> = events
        .iter()
        .filter_map(|event| match &event.event {
            RunEvent::ToolCallRequested {
                call_id, tool_name, ..
            } if agent_memory::is_recall_tool(tool_name) => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    let mut sources = Vec::new();
    let mut chars = 0;
    let mut users = 0;
    let continuing_event = start.as_ref().is_some_and(|cursor| cursor.offset > 0);
    let mut cursor = start.unwrap_or(WikiSourceCursor {
        sequence: 0,
        offset: 0,
    });
    let start_sequence = cursor.sequence;
    let mut through = cursor.sequence;
    for envelope in events
        .iter()
        .filter(|event| event.sequence >= start_sequence)
    {
        through = envelope.sequence;
        let Some((kind, text)) = source_text(&envelope.event, &recall_calls) else {
            cursor = WikiSourceCursor {
                sequence: envelope.sequence + 1,
                offset: 0,
            };
            continue;
        };
        if text.trim().is_empty() {
            cursor = WikiSourceCursor {
                sequence: envelope.sequence + 1,
                offset: 0,
            };
            continue;
        }
        if kind == "user" {
            users += 1;
        }
        // Redact before splitting so a credential crossing a chunk boundary
        // cannot leak. The persisted cursor uses this deterministic text.
        let text = redact_tokens(&text);
        let total_chars = text.chars().count();
        let mut offset = if envelope.sequence == start_sequence {
            cursor.offset
        } else {
            0
        };
        // A corrupted cursor must not silently discard the rest of the event.
        if offset > total_chars {
            return None;
        }
        while offset < total_chars && chars < 24_000 && sources.len() < 64 {
            let size = (total_chars - offset).min(4_000).min(24_000 - chars);
            let end = offset + size;
            sources.push(Source {
                sequence: envelope.sequence,
                kind,
                text: text.chars().skip(offset).take(size).collect(),
                offset,
                next_offset: (end < total_chars).then_some(end),
                total_chars,
            });
            cursor = sources.last().unwrap().cursor();
            chars += size;
            offset = end;
        }
        if chars >= 24_000 || sources.len() >= 64 {
            break;
        }
    }
    let ready = match reason {
        UpdateReason::Progress => chars >= 12_000 || sources.len() >= 32 || users >= 8,
        UpdateReason::Response => chars >= 4_000 || sources.len() >= 16 || users >= 4,
        UpdateReason::Compaction => chars > 0,
    };
    ((ready || continuing_event) && !sources.is_empty()).then_some(Batch {
        through,
        cursor,
        sources,
    })
}

/// Only visible canonical event data is eligible for the memory compiler.
pub(super) fn source_text(
    event: &RunEvent,
    excluded_calls: &HashSet<&str>,
) -> Option<(&'static str, String)> {
    Some(match event {
        event @ (RunEvent::RunStarted { .. }
        | RunEvent::InputReceived { .. }
        | RunEvent::RunResumed { .. }) => ("user", event.user_input()?),
        RunEvent::ModelResponseReceived { content, .. } => ("assistant", content.clone()),
        RunEvent::ToolCallRequested {
            call_id,
            tool_name,
            arguments,
        } if !excluded_calls.contains(call_id.as_str()) => (
            "tool_requested_not_completed",
            format!("{call_id}: {tool_name} {arguments}"),
        ),
        RunEvent::ToolCallSucceeded { call_id, output }
            if !excluded_calls.contains(call_id.as_str()) =>
        {
            ("tool_succeeded", format!("{call_id}: {output}"))
        }
        RunEvent::ToolCallFailed { call_id, message }
            if !excluded_calls.contains(call_id.as_str()) =>
        {
            ("tool_failed", format!("{call_id}: {message}"))
        }
        RunEvent::RunFailed { message } => ("run_failed", message.clone()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn event(sequence: u64, event: RunEvent) -> RunEventEnvelope {
        RunEventEnvelope {
            sequence,
            step: 0,
            occurred_at: SystemTime::UNIX_EPOCH,
            event,
        }
    }

    fn user(sequence: u64, text: &str) -> RunEventEnvelope {
        event(
            sequence,
            if sequence == 0 {
                RunEvent::RunStarted {
                    instruction: text.into(),
                    files: vec![],
                }
            } else {
                RunEvent::InputReceived {
                    content: text.into(),
                    files: vec![],
                }
            },
        )
    }

    #[test]
    fn long_unicode_source_is_consumed_without_losing_the_middle_or_tiny_tail() {
        let original = format!(
            "{}禁止使用 tokenizer。{}尾",
            "甲🙂".repeat(12_000),
            "乙🙂".repeat(12_000)
        );
        let events = vec![user(0, &original)];
        let mut cursor = None;
        let mut recovered = String::new();
        let mut batches = 0;
        while let Some(batch) = batch(&events, cursor, UpdateReason::Progress) {
            for source in batch.sources {
                assert_eq!(source.offset, recovered.chars().count());
                assert!(source.text.chars().count() <= 4_000);
                recovered.push_str(&source.text);
            }
            cursor = Some(batch.cursor);
            batches += 1;
            assert!(batches < 10);
        }
        assert_eq!(recovered, original);
        assert_eq!(
            cursor,
            Some(WikiSourceCursor {
                sequence: 1,
                offset: 0
            })
        );
        assert!(batches >= 3);
    }

    #[test]
    fn a_batch_boundary_inside_the_next_event_does_not_starve_its_short_remainder() {
        let first = "前".repeat(23_990);
        let second = "小".repeat(205);
        let events = vec![user(0, &first), user(1, &second)];
        let initial = batch(&events, None, UpdateReason::Progress).unwrap();
        assert_eq!(
            initial.cursor,
            WikiSourceCursor {
                sequence: 1,
                offset: 10
            }
        );
        let remaining = batch(&events, Some(initial.cursor), UpdateReason::Progress).unwrap();
        assert_eq!(remaining.sources[0].offset, 10);
        assert_eq!(remaining.sources[0].text.chars().count(), 195);
        assert_eq!(
            remaining.cursor,
            WikiSourceCursor {
                sequence: 2,
                offset: 0
            }
        );
    }

    #[test]
    fn legacy_cursor_inside_history_lookup_skips_echo_without_losing_later_source() {
        let mut events = vec![
            user(0, "Original requirement"),
            event(
                1,
                RunEvent::ToolCallRequested {
                    call_id: "lookup".into(),
                    tool_name: agent_runtime::READ_RUN_HISTORY_TOOL.into(),
                    arguments: "{}".into(),
                },
            ),
            event(
                2,
                RunEvent::ToolCallSucceeded {
                    call_id: "lookup".into(),
                    output: "Old lookup copy. ".repeat(100),
                },
            ),
        ];
        let cursor = Some(WikiSourceCursor {
            sequence: 2,
            offset: 30,
        });
        assert!(batch(&events, cursor, UpdateReason::Compaction).is_none());
        events.push(user(3, "新的要求🙂"));
        let next = batch(&events, cursor, UpdateReason::Progress).unwrap();
        assert_eq!(next.sources.len(), 1);
        assert_eq!(next.sources[0].sequence, 3);
        assert_eq!(next.sources[0].offset, 0);
        assert_eq!(next.sources[0].text, "新的要求🙂");
        assert_eq!(
            next.cursor,
            WikiSourceCursor {
                sequence: 4,
                offset: 0
            }
        );
    }

    #[test]
    fn reducing_a_budget_returns_unconsumed_chunks_to_the_next_batch() {
        let original = "资料🙂".repeat(3_000);
        let events = vec![user(0, &original), user(1, "后续要求")];
        let mut initial = batch(&events, None, UpdateReason::Compaction).unwrap();
        while initial.sources.len() > 1 {
            initial.pop_source();
        }
        initial.shorten_last(777);
        assert_eq!(initial.through, 0);
        assert_eq!(
            initial.cursor,
            WikiSourceCursor {
                sequence: 0,
                offset: 777
            }
        );
        let remaining = batch(&events, Some(initial.cursor), UpdateReason::Progress).unwrap();
        let recovered: String = initial
            .sources
            .iter()
            .chain(&remaining.sources)
            .filter(|source| source.sequence == 0)
            .map(|source| source.text.as_str())
            .collect();
        assert_eq!(recovered, original);
        assert_eq!(remaining.sources.last().unwrap().text, "后续要求");
    }

    #[test]
    fn credentials_are_redacted_before_splitting_and_memory_echoes_are_excluded() {
        let original = format!(
            "{} sk-private-abcdefghijklmnopqrstuvwxyz 后续约束",
            "前".repeat(3_990)
        );
        let events = vec![
            user(0, &original),
            event(
                1,
                RunEvent::ToolCallRequested {
                    call_id: "recall".into(),
                    tool_name: "session_memory".into(),
                    arguments: "{}".into(),
                },
            ),
            event(
                2,
                RunEvent::ToolCallSucceeded {
                    call_id: "recall".into(),
                    output: "MEMORY_ECHO".into(),
                },
            ),
            event(3, RunEvent::ModelRequestStarted),
        ];
        let batch = batch(&events, None, UpdateReason::Compaction).unwrap();
        let recovered: String = batch
            .sources
            .iter()
            .map(|source| source.text.as_str())
            .collect();
        assert_eq!(recovered, redact_tokens(&original));
        assert!(!recovered.contains("sk-private"));
        assert!(!recovered.contains("MEMORY_ECHO"));
        assert_eq!(
            batch.cursor,
            WikiSourceCursor {
                sequence: 4,
                offset: 0
            }
        );
        assert_eq!(batch.through, 3);
    }
}
