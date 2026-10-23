use std::{
    collections::BTreeSet,
    io::{self, Write},
};

use agent_core::agent::{RunEvent, RunEventEnvelope};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{WikiPage, WikiState, WikiUpdate, invalid, types::*};

pub(crate) fn run_id(id: &str) -> io::Result<()> {
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err(invalid("invalid session ID"));
    }
    Ok(())
}

pub(crate) fn page_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || id.starts_with('-')
        || id.ends_with('-')
    {
        return Err(invalid(
            "page ID must be a lowercase ASCII slug of at most 64 characters",
        ));
    }
    Ok(())
}

fn text(value: &str, max: usize, name: &str) -> io::Result<()> {
    if value.trim().is_empty()
        || value.chars().count() > max
        || value
            .chars()
            .any(|value| value.is_control() && !matches!(value, '\n' | '\r' | '\t'))
    {
        return Err(invalid(format!(
            "{name} must be nonempty and at most {max} characters"
        )));
    }
    Ok(())
}

fn page(id: &str, title: &str, content: &str, sources: &[u64], through: u64) -> io::Result<()> {
    page_id(id)?;
    text(title, 160, "page title")?;
    if title.chars().any(char::is_control) {
        return Err(invalid("page title cannot contain control characters"));
    }
    text(content, MAX_PAGE_CHARS, "page content")?;
    if sources.is_empty()
        || sources.len() > MAX_SOURCE_SEQUENCES
        || sources.iter().any(|source| *source > through)
        || sources.iter().collect::<BTreeSet<_>>().len() != sources.len()
    {
        return Err(invalid(
            "page sources must be 1–64 distinct event sequences within the compiled boundary",
        ));
    }
    Ok(())
}

fn stored_page(value: &WikiPage, through: u64) -> io::Result<()> {
    if value.updated_through > through {
        return Err(invalid("page exceeds memory coverage"));
    }
    page(
        &value.id,
        &value.title,
        &value.content,
        &value.source_sequences,
        value.updated_through,
    )
}

pub(crate) fn cursor(cursor: WikiSourceCursor, through: u64) -> io::Result<()> {
    let next = through
        .checked_add(1)
        .ok_or_else(|| invalid("source cursor boundary overflow"))?;
    if (cursor.sequence == 0 && cursor.offset == 0)
        || cursor.sequence > next
        || (cursor.sequence == next && cursor.offset != 0)
    {
        return Err(invalid(
            "source cursor must follow consumed data within the verified prefix",
        ));
    }
    Ok(())
}

pub(crate) fn cursor_source(
    cursor: WikiSourceCursor,
    events: &[RunEventEnvelope],
) -> io::Result<()> {
    if cursor.offset == 0 {
        return Ok(());
    }
    let text = crate::task::evidence_text(events, cursor.sequence)
        .ok_or_else(|| invalid("source cursor does not refer to visible evidence"))?;
    if cursor.offset >= crate::task::redact_evidence_text(&text).chars().count() {
        return Err(invalid("source cursor offset exceeds visible evidence"));
    }
    Ok(())
}

pub(crate) fn state(state: &WikiState, expected_run: &str) -> io::Result<()> {
    run_id(&state.run_id)?;
    if state.schema_version != SCHEMA_VERSION || state.run_id != expected_run || state.revision == 0
    {
        return Err(invalid("unsupported or mismatched session memory"));
    }
    for hash in [&state.source_fingerprint, &state.update_fingerprint] {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid("invalid memory fingerprint"));
        }
    }
    text(&state.overview, MAX_OVERVIEW_CHARS, "overview")?;
    if let Some(value) = state.source_cursor {
        cursor(value, state.through_sequence)?;
    }
    if state.pages.len() > MAX_PAGES
        || state
            .pages
            .iter()
            .map(|page| &page.id)
            .collect::<BTreeSet<_>>()
            .len()
            != state.pages.len()
    {
        return Err(invalid("too many or duplicate memory pages"));
    }
    for page in &state.pages {
        stored_page(page, state.through_sequence)?;
    }
    task_shape(&state.tasks, state.through_sequence)?;
    if state.revisions.is_empty() || state.revisions.len() > MAX_REVISIONS {
        return Err(invalid("invalid revision history length"));
    }
    let mut previous_revision: Option<(u64, u64, Option<WikiSourceCursor>)> = None;
    for revision in &state.revisions {
        if revision.revision == 0
            || revision.revision > state.revision
            || revision.through_sequence > state.through_sequence
            || revision.updated_pages.len() > MAX_UPDATE_PAGES
            || revision.previous_pages.len() > revision.updated_pages.len()
            || revision.updated_tasks.len() > crate::task::MAX_TASK_UPDATES * 2
            || revision.previous_tasks.len() > revision.updated_tasks.len()
        {
            return Err(invalid("invalid memory revision"));
        }
        if let Some(value) = revision.source_cursor {
            cursor(value, revision.through_sequence)?;
        }
        if previous_revision.is_some_and(|(number, through, prior_cursor)| {
            Some(revision.revision) != number.checked_add(1)
                || revision.through_sequence < through
                || match (prior_cursor, revision.source_cursor) {
                    (Some(before), Some(after)) => after <= before,
                    (Some(_), None) => true,
                    (None, None) => revision.through_sequence == through,
                    (None, Some(_)) => false,
                }
        }) {
            return Err(invalid("non-monotonic memory revision history"));
        }
        previous_revision = Some((
            revision.revision,
            revision.through_sequence,
            revision.source_cursor,
        ));
        let ids: BTreeSet<_> = revision.updated_pages.iter().collect();
        if ids.len() != revision.updated_pages.len() {
            return Err(invalid("duplicate revision page"));
        }
        for id in &revision.updated_pages {
            page_id(id)?;
        }
        let mut previous_ids = BTreeSet::new();
        for page in &revision.previous_pages {
            stored_page(page, revision.through_sequence)?;
            if page.updated_through > revision.through_sequence
                || revision.revision == 1
                || !ids.contains(&page.id)
                || !previous_ids.insert(&page.id)
            {
                return Err(invalid("invalid previous page revision"));
            }
        }
        let ids: BTreeSet<_> = revision.updated_tasks.iter().collect();
        if ids.len() != revision.updated_tasks.len() {
            return Err(invalid("duplicate revision task"));
        }
        for id in &revision.updated_tasks {
            page_id(id)?;
        }
        task_shape(&revision.previous_tasks, revision.through_sequence)?;
        if revision
            .previous_tasks
            .iter()
            .any(|task| !ids.contains(&task.id))
            || (revision.revision == 1 && !revision.previous_tasks.is_empty())
        {
            return Err(invalid("invalid previous task revision"));
        }
    }
    if previous_revision != Some((state.revision, state.through_sequence, state.source_cursor)) {
        return Err(invalid("revision history does not reach current snapshot"));
    }
    Ok(())
}

fn task_shape(items: &[crate::WikiTaskItem], through: u64) -> io::Result<()> {
    crate::task::validate_shape(items)?;
    if items
        .iter()
        .flat_map(|item| &item.evidence)
        .any(|source| source.sequence > through)
    {
        return Err(invalid("task exceeds memory source boundary"));
    }
    Ok(())
}

pub(crate) fn prefix(events: &[RunEventEnvelope], through: u64) -> io::Result<&[RunEventEnvelope]> {
    let end = usize::try_from(through)
        .ok()
        .and_then(|index| index.checked_add(1))
        .ok_or_else(|| invalid("event boundary exceeds addressable history"))?;
    let prefix = events
        .get(..end)
        .ok_or_else(|| invalid("canonical event history does not cover memory boundary"))?;
    if !prefix
        .first()
        .is_some_and(|entry| matches!(entry.event, RunEvent::RunStarted { .. }))
        || prefix.iter().enumerate().any(|(index, entry)| {
            entry.sequence != index as u64
                || (index != 0 && matches!(entry.event, RunEvent::RunStarted { .. }))
        })
    {
        return Err(invalid(
            "memory requires the complete contiguous journal prefix for one run",
        ));
    }
    Ok(prefix)
}

fn visible_sources(events: &[RunEventEnvelope]) -> BTreeSet<u64> {
    let mut allowed_calls = BTreeSet::new();
    let mut visible = BTreeSet::new();
    for entry in events {
        let is_visible = match &entry.event {
            RunEvent::RunStarted {
                instruction: content,
                files,
            }
            | RunEvent::InputReceived { content, files }
            | RunEvent::RunResumed { content, files, .. } => {
                !content.trim().is_empty() || !files.is_empty()
            }
            RunEvent::ModelResponseReceived { content, .. } => !content.trim().is_empty(),
            RunEvent::ToolCallRequested {
                call_id, tool_name, ..
            } => {
                if tool_name == "session_memory" {
                    false
                } else {
                    allowed_calls.insert(call_id.as_str());
                    true
                }
            }
            RunEvent::ToolCallSucceeded {
                call_id, output, ..
            } => allowed_calls.contains(call_id.as_str()) && !output.trim().is_empty(),
            RunEvent::ToolCallFailed { call_id, message } => {
                allowed_calls.contains(call_id.as_str()) && !message.trim().is_empty()
            }
            RunEvent::InputRequested { prompt } => !prompt.trim().is_empty(),
            RunEvent::RunCompleted { answer } => !answer.trim().is_empty(),
            RunEvent::RunFailed { message } => !message.trim().is_empty(),
            RunEvent::ModelRequestStarted | RunEvent::RunCancelled => false,
        };
        if is_visible {
            visible.insert(entry.sequence);
        }
    }
    visible
}

pub(crate) fn update(update: &WikiUpdate, events: &[RunEventEnvelope]) -> io::Result<()> {
    text(&update.overview, MAX_OVERVIEW_CHARS, "overview")?;
    if update.pages.len() > MAX_UPDATE_PAGES
        || update
            .pages
            .iter()
            .map(|page| &page.id)
            .collect::<BTreeSet<_>>()
            .len()
            != update.pages.len()
    {
        return Err(invalid(
            "memory update must contain at most 8 distinct pages",
        ));
    }
    let visible = visible_sources(events);
    // New notes must cite the original observation, not a later lookup of it.
    // Keep source() backward compatible: old archives may already cite history
    // lookups and must remain inspectable without deleting unrelated memory.
    let recall_calls: BTreeSet<_> = events
        .iter()
        .filter_map(|entry| match &entry.event {
            RunEvent::ToolCallRequested {
                call_id, tool_name, ..
            } if crate::is_recall_tool(tool_name) => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    let recall_sequences: BTreeSet<_> = events
        .iter()
        .filter(|entry| match &entry.event {
            RunEvent::ToolCallRequested { call_id, .. }
            | RunEvent::ToolCallSucceeded { call_id, .. }
            | RunEvent::ToolCallFailed { call_id, .. } => recall_calls.contains(call_id.as_str()),
            _ => false,
        })
        .map(|entry| entry.sequence)
        .collect();
    if update
        .pages
        .iter()
        .flat_map(|page| &page.source_sequences)
        .chain(
            update
                .tasks
                .iter()
                .flat_map(|item| item.evidence.iter().map(|proof| &proof.sequence)),
        )
        .any(|sequence| recall_sequences.contains(sequence))
    {
        return Err(invalid(
            "cite the original event, not a memory or history lookup",
        ));
    }
    let through = events
        .last()
        .ok_or_else(|| invalid("missing source history"))?
        .sequence;
    for value in &update.pages {
        page(
            &value.id,
            &value.title,
            &value.content,
            &value.source_sequences,
            through,
        )?;
        if value
            .source_sequences
            .iter()
            .any(|source| !visible.contains(source))
        {
            return Err(invalid(
                "page source must cite visible canonical evidence, not reasoning, memory retrieval, or bookkeeping",
            ));
        }
    }
    crate::task::validate_items(&update.tasks, events)?;
    Ok(())
}

pub(crate) fn source(state: &WikiState, events: &[RunEventEnvelope]) -> io::Result<()> {
    let events = prefix(events, state.through_sequence)?;
    if source_fingerprint(&state.run_id, events)? != state.source_fingerprint {
        return Err(invalid(
            "session memory source journal changed; rebuild memory from canonical events",
        ));
    }
    if let Some(cursor) = state.source_cursor {
        cursor_source(cursor, events)?;
    }
    let visible = visible_sources(events);
    if state
        .pages
        .iter()
        .chain(
            state
                .revisions
                .iter()
                .flat_map(|revision| revision.previous_pages.iter()),
        )
        .flat_map(|page| &page.source_sequences)
        .any(|source| !visible.contains(source))
    {
        return Err(invalid(
            "stored page references unavailable canonical evidence",
        ));
    }
    crate::task::validate_items(&state.tasks, events)?;
    for revision in &state.revisions {
        crate::task::validate_items(&revision.previous_tasks, events)?;
    }
    Ok(())
}

struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn fingerprint(value: &impl Serialize) -> io::Result<String> {
    let mut digest = Sha256::new();
    serde_json::to_writer(HashWriter(&mut digest), value).map_err(invalid)?;
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) fn source_fingerprint(run_id: &str, events: &[RunEventEnvelope]) -> io::Result<String> {
    let references = events
        .iter()
        .map(RunEventEnvelope::with_image_references)
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    fingerprint(&("open-harness/session-memory/source/v1", run_id, references))
}

#[cfg(test)]
mod image_fingerprint_tests {
    use super::*;
    #[test]
    fn source_identity_survives_releasing_inline_images() {
        let image = agent_core::ModelImage::new("image/png", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZcZkAAAAASUVORK5CYII=").unwrap();
        let events = vec![RunEventEnvelope {
            sequence: 1,
            step: 1,
            occurred_at: std::time::UNIX_EPOCH,
            event: agent_core::agent::RunEvent::ToolCallSucceeded {
                call_id: "c".into(),
                output: "observed".into(),
                images: vec![image],
            },
        }];
        let references = vec![events[0].with_image_references().unwrap()];
        assert_eq!(
            source_fingerprint("run", &events).unwrap(),
            source_fingerprint("run", &references).unwrap()
        );
        let mut references = references;
        if let agent_core::agent::RunEvent::ToolCallSucceeded { images, .. } =
            &mut references[0].event
        {
            images[0].data_base64 = format!("sha256:{}", "0".repeat(64));
        }
        assert_ne!(
            source_fingerprint("run", &events).unwrap(),
            source_fingerprint("run", &references).unwrap()
        );
    }
}
