#[path = "support/history.rs"]
mod history;
use agent_core::agent::{AgentRun, RunEvent, RunEventEnvelope};
use agent_runtime::{
    AppendOutcome, EventStore, EventStoreError, EventStoreHistoryAccess, HistoryAccess,
    HistoryContext, InMemoryEventStore, NoHistoryAccess, READ_RUN_HISTORY_TOOL,
    SEARCH_RUN_HISTORY_TOOL, StoredRun, ToggleHistoryAccess, ToolCall,
};
use history::persist_run;
use std::{ops::Range, sync::Mutex};

#[test]
fn no_history_access_exposes_no_model_tools() {
    let access = NoHistoryAccess;

    assert!(HistoryAccess::<InMemoryEventStore>::definitions(&access).is_empty());
    assert!(!HistoryAccess::<InMemoryEventStore>::owns_tool(
        &access,
        SEARCH_RUN_HISTORY_TOOL
    ));
    assert!(!HistoryAccess::<InMemoryEventStore>::owns_tool(
        &access,
        READ_RUN_HISTORY_TOOL
    ));
}

#[test]
fn a_disabled_history_toggle_behaves_like_no_history_access() {
    let noop = NoHistoryAccess;
    let mut toggle = ToggleHistoryAccess::disabled(EventStoreHistoryAccess::new());

    assert_eq!(
        HistoryAccess::<InMemoryEventStore>::definitions(&toggle),
        HistoryAccess::<InMemoryEventStore>::definitions(&noop)
    );
    assert_eq!(
        HistoryAccess::<InMemoryEventStore>::owns_tool(&toggle, SEARCH_RUN_HISTORY_TOOL),
        HistoryAccess::<InMemoryEventStore>::owns_tool(&noop, SEARCH_RUN_HISTORY_TOOL)
    );

    toggle.set_enabled(true);
    assert!(HistoryAccess::<InMemoryEventStore>::owns_tool(
        &toggle,
        SEARCH_RUN_HISTORY_TOOL
    ));
    assert_eq!(
        HistoryAccess::<InMemoryEventStore>::definitions(&toggle).len(),
        2
    );
}

#[tokio::test]
async fn event_store_history_access_is_bounded_by_the_checkpoint_cutoff() {
    let store = InMemoryEventStore::new();
    let run = historical_run();
    persist_run(&store, &run).await;
    let access = EventStoreHistoryAccess::new();
    let context = HistoryContext {
        run_id: run.id().to_owned(),
        through_sequence: 4,
        checkpoint_id: "checkpoint-1".to_owned(),
    };

    let search = access
        .execute(
            &store,
            context.clone(),
            ToolCall {
                id: "search".to_owned(),
                name: SEARCH_RUN_HISTORY_TOOL.to_owned(),
                arguments: r#"{"query":"historical-needle","limit":2}"#.to_owned(),
            },
        )
        .await
        .unwrap();
    let search: serde_json::Value = serde_json::from_str(&search).unwrap();
    assert_eq!(search["checkpoint_id"], "checkpoint-1");
    assert_eq!(search["through_sequence"], 4);
    assert_eq!(search["total_matches"], 1);
    assert_eq!(search["hits"][0]["sequence"], 0);

    let hidden = access
        .execute(
            &store,
            context.clone(),
            ToolCall {
                id: "hidden-search".to_owned(),
                name: SEARCH_RUN_HISTORY_TOOL.to_owned(),
                arguments: r#"{"query":"newest-secret"}"#.to_owned(),
            },
        )
        .await
        .unwrap();
    let hidden: serde_json::Value = serde_json::from_str(&hidden).unwrap();
    assert_eq!(hidden["total_matches"], 0);
    assert_eq!(hidden["hits"], serde_json::json!([]));

    let readable = access
        .execute(
            &store,
            context.clone(),
            ToolCall {
                id: "read".to_owned(),
                name: READ_RUN_HISTORY_TOOL.to_owned(),
                arguments: r#"{"from_sequence":0,"to_sequence":4}"#.to_owned(),
            },
        )
        .await
        .unwrap();
    let readable: serde_json::Value = serde_json::from_str(&readable).unwrap();
    assert_eq!(readable["events"].as_array().unwrap().len(), 5);
    assert_eq!(readable["events"][4]["sequence"], 4);

    let error = access
        .execute(
            &store,
            context,
            ToolCall {
                id: "past-cutoff".to_owned(),
                name: READ_RUN_HISTORY_TOOL.to_owned(),
                arguments: r#"{"from_sequence":5,"to_sequence":5}"#.to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("past readable checkpoint"));
}

#[tokio::test]
async fn history_search_matches_original_text_without_json_escaping() {
    let store = InMemoryEventStore::new();
    let mut run = AgentRun::new(
        "search-run-metadata",
        "search-agent-metadata",
        "Keep \"strict mode\" enabled.",
    );
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Inspect the archived files.".to_owned(),
    })
    .unwrap();
    for (call_id, arguments) in [
        ("successful-call", r#"{"path":"old.txt"}"#),
        ("failed-call", "{}"),
    ] {
        run.record(RunEvent::ToolCallRequested {
            call_id: call_id.to_owned(),
            tool_name: "inspect_archive".to_owned(),
            arguments: arguments.to_owned(),
        })
        .unwrap();
    }
    let output = "missing \"token\" in C:\\repo\\result.json\nsecond line";
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "successful-call".to_owned(),
        output: output.to_owned(),
    })
    .unwrap();
    let success_sequence = run.events().last().unwrap().sequence;
    run.record(RunEvent::ToolCallFailed {
        call_id: "failed-call".to_owned(),
        message: "access \"denied\"".to_owned(),
    })
    .unwrap();
    let failure_sequence = run.events().last().unwrap().sequence;
    persist_run(&store, &run).await;
    let access = EventStoreHistoryAccess::new();
    let context = HistoryContext {
        run_id: run.id().to_owned(),
        through_sequence: failure_sequence,
        checkpoint_id: "search-checkpoint-metadata".to_owned(),
    };

    for (query, expected_sequence) in [
        ("missing \"token\"", success_sequence),
        (r"C:\repo\result.json", success_sequence),
        ("result.json\nsecond line", success_sequence),
        ("SECOND LINE", success_sequence),
        ("access \"denied\"", failure_sequence),
        ("\"strict mode\"", 0),
        ("ARCHIVED FILES", 2),
        (r#"{"path":"old.txt"}"#, 3),
    ] {
        let encoded = access
            .execute(
                &store,
                context.clone(),
                ToolCall {
                    id: "literal-search".to_owned(),
                    name: SEARCH_RUN_HISTORY_TOOL.to_owned(),
                    arguments: serde_json::json!({ "query": query }).to_string(),
                },
            )
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(response["total_matches"], 1, "query: {query:?}");
        assert_eq!(
            response["hits"][0]["sequence"], expected_sequence,
            "query: {query:?}"
        );
        if expected_sequence == success_sequence {
            let preview: serde_json::Value =
                serde_json::from_str(response["hits"][0]["preview"].as_str().unwrap()).unwrap();
            assert_eq!(preview["output"], output);
        }
    }

    for (query, expected_matches) in [
        ("successful-call", 2),
        ("inspect_archive", 2),
        ("tool_call_requested", 2),
        ("search-run-metadata", 0),
        ("search-agent-metadata", 0),
        ("search-checkpoint-metadata", 0),
        (r#""call_id":"#, 0),
        ("absent evidence", 0),
    ] {
        let encoded = access
            .execute(
                &store,
                context.clone(),
                ToolCall {
                    id: "field-search".to_owned(),
                    name: SEARCH_RUN_HISTORY_TOOL.to_owned(),
                    arguments: serde_json::json!({ "query": query }).to_string(),
                },
            )
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            response["total_matches"], expected_matches,
            "query: {query:?}"
        );
    }
}

#[tokio::test]
async fn oversized_history_events_can_be_reassembled_from_bounded_chunks() {
    let store = InMemoryEventStore::new();
    let large_output = format!("start-{}-finish", "细节".repeat(1_000));
    let mut run = AgentRun::new("chunked-history", "context-agent", "read a large result");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Read the large result.".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: "large-output".to_owned(),
        tool_name: "read_file".to_owned(),
        arguments: "{}".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "large-output".to_owned(),
        output: large_output.clone(),
    })
    .unwrap();
    let sequence = run.events().last().unwrap().sequence;
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "the checkpoint advanced after the first history chunk".to_owned(),
    })
    .unwrap();
    let advanced_through = run.events().last().unwrap().sequence;
    persist_run(&store, &run).await;

    let access = EventStoreHistoryAccess::with_limits(
        agent_runtime::HistoryLimits::builder()
            .max_results(8)
            .max_events(20)
            .max_output_chars(400)
            .preview_chars(100)
            .build()
            .unwrap(),
    );
    let mut context = HistoryContext {
        run_id: run.id().to_owned(),
        through_sequence: sequence,
        checkpoint_id: "chunk-checkpoint".to_owned(),
    };
    let mut offset = None;
    let mut reconstructed = String::new();

    for chunk_index in 0..100 {
        let arguments = offset.map_or_else(
            || format!(r#"{{"from_sequence":{sequence},"to_sequence":{sequence}}}"#),
            |offset| {
                format!(
                    r#"{{"from_sequence":{sequence},"to_sequence":{sequence},"offset":{offset}}}"#
                )
            },
        );
        let encoded = access
            .execute(
                &store,
                context.clone(),
                ToolCall {
                    id: "chunk-read".to_owned(),
                    name: READ_RUN_HISTORY_TOOL.to_owned(),
                    arguments,
                },
            )
            .await
            .unwrap();
        assert!(encoded.chars().count() <= 400);
        let envelope: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(envelope["payload_format"], "history_read_payload_v1");
        reconstructed.push_str(envelope["chunk"].as_str().unwrap());

        if envelope["complete"].as_bool().unwrap() {
            offset = None;
            break;
        }
        offset = Some(envelope["next_offset"].as_u64().unwrap() as usize);
        if chunk_index == 0 {
            context.through_sequence = advanced_through;
            context.checkpoint_id = "newer-checkpoint".to_owned();
        }
    }

    assert!(
        offset.is_none(),
        "the response did not finish within 100 chunks"
    );
    let response: serde_json::Value = serde_json::from_str(&reconstructed).unwrap();
    assert_eq!(response["events"][0]["event"]["output"], large_output);
}

#[tokio::test]
async fn a_history_budget_that_cannot_hold_an_envelope_returns_an_explicit_error() {
    let store = InMemoryEventStore::new();
    let run = historical_run();
    persist_run(&store, &run).await;
    let access = EventStoreHistoryAccess::new();
    for budget in [0, 1] {
        let error = access
            .execute_bounded(
                &store,
                HistoryContext {
                    run_id: run.id().to_owned(),
                    through_sequence: 4,
                    checkpoint_id: "checkpoint".to_owned(),
                },
                ToolCall {
                    id: "tiny-budget".to_owned(),
                    name: READ_RUN_HISTORY_TOOL.to_owned(),
                    arguments: r#"{"from_sequence":0}"#.to_owned(),
                },
                Some(budget),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            agent_runtime::HistoryAccessError::OutputTooLarge { configured, .. }
                if configured == budget
        ));
    }
}

struct RangeObservedStore {
    inner: InMemoryEventStore,
    ranges: Mutex<Vec<Range<u64>>>,
    call_lookups: Mutex<Vec<Vec<String>>>,
}

impl EventStore for RangeObservedStore {
    async fn append(
        &self,
        run_id: &str,
        agent_id: &str,
        event: &RunEventEnvelope,
    ) -> Result<AppendOutcome, EventStoreError> {
        self.inner.append(run_id, agent_id, event).await
    }

    async fn load(&self, _run_id: &str) -> Result<Option<StoredRun>, EventStoreError> {
        panic!("a bounded history read must not load the entire run")
    }

    async fn load_range(
        &self,
        run_id: &str,
        sequences: Range<u64>,
    ) -> Result<Option<StoredRun>, EventStoreError> {
        self.ranges.lock().unwrap().push(sequences.clone());
        self.inner.load_range(run_id, sequences).await
    }

    async fn load_tool_calls(
        &self,
        run_id: &str,
        call_ids: &[String],
    ) -> Result<Vec<RunEventEnvelope>, EventStoreError> {
        self.call_lookups.lock().unwrap().push(call_ids.to_vec());
        self.inner.load_tool_calls(run_id, call_ids).await
    }
}

#[tokio::test]
async fn excluded_derived_memory_is_not_searchable_or_readable_as_original_evidence() {
    let store = InMemoryEventStore::new();
    let mut run = AgentRun::new("derived-memory", "agent", "Original user requirement");
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Look up past evidence.".into(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: "wiki".into(),
        tool_name: "session_memory".into(),
        arguments: r#"{"query":"derived-only-marker"}"#.into(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "wiki".into(),
        output: "derived-only-marker is a generated Wiki claim".into(),
    })
    .unwrap();
    persist_run(&store, &run).await;
    let context = HistoryContext {
        run_id: run.id().into(),
        through_sequence: run.events().last().unwrap().sequence,
        checkpoint_id: "derived-memory-checkpoint".into(),
    };
    let call = |name: &str, arguments: &str| ToolCall {
        id: "lookup".into(),
        name: name.into(),
        arguments: arguments.into(),
    };
    let query = r#"{"query":"derived-only-marker"}"#;
    let default_output = EventStoreHistoryAccess::new()
        .execute(
            &store,
            context.clone(),
            call(SEARCH_RUN_HISTORY_TOOL, query),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&default_output).unwrap()["total_matches"],
        2
    );

    let access = EventStoreHistoryAccess::new().with_excluded_tools(["session_memory".to_owned()]);
    // Exclusion is a projection policy, not a third tool or an ownership grant.
    assert_eq!(
        HistoryAccess::<InMemoryEventStore>::definitions(&access).len(),
        2
    );
    assert!(!HistoryAccess::<InMemoryEventStore>::owns_tool(
        &access,
        "session_memory"
    ));
    let search = access
        .execute(
            &store,
            context.clone(),
            call(SEARCH_RUN_HISTORY_TOOL, query),
        )
        .await
        .unwrap();
    let search: serde_json::Value = serde_json::from_str(&search).unwrap();
    assert_eq!(search["total_matches"], 0);
    assert_eq!(search["hits"], serde_json::json!([]));
    let read = access
        .execute(
            &store,
            context.clone(),
            call(
                READ_RUN_HISTORY_TOOL,
                &format!(
                    r#"{{"from_sequence":0,"to_sequence":{}}}"#,
                    context.through_sequence
                ),
            ),
        )
        .await
        .unwrap();
    assert!(read.contains("Original user requirement"));
    assert!(!read.contains("derived-only-marker"));
    assert!(!read.contains("session_memory"));
    assert_eq!(
        store.load(run.id()).await.unwrap().unwrap().events,
        run.events()
    );
}

#[tokio::test]
async fn a_narrow_history_read_looks_up_requests_to_filter_history_results() {
    let inner = InMemoryEventStore::new();
    let mut run = AgentRun::new(
        "narrow-read",
        "context-agent",
        "Read exact historical evidence.",
    );
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "Read current and archived evidence.".to_owned(),
    })
    .unwrap();
    for (call_id, tool_name) in [
        ("regular", "read_file"),
        ("history", READ_RUN_HISTORY_TOOL),
        ("wiki", "session_memory"),
    ] {
        run.record(RunEvent::ToolCallRequested {
            call_id: call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            arguments: "{}".to_owned(),
        })
        .unwrap();
    }
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "regular".to_owned(),
        output: "Original evidence".to_owned(),
    })
    .unwrap();
    let first_result = run.events().last().unwrap().sequence;
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "history".to_owned(),
        output: "An old history response must not appear recursively".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallFailed {
        call_id: "wiki".to_owned(),
        message: "A derived memory lookup failed; this is not original evidence".to_owned(),
    })
    .unwrap();
    let last_result = run.events().last().unwrap().sequence;
    persist_run(&inner, &run).await;
    let store = RangeObservedStore {
        inner,
        ranges: Mutex::new(Vec::new()),
        call_lookups: Mutex::new(Vec::new()),
    };

    let output = EventStoreHistoryAccess::new()
        .with_excluded_tools(["session_memory".to_owned()])
        .execute(
            &store,
            HistoryContext {
                run_id: run.id().to_owned(),
                through_sequence: last_result,
                checkpoint_id: "narrow-checkpoint".to_owned(),
            },
            ToolCall {
                id: "narrow-read".to_owned(),
                name: READ_RUN_HISTORY_TOOL.to_owned(),
                arguments: format!(
                    r#"{{"from_sequence":{first_result},"to_sequence":{last_result}}}"#
                ),
            },
        )
        .await
        .unwrap();

    let response: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(response["events"].as_array().unwrap().len(), 1);
    assert_eq!(response["events"][0]["sequence"], first_result);
    assert_eq!(
        response["events"][0]["event"]["output"],
        "Original evidence"
    );
    let ranges = store.ranges.lock().unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0], first_result..last_result + 1);
    assert_eq!(
        *store.call_lookups.lock().unwrap(),
        [vec![
            "regular".to_owned(),
            "history".to_owned(),
            "wiki".to_owned()
        ]]
    );
}

fn historical_run() -> AgentRun {
    let mut run = AgentRun::new(
        "history-run",
        "context-agent",
        "historical-needle was supplied at the beginning",
    );
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "I will inspect the archived state.".to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: "regular-call".to_owned(),
        tool_name: "read_file".to_owned(),
        arguments: r#"{"path":"old.txt"}"#.to_owned(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "regular-call".to_owned(),
        output: "archived contents".to_owned(),
    })
    .unwrap();
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        response: None,
        provider_continuation: None,
        token_usage: None,
        provider_state: None,
        content: "newest-secret must remain past the checkpoint".to_owned(),
    })
    .unwrap();
    run
}
