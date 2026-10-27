use std::time::SystemTime;

use agent_core::{
    AgentMessage, TextFileReference,
    agent::{RunEvent, RunEventEnvelope},
};
use agent_runtime::{
    ContentBlock, EventStore, EventStoreHistoryAccess, HistoryAccess, HistoryAccessError,
    HistoryContext, HistoryLimits, InMemoryEventStore, ModelResponse, READ_RUN_HISTORY_TOOL,
    SEARCH_RUN_HISTORY_TOOL, ToolCall, ToolOutputProjector,
};
use serde_json::{Value, json};

async fn store_events(events: Vec<RunEvent>) -> (InMemoryEventStore, HistoryContext) {
    let store = InMemoryEventStore::new();
    let through_sequence = events.len() as u64 - 1;
    for (sequence, event) in events.into_iter().enumerate() {
        store
            .append(
                "history",
                "agent",
                &RunEventEnvelope {
                    sequence: sequence as u64,
                    step: 0,
                    occurred_at: SystemTime::UNIX_EPOCH,
                    event,
                },
            )
            .await
            .unwrap();
    }
    (
        store,
        HistoryContext {
            run_id: "history".into(),
            through_sequence,
            checkpoint_id: "checkpoint".into(),
        },
    )
}

fn started(text: &str) -> RunEvent {
    RunEvent::RunStarted {
        instruction: text.into(),
        files: vec![],
    }
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "lookup".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

async fn search(
    access: &EventStoreHistoryAccess,
    store: &InMemoryEventStore,
    context: &HistoryContext,
    arguments: Value,
) -> Value {
    let output = access
        .execute(
            store,
            context.clone(),
            call(SEARCH_RUN_HISTORY_TOOL, arguments),
        )
        .await
        .unwrap();
    serde_json::from_str(&output).unwrap()
}

fn assert_source_slice(hit: &Value, source: &str) {
    let offset = hit["preview_offset"].as_u64().unwrap() as usize;
    let preview = hit["preview"].as_str().unwrap();
    assert_eq!(
        preview,
        source
            .chars()
            .skip(offset)
            .take(preview.chars().count())
            .collect::<String>()
    );
    assert_eq!(hit["total_characters"], source.chars().count());
    assert_eq!(
        hit["preview_truncated"],
        preview.chars().count() < source.chars().count()
    );
}

#[tokio::test]
async fn user_source_keeps_original_inputs_and_attachments_without_derived_copies() {
    let attached = TextFileReference {
        name: "records.txt".into(),
        path: Some("source/records.txt".into()),
        text: "original NEEDLE attachment".into(),
    };
    let mut response = ModelResponse::final_answer("assistant NEEDLE".into());
    response.content.insert(
        0,
        ContentBlock::Reasoning {
            text: "private-reasoning-marker".into(),
        },
    );
    let events = vec![
        started("original NEEDLE"),
        RunEvent::InputReceived {
            content: "corrected NEEDLE".into(),
            files: vec![],
        },
        RunEvent::RunResumed {
            content: "continue".into(),
            files: vec![attached],
            interrupted_calls: vec![],
        },
        RunEvent::ModelResponseReceived {
            content: "assistant NEEDLE".into(),
            response: Some(response),
            provider_state: None,
            provider_continuation: None,
            token_usage: None,
        },
        RunEvent::InputRequested {
            prompt: "echo NEEDLE".into(),
        },
        RunEvent::ToolCallRequested {
            call_id: "result".into(),
            tool_name: "agent_control".into(),
            arguments: "NEEDLE".into(),
        },
        RunEvent::ToolCallSucceeded {
            call_id: "result".into(),
            output: "derived NEEDLE".into(),
            images: vec![],
        },
        RunEvent::AgentMessageReceived {
            message: AgentMessage {
                id: "report".into(),
                sender: "child".into(),
                text: "report NEEDLE".into(),
            },
        },
    ];
    let (store, context) = store_events(events).await;
    let access = EventStoreHistoryAccess::new();
    let default = search(&access, &store, &context, json!({"query":"needle"})).await;
    assert_eq!(default["total_matches"], 8);
    assert_eq!(
        default,
        search(
            &access,
            &store,
            &context,
            json!({"query":"needle","source":"all"})
        )
        .await
    );
    let users = search(
        &access,
        &store,
        &context,
        json!({"query":"needle","source":"user"}),
    )
    .await;
    assert_eq!(users["total_matches"], 3);
    for (index, field) in ["/instruction", "/content", "/files/0/text"]
        .iter()
        .enumerate()
    {
        assert_eq!(users["hits"][index]["sequence"], index);
        assert_eq!(users["hits"][index]["field"], *field);
        assert_eq!(users["hits"][index]["preview_truncated"], false);
    }
    assert_eq!(
        search(
            &access,
            &store,
            &context,
            json!({"query":"source/records","source":"user"})
        )
        .await["hits"][0]["field"],
        "/files/0/path"
    );
    assert_eq!(
        search(
            &access,
            &store,
            &context,
            json!({"query":"records.txt","source":"user"})
        )
        .await["hits"][0]["field"],
        "/files/0/name"
    );
    let cutoff = HistoryContext {
        through_sequence: 0,
        ..context.clone()
    };
    assert_eq!(
        search(
            &access,
            &store,
            &cutoff,
            json!({"query":"corrected","source":"user"})
        )
        .await["total_matches"],
        0
    );
    for source in ["all", "user"] {
        let result = search(
            &access,
            &store,
            &context,
            json!({"query":"private-reasoning-marker","source":source}),
        )
        .await;
        assert_eq!(result["total_matches"], 0);
    }
    let raw = access
        .execute(
            &store,
            context.clone(),
            call(
                READ_RUN_HISTORY_TOOL,
                json!({"from_sequence":0,"to_sequence":7}),
            ),
        )
        .await
        .unwrap();
    assert!(raw.contains("derived NEEDLE"));
    assert!(!raw.contains("private-reasoning-marker"));
    let schema: Value = serde_json::from_str(
        &HistoryAccess::<InMemoryEventStore>::definitions(&access)[0].input_schema,
    )
    .unwrap();
    assert_eq!(
        schema["properties"]["source"]["enum"],
        json!(["all", "user"])
    );
    assert_eq!(schema["properties"]["source"]["default"], "all");
    let invalid = access
        .execute(
            &store,
            context,
            call(
                SEARCH_RUN_HISTORY_TOOL,
                json!({"query":"needle","source":"assistant"}),
            ),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        invalid,
        HistoryAccessError::InvalidArguments { .. }
    ));
}

#[tokio::test]
async fn previews_find_late_unicode_matches_and_map_case_expansion_to_original_characters() {
    let text = format!(
        "{} before İSTANBUL ΟΣ NEEDLE 🦀 after {}",
        "旧🦀".repeat(500),
        "尾".repeat(100)
    );
    let (store, context) = store_events(vec![started(&text)]).await;
    let access = EventStoreHistoryAccess::with_limits(
        HistoryLimits::builder().preview_chars(40).build().unwrap(),
    );
    for (query, original) in [
        ("needle", "NEEDLE"),
        ("i\u{307}stanbul", "İSTANBUL"),
        ("ος", "ΟΣ"),
        ("🦀 after", "🦀 after"),
    ] {
        let result = search(
            &access,
            &store,
            &context,
            json!({"query":query,"source":"user"}),
        )
        .await;
        assert_eq!(result["total_matches"], 1);
        let hit = &result["hits"][0];
        assert_source_slice(hit, &text);
        assert!(hit["preview"].as_str().unwrap().contains(original));
        assert!(hit["preview"].as_str().unwrap().chars().count() <= 40);
        let offset = hit["match_offset"].as_u64().unwrap() as usize;
        let length = hit["match_length"].as_u64().unwrap() as usize;
        assert_eq!(
            text.chars().skip(offset).take(length).collect::<String>(),
            original
        );
    }
    // A match spanning more than the preview budget still locates the full
    // source range without pretending its clipped excerpt is complete.
    let result = search(&access, &store, &context, json!({"query":"尾".repeat(50)})).await;
    assert_eq!(result["hits"][0]["match_length"], 50);
    assert_source_slice(&result["hits"][0], &text);
}

#[tokio::test]
async fn search_budget_and_saved_projection_preserve_match_location_and_completeness() {
    let text = format!("{}needle{}", "前🦀".repeat(200), "尾".repeat(200));
    let (store, context) = store_events(vec![
        started(&text),
        RunEvent::InputReceived {
            content: text.clone(),
            files: vec![],
        },
    ])
    .await;
    let access = EventStoreHistoryAccess::new();
    let output = access
        .execute(
            &store,
            context.clone(),
            call(
                SEARCH_RUN_HISTORY_TOOL,
                json!({"query":"needle","source":"user","limit":1}),
            ),
        )
        .await
        .unwrap();
    let original: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(original["total_matches"], 2);
    assert_eq!(original["truncated"], true);
    let budget = output.chars().count() - 250;
    let projected = access
        .project_output(SEARCH_RUN_HISTORY_TOOL, &output, budget)
        .unwrap()
        .unwrap();
    assert!(projected.chars().count() <= budget);
    let projected: Value = serde_json::from_str(&projected).unwrap();
    assert_eq!(projected["hits"].as_array().unwrap().len(), 1);
    assert_source_slice(&projected["hits"][0], &text);
    assert!(
        projected["hits"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("needle")
    );
    assert_eq!(
        projected["hits"][0]["match_offset"],
        original["hits"][0]["match_offset"]
    );
    let bounded = access
        .execute_bounded(
            &store,
            context.clone(),
            call(
                SEARCH_RUN_HISTORY_TOOL,
                json!({"query":"needle","source":"user","limit":1}),
            ),
            Some(budget),
        )
        .await
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&bounded).unwrap(), projected);
    for budget in [0, 1] {
        let error = access
            .execute_bounded(
                &store,
                context.clone(),
                call(SEARCH_RUN_HISTORY_TOOL, json!({"query":"needle"})),
                Some(budget),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, HistoryAccessError::OutputTooLarge { .. }));
    }
    // Saved old responses lack field metadata. Reprojection must keep working
    // without claiming their serialized-event preview is a full source field.
    let legacy = json!({"warning":"old warning","checkpoint_id":"checkpoint","through_sequence":1,"total_matches":1,"truncated":false,"hits":[{"sequence":0,"step":0,"event_type":"run_started","preview":"旧".repeat(1000)}]}).to_string();
    let projected = access
        .project_output(SEARCH_RUN_HISTORY_TOOL, &legacy, 600)
        .unwrap()
        .unwrap();
    assert!(projected.chars().count() <= 600);
    let projected: Value = serde_json::from_str(&projected).unwrap();
    assert_eq!(projected["hits"][0]["preview_truncated"], true);
    assert!(projected["hits"][0]["field"].is_null());
}
