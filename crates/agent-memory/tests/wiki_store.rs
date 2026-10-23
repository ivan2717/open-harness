use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agent_core::agent::{RunEvent, RunEventEnvelope};
use agent_memory::{
    MAX_QUERY_OUTPUT_CHARS, WikiEvidence, WikiPageUpdate, WikiSourceCursor, WikiStore,
    WikiTaskItem, WikiTaskKind, WikiTaskStatus, WikiUpdate, tool_definition,
};
use serde_json::{Value, json};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "open-harness-wiki-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run_directory(&self) -> PathBuf {
        fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn events(values: Vec<RunEvent>) -> Vec<RunEventEnvelope> {
    values
        .into_iter()
        .enumerate()
        .map(|(sequence, event)| RunEventEnvelope {
            sequence: sequence as u64,
            step: 0,
            occurred_at: UNIX_EPOCH,
            event,
        })
        .collect()
}

fn history() -> Vec<RunEventEnvelope> {
    events(vec![
        RunEvent::RunStarted {
            instruction: "Implement a wiki memory system".into(),
            files: vec![],
        },
        RunEvent::ModelRequestStarted,
        RunEvent::ModelResponseReceived {
            content: "I will inspect the repository.".into(),
            response: None,
            provider_state: None,
            provider_continuation: None,
            token_usage: None,
        },
        RunEvent::ToolCallRequested {
            call_id: "read".into(),
            tool_name: "read_file".into(),
            arguments: "{}".into(),
        },
        RunEvent::ToolCallSucceeded {
            call_id: "read".into(),
            output: "The repository uses Rust and TypeScript.".into(),
            images: Vec::new(),
        },
        RunEvent::InputReceived {
            content: "Keep all memory local.".into(),
            files: vec![],
        },
    ])
}

fn page(id: &str, content: &str, sources: &[u64]) -> WikiPageUpdate {
    WikiPageUpdate {
        id: id.into(),
        title: format!("Page {id}"),
        content: content.into(),
        source_sequences: sources.to_vec(),
    }
}

fn update(pages: Vec<WikiPageUpdate>) -> WikiUpdate {
    WikiUpdate {
        overview: "Build a local wiki for this session.".into(),
        pages,
        tasks: Vec::new(),
    }
}

#[test]
fn upserts_survive_restart_keep_untouched_pages_and_previous_versions() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    let unchanged = source.clone();
    store
        .apply(
            "run",
            &source,
            4,
            update(vec![
                page("task", "Build session memory", &[0]),
                page("stack", "Rust and TypeScript", &[4]),
            ]),
        )
        .unwrap();
    let result = store
        .apply(
            "run",
            &source,
            5,
            update(vec![page("task", "Build local session memory", &[0, 5])]),
        )
        .unwrap();
    assert_eq!(result.revision, 2);
    assert_eq!(result.pages.len(), 2);
    assert_eq!(
        result
            .pages
            .iter()
            .find(|page| page.id == "stack")
            .unwrap()
            .updated_through,
        4
    );
    assert_eq!(
        result.revisions[1].previous_pages[0].content,
        "Build session memory"
    );
    assert_eq!(source, unchanged);
    let restarted = WikiStore::open(&directory.0).unwrap();
    assert_eq!(
        restarted.validate_source("run", &source).unwrap(),
        Some(result)
    );
    let run = directory.run_directory();
    assert!(
        fs::read_to_string(run.join("index.md"))
            .unwrap()
            .contains("pages/task.md")
    );
    assert!(
        fs::read_to_string(run.join("pages/task.md"))
            .unwrap()
            .contains("event:5")
    );
    assert!(
        fs::read_to_string(run.join("log.md"))
            .unwrap()
            .contains("Revision 2")
    );
}

#[test]
fn repeated_updates_are_idempotent_and_old_or_conflicting_updates_are_rejected() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    let change = update(vec![page("task", "Local memory", &[0, 5])]);
    let first = store.apply("run", &source, 5, change.clone()).unwrap();
    assert_eq!(store.apply("run", &source, 5, change).unwrap(), first);
    assert!(
        store
            .apply(
                "run",
                &source,
                5,
                update(vec![page("task", "Conflicting content", &[0])])
            )
            .is_err()
    );
    assert!(store.apply("run", &source, 4, update(vec![])).is_err());
    assert_eq!(store.load("run").unwrap(), Some(first));
}

#[test]
fn source_chunks_commit_notes_and_progress_together_across_restarts() {
    let directory = Directory::new();
    let source = history();
    let first_cursor = WikiSourceCursor {
        sequence: 0,
        offset: 10,
    };
    let change = update(vec![page("task", "First source chunk", &[0])]);
    let first = WikiStore::open(&directory.0)
        .unwrap()
        .apply_progress("run", &source, 0, first_cursor, change.clone())
        .unwrap();
    let restarted = WikiStore::open(&directory.0).unwrap();
    assert_eq!(
        restarted.validate_source("run", &source).unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        restarted
            .apply_progress("run", &source, 0, first_cursor, change)
            .unwrap(),
        first
    );

    let second_cursor = WikiSourceCursor {
        sequence: 0,
        offset: 20,
    };
    let second = restarted
        .apply_progress(
            "run",
            &source,
            0,
            second_cursor,
            update(vec![page("task", "First and middle chunks", &[0])]),
        )
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(second.through_sequence, 0);
    assert_eq!(second.source_cursor, Some(second_cursor));
    assert_eq!(
        second.revisions[1].previous_pages[0].content,
        "First source chunk"
    );
    let complete = WikiSourceCursor {
        sequence: 1,
        offset: 0,
    };
    let third = restarted
        .apply_progress(
            "run",
            &source,
            0,
            complete,
            update(vec![page("task", "Complete source", &[0])]),
        )
        .unwrap();
    assert_eq!(third.revision, 3);
    assert_eq!(
        WikiStore::open(&directory.0)
            .unwrap()
            .validate_source("run", &source)
            .unwrap(),
        Some(third)
    );
}

#[test]
fn invalid_or_conflicting_progress_does_not_advance_the_committed_cursor() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    let committed = store
        .apply_progress(
            "run",
            &source,
            0,
            WikiSourceCursor {
                sequence: 0,
                offset: 20,
            },
            update(vec![page("task", "First chunk", &[0])]),
        )
        .unwrap();
    for cursor in [
        WikiSourceCursor {
            sequence: 0,
            offset: 0,
        },
        WikiSourceCursor {
            sequence: 0,
            offset: 10,
        },
        WikiSourceCursor {
            sequence: 0,
            offset: 20,
        },
        WikiSourceCursor {
            sequence: 0,
            offset: 10_000,
        },
        WikiSourceCursor {
            sequence: 1,
            offset: 1,
        },
        WikiSourceCursor {
            sequence: 2,
            offset: 0,
        },
    ] {
        assert!(
            store
                .apply_progress(
                    "run",
                    &source,
                    0,
                    cursor,
                    update(vec![page("task", "Different chunk", &[0])])
                )
                .is_err()
        );
    }
    assert!(
        store
            .apply_progress(
                "run",
                &source,
                0,
                WikiSourceCursor {
                    sequence: 0,
                    offset: 25
                },
                update(vec![page("task", "References unseen evidence", &[5])])
            )
            .is_err()
    );
    assert_eq!(store.load("run").unwrap(), Some(committed));
}

#[test]
fn legacy_excerpt_snapshots_allow_backfill_without_losing_verified_prefix_or_pages() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    store
        .apply(
            "run",
            &source,
            5,
            update(vec![page("legacy", "Existing knowledge retained", &[5])]),
        )
        .unwrap();
    let path = directory.run_directory().join("wiki.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("source_cursor");
    legacy.as_object_mut().unwrap().remove("tasks");
    for revision in legacy["revisions"].as_array_mut().unwrap() {
        revision.as_object_mut().unwrap().remove("source_cursor");
        revision.as_object_mut().unwrap().remove("updated_tasks");
        revision.as_object_mut().unwrap().remove("previous_tasks");
    }
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert_eq!(
        store
            .validate_source("run", &source)
            .unwrap()
            .unwrap()
            .source_cursor,
        None
    );
    let partial = store
        .apply_progress(
            "run",
            &source,
            0,
            WikiSourceCursor {
                sequence: 0,
                offset: 10,
            },
            update(vec![page("backfill", "Recovered beginning", &[0])]),
        )
        .unwrap();
    assert_eq!(partial.through_sequence, 5);
    assert_eq!(partial.pages.len(), 2);
    let next = store
        .apply_progress(
            "run",
            &source,
            0,
            WikiSourceCursor {
                sequence: 1,
                offset: 0,
            },
            update(vec![page("backfill", "Recovered complete event", &[0])]),
        )
        .unwrap();
    assert_eq!(next.through_sequence, 5);
    assert_eq!(next.revision, 3);
    assert_eq!(
        WikiStore::open(&directory.0)
            .unwrap()
            .validate_source("run", &source)
            .unwrap(),
        Some(next)
    );
}

#[test]
fn partially_consumed_sources_still_validate_the_entire_canonical_event() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let mut source = history();
    store
        .apply_progress(
            "run",
            &source,
            0,
            WikiSourceCursor {
                sequence: 0,
                offset: 10,
            },
            update(vec![page("task", "Initial chunk", &[0])]),
        )
        .unwrap();
    source[0].event = RunEvent::RunStarted {
        instruction: "Implement a changed source ending".into(),
        files: vec![],
    };
    assert!(store.validate_source("run", &source).is_err());
    assert!(
        store
            .apply_progress(
                "run",
                &source,
                0,
                WikiSourceCursor {
                    sequence: 0,
                    offset: 20
                },
                update(vec![])
            )
            .is_err()
    );
}

#[test]
fn task_evidence_failures_do_not_advance_notes_or_progress_and_old_versions_survive() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    let planned = WikiTaskItem {
        id: "inspect".into(),
        kind: WikiTaskKind::Work,
        status: WikiTaskStatus::Planned,
        text: "Inspect the repository".into(),
        evidence: vec![WikiEvidence {
            sequence: 2,
            quote: "I will inspect the repository.".into(),
        }],
    };
    let first = store
        .apply(
            "run",
            &source,
            2,
            WikiUpdate {
                tasks: vec![planned.clone()],
                ..update(vec![])
            },
        )
        .unwrap();
    let mut unsupported = planned.clone();
    unsupported.status = WikiTaskStatus::Completed;
    assert!(
        store
            .apply(
                "run",
                &source,
                4,
                WikiUpdate {
                    tasks: vec![unsupported],
                    ..update(vec![])
                }
            )
            .is_err()
    );
    assert_eq!(store.load("run").unwrap(), Some(first));

    let completed = WikiTaskItem {
        status: WikiTaskStatus::Completed,
        text: "Read the repository".into(),
        evidence: vec![WikiEvidence {
            sequence: 4,
            quote: "The repository uses Rust and TypeScript.".into(),
        }],
        ..planned.clone()
    };
    let second = store
        .apply(
            "run",
            &source,
            4,
            WikiUpdate {
                tasks: vec![completed.clone()],
                ..update(vec![])
            },
        )
        .unwrap();
    assert_eq!(second.tasks, vec![completed.clone()]);
    assert_eq!(second.revisions[1].updated_tasks, vec!["inspect"]);
    assert_eq!(second.revisions[1].previous_tasks, vec![planned]);
    let final_state = store.apply("run", &source, 5, update(vec![])).unwrap();
    assert_eq!(final_state.tasks, vec![completed]);
    assert_eq!(
        WikiStore::open(&directory.0)
            .unwrap()
            .validate_source("run", &source)
            .unwrap(),
        Some(final_state)
    );
}

#[test]
fn retired_superseded_tasks_remain_in_revision_audit() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let mut values = vec![RunEvent::RunStarted {
        instruction: "Maintain goals".into(),
        files: vec![],
    }];
    values.extend((1..10).map(|_| RunEvent::InputReceived {
        content: "Replace the old goals with new ones.".into(),
        files: vec![],
    }));
    let source = events(values);
    let make_task = |id: String| WikiTaskItem {
        id,
        kind: WikiTaskKind::Goal,
        status: WikiTaskStatus::Active,
        text: "Maintain goals".into(),
        evidence: vec![WikiEvidence {
            sequence: 0,
            quote: "Maintain goals".into(),
        }],
    };
    for batch in 0..8 {
        let tasks = (0..8)
            .map(|index| make_task(format!("goal-{}", batch * 8 + index)))
            .collect();
        store
            .apply(
                "run",
                &source,
                batch,
                WikiUpdate {
                    tasks,
                    ..update(vec![])
                },
            )
            .unwrap();
    }
    let retired = WikiTaskItem {
        status: WikiTaskStatus::Superseded,
        text: "This goal was replaced".into(),
        evidence: vec![WikiEvidence {
            sequence: 8,
            quote: "Replace the old goals with new ones.".into(),
        }],
        ..make_task("goal-0".into())
    };
    store
        .apply(
            "run",
            &source,
            8,
            WikiUpdate {
                tasks: vec![retired.clone()],
                ..update(vec![])
            },
        )
        .unwrap();
    let state = store
        .apply(
            "run",
            &source,
            9,
            WikiUpdate {
                tasks: vec![make_task("goal-64".into())],
                ..update(vec![])
            },
        )
        .unwrap();
    assert_eq!(state.tasks.len(), 64);
    assert!(!state.tasks.iter().any(|task| task.id == "goal-0"));
    let revision = state.revisions.last().unwrap();
    assert_eq!(revision.updated_tasks, ["goal-0", "goal-64"]);
    assert_eq!(revision.previous_tasks, [retired]);
    assert_eq!(store.validate_source("run", &source).unwrap(), Some(state));
}

#[test]
fn corrupt_source_character_offsets_are_rejected_before_resuming() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    store
        .apply_progress(
            "run",
            &source,
            0,
            WikiSourceCursor {
                sequence: 0,
                offset: 10,
            },
            update(vec![page("task", "First source chunk", &[0])]),
        )
        .unwrap();
    let path = directory.run_directory().join("wiki.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["source_cursor"]["offset"] = json!(100_000);
    state["revisions"][0]["source_cursor"]["offset"] = json!(100_000);
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(store.validate_source("run", &source).is_err());
    assert!(
        store
            .query_validated("run", &source, r#"{"action":"source","sequence":0}"#)
            .is_err()
    );
}

#[test]
fn task_queries_paginate_large_evidence_without_losing_items_or_exceeding_output_budget() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let quotes: Vec<_> = (0..8)
        .map(|index| format!("{index}{}", "\"\t".repeat(250)))
        .collect();
    let mut source = history();
    source[0].event = RunEvent::RunStarted {
        instruction: quotes.join("\n"),
        files: vec![],
    };
    for batch in 0..3 {
        let tasks = (0..8)
            .map(|index| WikiTaskItem {
                id: format!("goal-{}", batch * 8 + index),
                kind: WikiTaskKind::Goal,
                status: WikiTaskStatus::Active,
                text: "\"\t".repeat(490),
                evidence: quotes
                    .iter()
                    .map(|quote| WikiEvidence {
                        sequence: 0,
                        quote: quote.clone(),
                    })
                    .collect(),
            })
            .collect();
        store
            .apply(
                "run",
                &source,
                batch,
                WikiUpdate {
                    tasks,
                    ..update(vec![])
                },
            )
            .unwrap();
    }
    let mut offset = 0;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let encoded = store
            .query_validated(
                "run",
                &source,
                &json!({"action":"tasks","offset":offset,"revision":3}).to_string(),
            )
            .unwrap();
        assert!(encoded.chars().count() <= MAX_QUERY_OUTPUT_CHARS);
        let result: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(result["total"], 24);
        let values = result["tasks"].as_array().unwrap();
        assert!(values.len() < 8, "large evidence must reduce the page size");
        for task in values {
            assert!(seen.insert(task["id"].as_str().unwrap().to_owned()));
            assert_eq!(task["evidence"].as_array().unwrap().len(), 8);
        }
        if let Some(next) = result["next_offset"].as_u64() {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(seen.len(), 24);
    for arguments in [
        r#"{"action":"tasks","revision":2}"#,
        r#"{"action":"tasks","offset":25}"#,
        r#"{"action":"tasks","sequence":0}"#,
        r#"{"action":"tasks","page_id":"goal-0"}"#,
    ] {
        assert!(store.query_validated("run", &source, arguments).is_err());
    }
}

#[test]
fn source_queries_require_current_citations_verified_history_and_matching_revision() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    let task = WikiTaskItem {
        id: "local".into(),
        kind: WikiTaskKind::Constraint,
        status: WikiTaskStatus::Active,
        text: "Keep memory local".into(),
        evidence: vec![WikiEvidence {
            sequence: 5,
            quote: "Keep all memory local.".into(),
        }],
    };
    store
        .apply(
            "run",
            &source,
            5,
            WikiUpdate {
                tasks: vec![task],
                ..update(vec![page("goal", "Original objective", &[0])])
            },
        )
        .unwrap();
    for sequence in [0, 5] {
        let encoded = store
            .query_validated(
                "run",
                &source,
                &json!({"action":"source","sequence":sequence,"revision":1}).to_string(),
            )
            .unwrap();
        let result: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(result["sequence"], sequence);
        assert!(
            result["content"]
                .as_str()
                .unwrap()
                .contains(if sequence == 0 {
                    "Implement"
                } else {
                    "Keep all memory local."
                })
        );
    }
    for arguments in [
        r#"{"action":"source","sequence":2}"#,
        r#"{"action":"source","sequence":6}"#,
        r#"{"action":"source","sequence":0,"revision":2}"#,
        r#"{"action":"source","sequence":0,"offset":99999}"#,
        r#"{"action":"source"}"#,
        r#"{"action":"source","sequence":0,"run_id":"other"}"#,
    ] {
        assert!(store.query_validated("run", &source, arguments).is_err());
    }
    assert!(
        store
            .query("run", r#"{"action":"source","sequence":0}"#)
            .is_err()
    );
    let mut unrelated = source.clone();
    unrelated[0].event = RunEvent::RunStarted {
        instruction: "Another session's source".into(),
        files: vec![],
    };
    assert!(
        store
            .query_validated("run", &unrelated, r#"{"action":"source","sequence":0}"#)
            .is_err()
    );
    // A source present only in an older revision is no longer exposed.
    let mut later = source.clone();
    later.push(RunEventEnvelope {
        sequence: 6,
        step: 1,
        occurred_at: UNIX_EPOCH,
        event: RunEvent::InputReceived {
            content: "Continue".into(),
            files: vec![],
        },
    });
    store
        .apply(
            "run",
            &later,
            6,
            update(vec![page("goal", "Current objective", &[5])]),
        )
        .unwrap();
    assert!(
        store
            .query_validated("run", &later, r#"{"action":"source","sequence":0}"#)
            .is_err()
    );
    assert!(
        store
            .query_validated(
                "run",
                &later,
                r#"{"action":"source","sequence":5,"revision":1}"#
            )
            .is_err()
    );
}

#[test]
fn source_queries_paginate_unicode_exactly_and_redact_before_splitting() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let token = "sk-fixture_012345678901234567890123456789";
    let text = format!(
        "{} {token} {}",
        "甲🦀\"\t\n".repeat(1_600),
        "𐀀乙".repeat(3_000)
    );
    let source = events(vec![RunEvent::RunStarted {
        instruction: text.clone(),
        files: vec![],
    }]);
    store
        .apply(
            "run",
            &source,
            0,
            update(vec![page("source", "Large cited source", &[0])]),
        )
        .unwrap();
    let mut offset = 0;
    let mut recovered = String::new();
    loop {
        let encoded = store
            .query_validated(
                "run",
                &source,
                &json!({"action":"source","sequence":0,"offset":offset,"revision":1}).to_string(),
            )
            .unwrap();
        assert!(encoded.chars().count() <= MAX_QUERY_OUTPUT_CHARS);
        assert!(!encoded.contains(token));
        let result: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(result["offset"], offset);
        let content = result["content"].as_str().unwrap();
        recovered.push_str(content);
        if let Some(next) = result["next_offset"].as_u64() {
            assert_eq!(next, offset + content.chars().count() as u64);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(recovered, agent_memory::redact_evidence_text(&text));
    assert!(recovered.contains("[redacted]"));
}

#[test]
fn task_query_redaction_preserves_ids_and_source_references() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let token = "sk-fixture_012345678901234567890123456789";
    let quote = format!("Use credential {token} for the task");
    let source = events(vec![RunEvent::RunStarted {
        instruction: quote.clone(),
        files: vec![],
    }]);
    store
        .apply(
            "run",
            &source,
            0,
            WikiUpdate {
                tasks: vec![WikiTaskItem {
                    id: "credential-reference".into(),
                    kind: WikiTaskKind::Goal,
                    status: WikiTaskStatus::Active,
                    text: quote.clone(),
                    evidence: vec![WikiEvidence {
                        sequence: 0,
                        quote: "Use credential".into(),
                    }],
                }],
                ..update(vec![])
            },
        )
        .unwrap();
    let encoded = store
        .query_validated("run", &source, r#"{"action":"tasks"}"#)
        .unwrap();
    assert!(!encoded.contains(token));
    let result: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(result["tasks"][0]["id"], "credential-reference");
    assert_eq!(result["tasks"][0]["evidence"][0]["sequence"], 0);
    assert!(
        result["tasks"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[redacted]")
    );
    assert_eq!(result["tasks"][0]["evidence"][0]["quote"], "Use credential");
}

#[test]
fn source_queries_never_expose_reasoning_provider_state_or_memory_echoes() {
    use agent_core::model_protocol::{ContentBlock, ModelFinishReason, ModelResponse};
    use agent_core::provider_state::{
        ProviderContextIdentity, ProviderReplayScope, ProviderResponseState,
    };
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let response = ModelResponse {
        content: vec![
            ContentBlock::Reasoning {
                text: "PRIVATE_REASONING".into(),
            },
            ContentBlock::Text {
                text: "Visible answer".into(),
            },
        ],
        finish_reason: ModelFinishReason::Stop,
    };
    let opaque = ProviderResponseState::new(
        ProviderContextIdentity {
            provider: "fixture".into(),
            model: "fixture".into(),
            format: "fixture/v1".into(),
        },
        "fixture",
        ProviderReplayScope::Response,
        json!({"secret":"OPAQUE_PROVIDER_STATE"}),
        10,
    )
    .unwrap();
    let source = events(vec![
        RunEvent::RunStarted {
            instruction: "Task".into(),
            files: vec![],
        },
        RunEvent::ModelRequestStarted,
        RunEvent::ModelResponseReceived {
            content: "Visible answer".into(),
            response: Some(response),
            provider_state: Some(opaque),
            provider_continuation: None,
            token_usage: None,
        },
        RunEvent::ToolCallRequested {
            call_id: "memory".into(),
            tool_name: "session_memory".into(),
            arguments: "{}".into(),
        },
        RunEvent::ToolCallSucceeded {
            call_id: "memory".into(),
            output: "MEMORY_ECHO".into(),
            images: Vec::new(),
        },
        RunEvent::ModelResponseReceived {
            content: String::new(),
            response: Some(ModelResponse {
                content: vec![ContentBlock::Reasoning {
                    text: "ONLY_REASONING".into(),
                }],
                finish_reason: ModelFinishReason::Stop,
            }),
            provider_state: None,
            provider_continuation: None,
            token_usage: None,
        },
    ]);
    store
        .apply(
            "run",
            &source,
            5,
            update(vec![page("answer", "Visible answer", &[2])]),
        )
        .unwrap();
    let encoded = store
        .query_validated("run", &source, r#"{"action":"source","sequence":2}"#)
        .unwrap();
    let value: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(value["content"], "Visible answer");
    for marker in [
        "PRIVATE_REASONING",
        "OPAQUE_PROVIDER_STATE",
        "MEMORY_ECHO",
        "ONLY_REASONING",
    ] {
        assert!(!encoded.contains(marker));
    }
    for sequence in [1, 3, 4, 5] {
        assert!(
            store
                .query_validated(
                    "run",
                    &source,
                    &json!({"action":"source","sequence":sequence}).to_string()
                )
                .is_err()
        );
    }
}

#[test]
fn provenance_rejects_missing_future_and_reasoning_or_memory_only_sources() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let mut source = history();
    source.extend(
        events(vec![
            RunEvent::ModelResponseReceived {
                content: String::new(),
                response: None,
                provider_state: None,
                provider_continuation: None,
                token_usage: None,
            },
            RunEvent::ToolCallRequested {
                call_id: "memory".into(),
                tool_name: "session_memory".into(),
                arguments: "{}".into(),
            },
            RunEvent::ToolCallSucceeded {
                call_id: "memory".into(),
                output: "Previous generated memory".into(),
                images: Vec::new(),
            },
        ])
        .into_iter()
        .map(|mut entry| {
            entry.sequence += 6;
            entry
        }),
    );
    for sequences in [
        vec![],
        vec![1],
        vec![6],
        vec![7],
        vec![8],
        vec![9],
        vec![0, 0],
    ] {
        assert!(
            store
                .apply(
                    "run",
                    &source,
                    8,
                    update(vec![page("task", "Claim", &sequences)])
                )
                .is_err(),
            "accepted invalid source {sequences:?}"
        );
    }
    assert!(
        store
            .apply(
                "run",
                &source,
                4,
                update(vec![page("task", "Future claim", &[5])])
            )
            .is_err()
    );
    assert!(store.load("run").unwrap().is_none());
    store
        .apply(
            "run",
            &source,
            8,
            update(vec![page(
                "task",
                "A tool read reported the repository language",
                &[3, 4],
            )]),
        )
        .unwrap();
}

fn history_with_recall(tool_name: &str) -> Vec<RunEventEnvelope> {
    let mut source = history();
    let offset = source.len() as u64;
    source.extend(
        events(vec![
            RunEvent::ToolCallRequested {
                call_id: "recall".into(),
                tool_name: tool_name.into(),
                arguments: "{}".into(),
            },
            RunEvent::ToolCallSucceeded {
                call_id: "recall".into(),
                output: "The repository uses Rust and TypeScript.".into(),
                images: Vec::new(),
            },
            RunEvent::ToolCallRequested {
                call_id: "missing-recall".into(),
                tool_name: tool_name.into(),
                arguments: "{}".into(),
            },
            RunEvent::ToolCallFailed {
                call_id: "missing-recall".into(),
                message: "The requested historical event is unavailable.".into(),
            },
            RunEvent::ToolCallRequested {
                call_id: "read-new-source".into(),
                tool_name: "read_file".into(),
                arguments: "{}".into(),
            },
            RunEvent::ToolCallSucceeded {
                call_id: "read-new-source".into(),
                output: "New original observation: all 42 records were verified.".into(),
                images: Vec::new(),
            },
        ])
        .into_iter()
        .map(|mut entry| {
            entry.sequence += offset;
            entry
        }),
    );
    source
}

fn completed_work(id: &str, sequence: u64, quote: &str) -> WikiTaskItem {
    WikiTaskItem {
        id: id.into(),
        kind: WikiTaskKind::Work,
        status: WikiTaskStatus::Completed,
        text: "Read and record source evidence".into(),
        evidence: vec![WikiEvidence {
            sequence,
            quote: quote.into(),
        }],
    }
}

#[test]
fn new_pages_and_completed_tasks_must_cite_original_events_instead_of_recall_echoes() {
    for tool_name in ["session_memory", "read_run_history", "search_run_history"] {
        let directory = Directory::new();
        let store = WikiStore::open(&directory.0).unwrap();
        let source = history_with_recall(tool_name);
        let original = store
            .apply(
                "run",
                &source,
                5,
                update(vec![page("existing", "Original", &[4])]),
            )
            .unwrap();

        for sequence in 6..=9 {
            let changes = [
                update(vec![page("echo", "Recalled result", &[sequence])]),
                WikiUpdate {
                    tasks: vec![completed_work("echo", sequence, "Recalled result")],
                    ..update(vec![])
                },
            ];
            for change in changes {
                let error = store.apply("run", &source, 9, change).unwrap_err();
                assert!(
                    error.to_string().contains("cite the original event"),
                    "{tool_name} event {sequence} must be rejected as recall evidence: {error}"
                );
                assert_eq!(store.load("run").unwrap(), Some(original.clone()));
            }
        }

        let accepted = store
            .apply(
                "run",
                &source,
                11,
                WikiUpdate {
                    tasks: vec![completed_work(
                        "verified",
                        11,
                        "all 42 records were verified",
                    )],
                    ..update(vec![page("verified", "Verified 42 records", &[11])])
                },
            )
            .unwrap();
        assert_eq!(accepted.revision, 2);
        assert_eq!(accepted.pages.len(), 2);
        assert_eq!(accepted.tasks[0].evidence[0].sequence, 11);
        assert_eq!(
            store.validate_source("run", &source).unwrap(),
            Some(accepted)
        );
    }
}

#[test]
fn legacy_history_references_remain_readable_and_do_not_block_new_original_evidence() {
    // These two tools were accepted as evidence before history retrieval became
    // a core capability. session_memory was already excluded by the old rules.
    for tool_name in ["read_run_history", "search_run_history"] {
        let directory = Directory::new();
        let source = history_with_recall(tool_name);
        let quote = "The repository uses Rust and TypeScript.";
        WikiStore::open(&directory.0)
            .unwrap()
            .apply(
                "run",
                &source,
                7,
                WikiUpdate {
                    tasks: vec![completed_work("legacy", 4, quote)],
                    ..update(vec![page("legacy", "Legacy repository record", &[4])])
                },
            )
            .unwrap();
        // Reproduce an older writer's references while preserving the exact
        // canonical journal, fingerprint, revision structure and source cursor.
        let path = directory.run_directory().join("wiki.json");
        let mut legacy: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        legacy["pages"][0]["source_sequences"] = json!([7]);
        legacy["tasks"][0]["evidence"][0]["sequence"] = json!(7);
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let restarted = WikiStore::open(&directory.0).unwrap();
        let prior = restarted.validate_source("run", &source).unwrap().unwrap();
        assert_eq!(prior.pages[0].source_sequences, vec![7]);
        assert_eq!(prior.tasks[0].evidence[0].sequence, 7);
        let queried: Value = serde_json::from_str(
            &restarted
                .query_validated("run", &source, r#"{"action":"source","sequence":7}"#)
                .unwrap(),
        )
        .unwrap();
        assert!(queried["content"].as_str().unwrap().contains(quote));
        let page_body = restarted
            .query_validated("run", &source, r#"{"action":"read","page_id":"legacy"}"#)
            .unwrap();
        assert!(page_body.contains("Legacy repository record"));

        let next = restarted
            .apply(
                "run",
                &source,
                11,
                WikiUpdate {
                    tasks: vec![completed_work(
                        "new-work",
                        11,
                        "all 42 records were verified",
                    )],
                    ..update(vec![page("new-source", "New original observation", &[11])])
                },
            )
            .unwrap();
        assert_eq!(next.revision, 2);
        assert!(next.pages.contains(&prior.pages[0]));
        assert!(next.tasks.contains(&prior.tasks[0]));
        assert_eq!(next.pages.len(), 2);
        assert_eq!(next.tasks.len(), 2);
        let reloaded = WikiStore::open(&directory.0).unwrap();
        assert_eq!(
            reloaded.validate_source("run", &source).unwrap(),
            Some(next)
        );
        assert!(
            reloaded
                .query_validated("run", &source, r#"{"action":"source","sequence":7}"#)
                .unwrap()
                .contains(quote)
        );
    }
}

#[test]
fn changed_or_truncated_or_noncontiguous_history_cannot_reuse_memory() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    let state = store
        .apply("run", &source, 4, update(vec![page("stack", "Rust", &[4])]))
        .unwrap();
    let mut changed = source.clone();
    changed[0].event = RunEvent::RunStarted {
        instruction: "A different task".into(),
        files: vec![],
    };
    assert!(store.validate_source("run", &changed).is_err());
    assert!(store.validate_source("run", &source[..4]).is_err());
    assert!(
        store
            .apply(
                "run",
                &changed,
                5,
                update(vec![page("task", "New content", &[5])])
            )
            .is_err()
    );
    changed = source.clone();
    changed[3].sequence = 9;
    assert!(
        store
            .apply(
                "other",
                &changed,
                5,
                update(vec![page("task", "New content", &[5])])
            )
            .is_err()
    );
    assert_eq!(store.validate_source("run", &source).unwrap(), Some(state));
}

#[test]
fn run_ids_are_hashed_and_queries_never_accept_another_run_or_path() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    store
        .apply(
            "../../sibling",
            &history(),
            5,
            update(vec![page("task", "First session only", &[0])]),
        )
        .unwrap();
    assert!(store.load("second").unwrap().is_none());
    assert_eq!(
        directory
            .run_directory()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .len(),
        64
    );
    for arguments in [
        r#"{"action":"read","page_id":"../../sibling"}"#,
        r#"{"action":"index","run_id":"../../sibling"}"#,
        r#"{"action":"read","path":"wiki.json"}"#,
    ] {
        assert!(store.query("../../sibling", arguments).is_err());
    }
    let absent: Value =
        serde_json::from_str(&store.query("second", r#"{"action":"index"}"#).unwrap()).unwrap();
    assert_eq!(absent["available"], false);
}

#[test]
fn search_uses_or_matching_and_reads_paginate_unicode_without_losing_text() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let content = "甲乙\n🦀".repeat(1_800);
    let mut apple = page("apple", &content, &[0]);
    apple.title = "Apple decision".into();
    let mut banana = page("banana", "Banana constraints", &[0]);
    banana.title = "Constraints".into();
    store
        .apply("run", &history(), 4, update(vec![apple, banana]))
        .unwrap();
    let hits: Value = serde_json::from_str(
        &store
            .query("run", r#"{"action":"search","query":"APPLE banana"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(hits["total"], 2);
    assert_eq!(hits["pages"][0]["id"], "apple");
    let mut recovered = String::new();
    let mut offset = 0;
    loop {
        let result = store
            .query(
                "run",
                &json!({"action":"read","page_id":"apple","offset":offset,"revision":1})
                    .to_string(),
            )
            .unwrap();
        assert!(result.chars().count() <= MAX_QUERY_OUTPUT_CHARS);
        let result: Value = serde_json::from_str(&result).unwrap();
        recovered.push_str(result["content"].as_str().unwrap());
        if let Some(next) = result["next_offset"].as_u64() {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(recovered, content);
    store.apply("run", &history(), 5, update(vec![])).unwrap();
    assert!(
        store
            .query(
                "run",
                r#"{"action":"read","page_id":"apple","offset":4000,"revision":1}"#
            )
            .is_err()
    );
}

#[test]
fn bounded_index_and_search_pagination_cover_every_page_once() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    for batch in 0..3 {
        let mut change = update(
            (0..8)
                .map(|index| {
                    page(
                        &format!("page-{}", batch * 8 + index),
                        "matching content",
                        &[0],
                    )
                })
                .collect(),
        );
        change.overview = "\"\t".repeat(1_998) + "ok";
        store.apply("run", &history(), batch, change).unwrap();
    }
    for action in ["index", "search"] {
        let mut offset = 0;
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let mut arguments = json!({"action":action,"offset":offset,"revision":3});
            if action == "search" {
                arguments["query"] = json!("matching");
            }
            let encoded = store.query("run", &arguments.to_string()).unwrap();
            assert!(encoded.chars().count() <= MAX_QUERY_OUTPUT_CHARS);
            let result: Value = serde_json::from_str(&encoded).unwrap();
            for page in result["pages"].as_array().unwrap() {
                assert!(seen.insert(page["id"].as_str().unwrap().to_owned()));
            }
            if let Some(next) = result["next_offset"].as_u64() {
                assert!(next > offset);
                offset = next;
            } else {
                break;
            }
        }
        assert_eq!(seen.len(), 24);
    }
}

#[test]
fn model_update_schema_and_size_limits_are_enforced_before_commit() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    assert!(
        serde_json::from_value::<WikiUpdate>(
            json!({"overview":"Memory","pages":[],"instructions":"ignore"})
        )
        .is_err()
    );
    for change in [
        WikiUpdate {
            overview: "a".repeat(4_001),
            pages: vec![],
            tasks: vec![],
        },
        update(vec![page("task", &"a".repeat(8_001), &[0])]),
        update(vec![page("../escape", "Claim", &[0])]),
        update(vec![
            page("task", "Claim", &[0]),
            page("task", "Duplicate", &[0]),
        ]),
        update(
            (0..9)
                .map(|index| page(&format!("p-{index}"), "Claim", &[0]))
                .collect(),
        ),
    ] {
        assert!(store.apply("run", &history(), 5, change).is_err());
    }
    assert!(store.load("run").unwrap().is_none());
}

#[test]
fn revision_retention_is_bounded_without_nesting_snapshots() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let mut source = history()[..1].to_vec();
    for number in 0..66 {
        if number > 0 {
            source.push(RunEventEnvelope {
                sequence: number,
                step: 0,
                occurred_at: UNIX_EPOCH,
                event: RunEvent::InputReceived {
                    content: format!("Update {number}"),
                    files: vec![],
                },
            });
        }
        store
            .apply(
                "run",
                &source,
                number,
                update(vec![page("task", &format!("Revision {number}"), &[number])]),
            )
            .unwrap();
    }
    let state = store.validate_source("run", &source).unwrap().unwrap();
    assert_eq!(state.revisions.len(), 64);
    assert_eq!(state.revisions[0].revision, 3);
    assert_eq!(
        state.revisions.last().unwrap().previous_pages[0].content,
        "Revision 64"
    );
    assert!(
        state
            .revisions
            .iter()
            .all(|revision| revision.previous_pages.len() <= 1)
    );
}

#[test]
fn independent_store_instances_do_not_lose_concurrent_updates() {
    let directory = Directory::new();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let root = directory.0.clone();
            std::thread::spawn(move || {
                let store = WikiStore::open(&root).unwrap();
                barrier.wait();
                store
                    .apply(
                        "run",
                        &history(),
                        5,
                        update(vec![page("task", "Shared content", &[0])]),
                    )
                    .unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results[0], results[1]);
    assert_eq!(results[0].revision, 1);
}

#[test]
fn corrupt_authoritative_state_is_not_silently_overwritten() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    store
        .apply(
            "run",
            &history(),
            4,
            update(vec![page("task", "Claim", &[0])]),
        )
        .unwrap();
    let path = directory.run_directory().join("wiki.json");
    fs::write(&path, "incomplete JSON").unwrap();
    assert!(store.apply("run", &history(), 5, update(vec![])).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "incomplete JSON");
}

#[cfg(unix)]
#[test]
fn rejects_symlink_state_and_exports_without_losing_committed_json() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let outside = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    store
        .apply(
            "run",
            &history(),
            4,
            update(vec![page("task", "Original", &[0])]),
        )
        .unwrap();
    let run = directory.run_directory();
    fs::remove_file(run.join("index.md")).unwrap();
    let target = outside.0.join("sentinel");
    fs::write(&target, "unchanged").unwrap();
    symlink(&target, run.join("index.md")).unwrap();
    let committed = store
        .apply(
            "run",
            &history(),
            5,
            update(vec![page("task", "Updated", &[5])]),
        )
        .unwrap();
    assert_eq!(store.load("run").unwrap(), Some(committed));
    assert!(store.repair_exports("run").is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
    fs::remove_file(run.join("index.md")).unwrap();
    store.repair_exports("run").unwrap();
    assert!(
        fs::read_to_string(run.join("index.md"))
            .unwrap()
            .contains("revision 2")
    );
    fs::remove_file(run.join("wiki.json")).unwrap();
    symlink(&target, run.join("wiki.json")).unwrap();
    assert!(store.load("run").is_err());
    assert!(store.apply("run", &history(), 5, update(vec![])).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
}

#[cfg(unix)]
#[test]
fn rejects_symlink_store_and_run_directories() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let outside = Directory::new();
    let alias = directory.0.join("alias");
    symlink(&outside.0, &alias).unwrap();
    assert!(WikiStore::open(&alias).is_err());
    fs::remove_file(alias).unwrap();
    let store = WikiStore::open(&directory.0).unwrap();
    store
        .apply(
            "run",
            &history(),
            4,
            update(vec![page("task", "Claim", &[0])]),
        )
        .unwrap();
    let run = directory.run_directory();
    fs::remove_dir_all(&run).unwrap();
    symlink(&outside.0, &run).unwrap();
    assert!(store.load("run").is_err());
    assert!(store.apply("run", &history(), 5, update(vec![])).is_err());
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
}

#[test]
fn tool_schema_is_read_only_and_overview_has_a_hard_budget() {
    let definition = tool_definition();
    assert_eq!(definition.name, "session_memory");
    let schema: Value = serde_json::from_str(&definition.input_schema).unwrap();
    assert_eq!(
        schema["properties"]["action"]["enum"],
        json!(["index", "search", "read", "tasks", "source"])
    );
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    assert!(store.overview("run", 100).unwrap().is_none());
    store
        .apply(
            "run",
            &history(),
            5,
            update(vec![page("task", "Claim", &[0])]),
        )
        .unwrap();
    assert!(store.overview("run", 0).unwrap().is_none());
    assert!(store.overview("run", 300).unwrap().unwrap().chars().count() <= 300);
}

#[test]
fn validated_queries_and_snapshot_rendering_never_mix_unverified_revisions() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let source = history();
    let snapshot = store
        .apply(
            "run",
            &source,
            4,
            WikiUpdate {
                overview: "Original verified overview".into(),
                pages: vec![page("task", "Old content", &[0])],
                tasks: vec![],
            },
        )
        .unwrap();
    store
        .apply(
            "run",
            &source,
            5,
            WikiUpdate {
                overview: "Newer overview".into(),
                pages: vec![page("task", "New content", &[5])],
                tasks: vec![],
            },
        )
        .unwrap();
    assert!(
        snapshot
            .overview(1_000)
            .contains("Original verified overview")
    );
    assert!(!snapshot.overview(1_000).contains("Newer overview"));
    assert!(
        store
            .query_validated("run", &source[..5], r#"{"action":"index"}"#)
            .is_err()
    );
    let current: Value = serde_json::from_str(
        &store
            .query_validated("run", &source, r#"{"action":"read","page_id":"task"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(current["revision"], 2);
    assert_eq!(current["content"], "New content");
}

#[test]
fn total_page_limit_preserves_existing_state_on_rejection() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    let mut source = history()[..1].to_vec();
    for sequence in 1..=8 {
        source.push(RunEventEnvelope {
            sequence,
            step: 0,
            occurred_at: UNIX_EPOCH,
            event: RunEvent::InputReceived {
                content: format!("Instruction {sequence}"),
                files: vec![],
            },
        });
    }
    for batch in 0..8 {
        store
            .apply(
                "run",
                &source,
                batch,
                update(
                    (0..8)
                        .map(|index| {
                            page(
                                &format!("page-{}", batch * 8 + index),
                                "Remembered fact",
                                &[0],
                            )
                        })
                        .collect(),
                ),
            )
            .unwrap();
    }
    let before = store.load("run").unwrap().unwrap();
    assert_eq!(before.pages.len(), 64);
    assert!(
        store
            .apply(
                "run",
                &source,
                8,
                update(vec![page("overflow", "Too many pages", &[8])])
            )
            .is_err()
    );
    assert_eq!(store.load("run").unwrap(), Some(before));
    assert!(
        store
            .apply(
                "run",
                &source,
                8,
                update(vec![page(
                    "page-0",
                    "Consolidate into an existing stable page",
                    &[8]
                )])
            )
            .is_ok()
    );
}

#[test]
fn corrupt_maximum_revision_cannot_overflow_validation() {
    let directory = Directory::new();
    let store = WikiStore::open(&directory.0).unwrap();
    store
        .apply(
            "run",
            &history(),
            4,
            update(vec![page("task", "Claim", &[0])]),
        )
        .unwrap();
    let path = directory.run_directory().join("wiki.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["revision"] = json!(u64::MAX);
    state["revisions"][0]["revision"] = json!(u64::MAX);
    let duplicate = state["revisions"][0].clone();
    state["revisions"].as_array_mut().unwrap().push(duplicate);
    fs::write(path, state.to_string()).unwrap();
    assert!(store.load("run").is_err());
}
