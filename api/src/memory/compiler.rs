use super::sources::{Batch, redact_tokens};
use agent_memory::{MAX_PAGES, WikiState, WikiTaskKind, WikiTaskStatus, WikiUpdate};
use agent_runtime::{
    ApproximateTokenCounter, Model, ModelFinishReason, ModelMessage, ModelRequest, TokenCounter,
};
use serde_json::json;
use std::{collections::HashSet, io, time::Duration};

const INSTRUCTIONS: &str = include_str!("prompt.txt");

pub(super) async fn compile<M: Model>(
    model: &M,
    previous: Option<&WikiState>,
    batch: &mut Batch,
) -> io::Result<WikiUpdate> {
    let metadata = model.context_metadata();
    let window = metadata.map_or(128_000, |m| m.context_window);
    let output = metadata
        .and_then(|m| m.max_output_tokens)
        .unwrap_or(4_096)
        .min(4_096)
        .min(window / 4);
    let input_budget = window
        .saturating_sub(output + (window / 100).clamp(64, 2_048))
        .min(32_000);
    let counter = ApproximateTokenCounter::conservative();
    let mut pages = Vec::new();
    let mut tasks = Vec::new();
    let remaining_page_slots =
        MAX_PAGES.saturating_sub(previous.map_or(0, |state| state.pages.len()));
    let index: Vec<_> = previous
        .into_iter()
        .flat_map(|state| state.pages.iter())
        .map(|page| json!({"id":page.id,"title":page.title}))
        .collect();
    let task_index: Vec<_> = previous
        .into_iter()
        .flat_map(|state| state.tasks.iter())
        .map(|task| json!({"id":task.id,"kind":task.kind,"status":task.status}))
        .collect();
    let mut request = ModelRequest {
        instructions: INSTRUCTIONS.into(),
        messages: Vec::new(),
        tools: Vec::new(),
        max_output_tokens: Some(output),
        provider_context: None,
    };
    let compose = |pages: &Vec<&agent_memory::WikiPage>,
                   tasks: &Vec<&agent_memory::WikiTaskItem>,
                   batch: &Batch,
                   include_sources: bool| {
        let sources = if include_sources {
            batch.sources.as_slice()
        } else {
            &[]
        };
        ModelMessage::user(
            json!({
                "previous_overview": previous.map(|state| &state.overview),
                "existing_page_index": index,
                "remaining_page_slots": remaining_page_slots,
                "editable_pages": pages,
                "existing_task_index": task_index,
                "editable_tasks": tasks,
                "new_source_events": sources,
                "next_source_cursor": batch.cursor,
            })
            .to_string(),
        )
    };
    // Measure fixed instructions/index/overview independently of evidence.
    // Old records may use at most half the remaining room, but only their
    // actual measured cost is reserved. A tiny old page does not halve source
    // throughput, even when the fixed prompt itself exceeds half the window.
    request.messages = vec![compose(&pages, &tasks, batch, false)];
    let fixed_tokens = counter.count_tokens(&request);
    let context_limit = fixed_tokens + input_budget.saturating_sub(fixed_tokens) / 2;
    // Load full pages, never partial bodies that could silently lose facts on
    // replacement. Prefer pages overlapping new evidence, then recent pages.
    let source_text = batch
        .sources
        .iter()
        .map(|source| source.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    // Task state has a separate budgeted inventory. Never replace an item
    // whose full text and evidence were not shown to the compiler.
    let mut task_candidates: Vec<_> = previous
        .into_iter()
        .flat_map(|state| &state.tasks)
        .collect();
    task_candidates.sort_by_key(|task| {
        let relevant = overlap(&source_text, &task.text);
        std::cmp::Reverse((
            relevant > 0,
            task.status == WikiTaskStatus::Active
                && matches!(task.kind, WikiTaskKind::Goal | WikiTaskKind::Constraint),
            relevant,
            task.evidence
                .iter()
                .map(|proof| proof.sequence)
                .max()
                .unwrap_or(0),
        ))
    });
    for task in task_candidates {
        tasks.push(task);
        request.messages = vec![compose(&pages, &tasks, batch, false)];
        if counter.count_tokens(&request) > context_limit {
            tasks.pop();
        }
        if tasks.len() == 8 {
            break;
        }
    }
    let mut candidates: Vec<_> = previous
        .into_iter()
        .flat_map(|state| &state.pages)
        .collect();
    candidates.sort_by_key(|page| {
        std::cmp::Reverse((overlap(&source_text, &page.title), page.updated_through))
    });
    for page in candidates {
        pages.push(page);
        request.messages = vec![compose(&pages, &tasks, batch, false)];
        if counter.count_tokens(&request) > context_limit {
            pages.pop();
        }
        if pages.len() == 8 {
            break;
        }
    }
    request.messages = vec![compose(&pages, &tasks, batch, true)];
    if counter.count_tokens(&request) > input_budget {
        // Find the longest source prefix that fits the actual selected state.
        // Every probe preserves source order; committing the chosen cursor
        // leaves its entire remainder available for subsequent compilations.
        let original = batch.clone();
        let total = original.source_chars();
        let mut low = total.min(128);
        let mut high = total.saturating_sub(1);
        let mut best = None;
        while low <= high {
            let mid = low + (high - low) / 2;
            let mut candidate = original.clone();
            candidate.retain_prefix(mid);
            request.messages = vec![compose(&pages, &tasks, &candidate, true)];
            if counter.count_tokens(&request) <= input_budget {
                best = Some(candidate);
                low = mid + 1;
            } else {
                high = mid.saturating_sub(1);
            }
        }
        *batch = best.ok_or_else(|| io::Error::other("memory batch exceeds model budget"))?;
        request.messages = vec![compose(&pages, &tasks, batch, true)];
    }
    let output = tokio::time::timeout(Duration::from_secs(20), model.respond(request))
        .await
        .map_err(|_| io::Error::other("memory compilation timed out"))?
        .map_err(|_| io::Error::other("memory model request failed"))?;
    if !matches!(
        output.finish_reason,
        ModelFinishReason::Stop | ModelFinishReason::InputRequired
    ) || !output.calls().is_empty()
    {
        return Err(io::Error::other("incomplete memory response"));
    }
    let text = output.text();
    if text.len() > 128 * 1024 {
        return Err(io::Error::other("memory response too large"));
    }
    let text = text
        .trim()
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(text.trim());
    let mut update: WikiUpdate = serde_json::from_str(text.trim())
        .map_err(|_| io::Error::other("invalid memory response"))?;
    let existing_ids: HashSet<_> = previous
        .into_iter()
        .flat_map(|state| state.pages.iter().map(|page| page.id.as_str()))
        .collect();
    let new_ids: HashSet<_> = update
        .pages
        .iter()
        .filter(|page| !existing_ids.contains(page.id.as_str()))
        .map(|page| page.id.as_str())
        .collect();
    if new_ids.len() > remaining_page_slots {
        return Err(io::Error::other(
            "memory response exceeds remaining page capacity",
        ));
    }
    let editable: HashSet<_> = pages.iter().map(|page| page.id.as_str()).collect();
    let allowed_sources: HashSet<_> = batch
        .sources
        .iter()
        .map(|source| source.sequence)
        .chain(
            pages
                .iter()
                .flat_map(|page| page.source_sequences.iter().copied()),
        )
        .collect();
    for page in &mut update.pages {
        if previous.is_some_and(|state| state.pages.iter().any(|old| old.id == page.id))
            && !editable.contains(page.id.as_str())
        {
            return Err(io::Error::other("cannot replace an unread memory page"));
        }
        if page
            .source_sequences
            .iter()
            .any(|source| !allowed_sources.contains(source))
        {
            return Err(io::Error::other("memory cites an unseen source"));
        }
        page.content = redact_tokens(&page.content);
        page.title = redact_tokens(&page.title);
        page.source_sequences.sort_unstable();
        page.source_sequences.dedup();
    }
    for task in &mut update.tasks {
        if previous.is_some_and(|state| state.tasks.iter().any(|old| old.id == task.id))
            && !tasks.iter().any(|old| old.id == task.id)
        {
            return Err(io::Error::other("cannot replace an unread task item"));
        }
        for proof in &task.evidence {
            let seen = batch.sources.iter().any(|source| {
                source.sequence == proof.sequence && source.text.contains(&proof.quote)
            }) || tasks
                .iter()
                .any(|old| old.evidence.iter().any(|evidence| evidence == proof));
            if proof.quote.trim().is_empty()
                || proof.quote.contains("[redacted]")
                || redact_tokens(&proof.quote) != proof.quote
                || !seen
            {
                return Err(io::Error::other("task item cites unseen evidence"));
            }
        }
        task.text = redact_tokens(&task.text);
    }
    update.overview = redact_tokens(&update.overview);
    Ok(update)
}

fn overlap(source: &str, title: &str) -> usize {
    let title = title.to_lowercase();
    let chars: Vec<_> = title.chars().collect();
    chars
        .windows(2)
        .filter(|pair| source.contains(&pair.iter().collect::<String>()))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::agent::{RunEvent, RunEventEnvelope};
    use agent_runtime::{ModelContextMetadata, ModelResponse};
    use serde_json::Value;
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
        time::SystemTime,
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "harness-wiki-capacity-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        metadata: ModelContextMetadata,
        requests: Mutex<Vec<ModelRequest>>,
        response: String,
    }
    impl Fixture {
        fn new(window: usize, response: Value) -> Self {
            Self {
                metadata: ModelContextMetadata::new("fixture", "compiler", window).unwrap(),
                requests: Mutex::new(Vec::new()),
                response: response.to_string(),
            }
        }
    }
    impl Model for Fixture {
        type Error = io::Error;
        fn context_metadata(&self) -> Option<&ModelContextMetadata> {
            Some(&self.metadata)
        }
        async fn respond(&self, request: ModelRequest) -> io::Result<ModelResponse> {
            self.requests.lock().unwrap().push(request);
            Ok(ModelResponse::final_answer(self.response.clone()))
        }
    }
    fn events(text: &str) -> Vec<RunEventEnvelope> {
        vec![RunEventEnvelope {
            sequence: 0,
            step: 0,
            occurred_at: SystemTime::UNIX_EPOCH,
            event: RunEvent::RunStarted {
                instruction: text.into(),
                files: vec![],
            },
        }]
    }

    #[tokio::test]
    async fn small_window_compilation_preserves_all_unicode_source_content_across_calls() {
        let original = format!(
            "{}不要重试已失败方案。{}",
            "甲🙂".repeat(2_500),
            "乙🙂".repeat(2_500)
        );
        let events = events(&original);
        let model = Fixture::new(
            8_192,
            json!({"overview":"Task notes", "pages":[], "tasks":[]}),
        );
        let mut cursor = None;
        let mut consumed = String::new();
        for _ in 0..100 {
            let Some(mut batch) = super::super::sources::batch(
                &events,
                cursor,
                super::super::UpdateReason::Compaction,
            ) else {
                break;
            };
            compile(&model, None, &mut batch).await.unwrap();
            for source in &batch.sources {
                assert_eq!(source.offset, consumed.chars().count());
                consumed.push_str(&source.text);
            }
            cursor = Some(batch.cursor);
        }
        assert_eq!(consumed, original);
        let requests = model.requests.lock().unwrap();
        assert!(requests.len() > 1);
        for request in requests.iter() {
            assert!(
                ApproximateTokenCounter::conservative().count_tokens(request) <= 8_192 - 2_048 - 81
            );
        }
    }

    #[tokio::test]
    async fn a_valid_sequence_does_not_authorize_a_quote_from_an_unseen_chunk() {
        let events = events(&format!("{}UNSEEN-MIDDLE", "甲🙂".repeat(5_000)));
        let mut batch =
            super::super::sources::batch(&events, None, super::super::UpdateReason::Compaction)
                .unwrap();
        let model = Fixture::new(
            8_192,
            json!({
                "overview":"Task notes", "pages":[], "tasks":[{
                    "id":"constraint", "kind":"constraint", "status":"active", "text":"Alleged requirement",
                    "evidence":[{"sequence":0,"quote":"UNSEEN-MIDDLE"}]
                }]
            }),
        );
        let error = compile(&model, None, &mut batch).await.unwrap_err();
        assert!(error.to_string().contains("unseen evidence"));
        assert!(batch.cursor.offset > 0);
        assert!(
            !batch
                .sources
                .iter()
                .any(|source| source.text.contains("UNSEEN-MIDDLE"))
        );
    }

    #[tokio::test]
    async fn a_small_existing_page_does_not_reserve_half_the_model_window() {
        let previous: WikiState = serde_json::from_value(json!({
            "schema_version":1, "run_id":"run", "revision":1, "through_sequence":0,
            "source_fingerprint":"a".repeat(64), "update_fingerprint":"b".repeat(64),
            "overview":"已有工作记录。".repeat(20), "tasks":[], "revisions":[],
            "pages":[{"id":"work", "title":"工作", "content":"已有工作记录。".repeat(20), "source_sequences":[0], "updated_through":0}]
        })).unwrap();
        let original = "中文材料🙂".repeat(2_600);
        let events = events(&original);
        let model = Fixture::new(16_384, json!({"overview":"Notes", "pages":[], "tasks":[]}));
        let mut cursor = None;
        let mut consumed = String::new();
        for _ in 0..20 {
            let Some(mut batch) = super::super::sources::batch(
                &events,
                cursor,
                super::super::UpdateReason::Compaction,
            ) else {
                break;
            };
            let remaining = original.chars().count() - consumed.chars().count();
            compile(&model, Some(&previous), &mut batch).await.unwrap();
            if remaining >= 1_000 {
                assert!(batch.source_chars() >= 1_000);
            }
            for source in &batch.sources {
                consumed.push_str(&source.text);
            }
            cursor = Some(batch.cursor);
        }
        assert_eq!(consumed, original);
        for request in model.requests.lock().unwrap().iter() {
            let payload: Value = serde_json::from_str(&request.messages[0].text()).unwrap();
            assert_eq!(payload["editable_pages"].as_array().unwrap().len(), 1);
            assert!(
                ApproximateTokenCounter::conservative().count_tokens(request)
                    <= 16_384 - 4_096 - 163
            );
        }
    }

    #[tokio::test]
    async fn compiler_cannot_replace_a_task_listed_only_in_the_index() {
        let tasks: Vec<_> = (0..9).map(|index| json!({
            "id":format!("task-{index}"), "kind":"work", "status":"planned", "text":"Inspect files",
            "evidence":[{"sequence":0,"quote":"Inspect files"}]
        })).collect();
        let previous: WikiState = serde_json::from_value(json!({
            "schema_version":1, "run_id":"run", "revision":1, "through_sequence":0,
            "source_fingerprint":"a".repeat(64), "update_fingerprint":"b".repeat(64),
            "overview":"Existing notes", "pages":[], "tasks":tasks, "revisions":[]
        }))
        .unwrap();
        let mut batch = super::super::sources::batch(
            &events("Inspect files"),
            None,
            super::super::UpdateReason::Compaction,
        )
        .unwrap();
        let model = Fixture::new(
            128_000,
            json!({"overview":"Notes", "pages":[], "tasks":[{
                "id":"task-8", "kind":"work", "status":"planned", "text":"Inspect files again",
                "evidence":[{"sequence":0,"quote":"Inspect files"}]
            }]}),
        );
        let error = compile(&model, Some(&previous), &mut batch)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unread task item"));
        let requests = model.requests.lock().unwrap();
        let payload: Value = serde_json::from_str(&requests[0].messages[0].text()).unwrap();
        assert_eq!(payload["existing_task_index"].as_array().unwrap().len(), 9);
        assert_eq!(payload["editable_tasks"].as_array().unwrap().len(), 8);
    }

    #[tokio::test]
    async fn a_full_wiki_updates_existing_pages_without_losing_notes_or_advancing_failed_updates() {
        use agent_memory::{MAX_UPDATE_PAGES, WikiPageUpdate, WikiStore};
        let directory = Directory::new();
        let store = WikiStore::open(&directory.0).unwrap();
        let mut history = events("Original project evidence");
        let seed_pages: Vec<_> = (0..MAX_PAGES)
            .map(|index| WikiPageUpdate {
                id: format!("topic-{index}"),
                title: if index == 0 {
                    "Needle governance".into()
                } else {
                    format!("Topic {index}")
                },
                content: format!("Preserved original fact {index}. [event #0]"),
                source_sequences: vec![0],
            })
            .collect();
        for (index, pages) in seed_pages.chunks(MAX_UPDATE_PAGES).enumerate() {
            if index > 0 {
                history.push(RunEventEnvelope {
                    sequence: index as u64,
                    step: 0,
                    occurred_at: SystemTime::UNIX_EPOCH,
                    event: RunEvent::InputReceived {
                        content: format!("Seed source {index}"),
                        files: vec![],
                    },
                });
            }
            store
                .apply(
                    "full",
                    &history,
                    index as u64,
                    WikiUpdate {
                        overview: "Existing project notes".into(),
                        pages: pages.to_vec(),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        let previous = store.load("full").unwrap().unwrap();
        assert_eq!(previous.pages.len(), MAX_PAGES);
        let sequence = history.len() as u64;
        history.push(RunEventEnvelope {
            sequence,
            step: 0,
            occurred_at: SystemTime::UNIX_EPOCH,
            event: RunEvent::InputReceived {
                content:
                    "Needle governance: preserve the original facts and add the approved revision."
                        .into(),
                files: vec![],
            },
        });
        let mut batch = super::super::sources::batch(
            &history,
            previous.source_cursor,
            super::super::UpdateReason::Compaction,
        )
        .unwrap();
        let overflow = json!({
            "overview":"New notes", "pages":[{"id":"overflow", "title":"Overflow", "content":"A new page", "source_sequences":[sequence]}], "tasks":[]
        });
        let model = Fixture::new(128_000, overflow.clone());
        let error = compile(&model, Some(&previous), &mut batch)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("remaining page capacity"));
        assert!(
            store
                .apply_progress(
                    "full",
                    &history,
                    batch.through,
                    batch.cursor,
                    serde_json::from_value(overflow).unwrap()
                )
                .is_err()
        );
        assert_eq!(store.load("full").unwrap().unwrap(), previous);

        let original = previous
            .pages
            .iter()
            .find(|page| page.id == "topic-0")
            .unwrap();
        let content = format!(
            "{}\nApproved revision. [event #{sequence}]",
            original.content
        );
        let model = Fixture::new(
            128_000,
            json!({
                "overview":"Updated project notes", "pages":[{"id":original.id,"title":original.title,"content":content,"source_sequences":[0,sequence]}], "tasks":[]
            }),
        );
        let update = compile(&model, Some(&previous), &mut batch).await.unwrap();
        let state = store
            .apply_progress("full", &history, batch.through, batch.cursor, update)
            .unwrap();
        assert_eq!(state.pages.len(), MAX_PAGES);
        assert!(state.source_cursor > previous.source_cursor);
        assert_eq!(state.revision, previous.revision + 1);
        assert_eq!(
            state
                .pages
                .iter()
                .find(|page| page.id == original.id)
                .unwrap()
                .content,
            content
        );
        for page in previous.pages.iter().filter(|page| page.id != original.id) {
            assert_eq!(
                state
                    .pages
                    .iter()
                    .find(|stored| stored.id == page.id)
                    .unwrap(),
                page
            );
        }
        let requests = model.requests.lock().unwrap();
        let payload: Value = serde_json::from_str(&requests[0].messages[0].text()).unwrap();
        assert_eq!(payload["remaining_page_slots"], 0);
        assert_eq!(
            payload["existing_page_index"].as_array().unwrap().len(),
            MAX_PAGES
        );
        assert!(
            payload["editable_pages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|page| page["id"] == original.id)
        );
    }
}
