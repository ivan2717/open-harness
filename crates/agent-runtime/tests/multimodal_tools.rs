mod support;
use agent_core::agent::{Agent, RunEvent};
use agent_runtime::*;
use std::{
    convert::Infallible,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

fn image() -> ModelImage {
    ModelImage::new("image/png", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZcZkAAAAASUVORK5CYII=").unwrap()
}
struct ScreenshotTools(Vec<ToolDefinition>, usize);
impl ToolExecutor for ScreenshotTools {
    type Error = Infallible;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.0
    }
    async fn execute(&self, _: ToolCall) -> Result<String, Self::Error> {
        panic!("must use structured output")
    }
    async fn execute_output(&self, call: ToolCall) -> Result<ToolOutput, Self::Error> {
        let mut observation = image();
        if self.1 > 0 {
            // Padding only stresses retention/storage, not raster rendering.
            let mut bytes = observation.decoded_bytes().unwrap();
            bytes.resize(self.1, 0);
            bytes.extend(call.id.as_bytes());
            observation = ModelImage::from_bytes("image/png", &bytes).unwrap();
        }
        Ok(ToolOutput {
            text: format!("Observation {}", call.id),
            images: vec![observation],
        })
    }
}
struct InspectModel {
    requests: Mutex<Vec<ModelRequest>>,
    vision: bool,
    screenshot_count: usize,
}
impl Model for InspectModel {
    type Error = Infallible;
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_images: self.vision,
            ..Default::default()
        }
    }
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(request);
        let n = requests.len();
        Ok(if n <= self.screenshot_count {
            ModelResponse::tool_calls(
                "observe".into(),
                vec![ToolCall {
                    id: format!("c{n}"),
                    name: "screenshot".into(),
                    arguments: "{}".into(),
                }],
            )
        } else {
            ModelResponse::final_answer("done".into())
        })
    }
}

#[tokio::test]
async fn screenshots_reach_vision_models_while_old_observations_leave_active_context() {
    for vision in [true, false] {
        let runtime = AgentRuntime::new(
            InspectModel {
                requests: Mutex::new(vec![]),
                vision,
                screenshot_count: 2,
            },
            ScreenshotTools(
                vec![ToolDefinition {
                    name: "screenshot".into(),
                    description: "observe".into(),
                    input_schema: "{}".into(),
                }],
                0,
            ),
            support::test_context_manager(),
            Some(4),
        );
        let agent = Agent::new("a", "a", "", "");
        let mut run = runtime
            .start_run(&agent, "images", "inspect")
            .await
            .unwrap();
        runtime.run_until_blocked(&agent, &mut run).await.unwrap();
        {
            let requests = runtime.model().requests.lock().unwrap();
            let results = requests[2]
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .filter_map(|b| match b {
                    ContentBlock::ToolResult {
                        content, images, ..
                    } => Some((content, images)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(results.len(), 2);
            assert!(results[0].1.is_empty());
            assert!(results[0].0.contains("Older image observation omitted"));
            assert_eq!(results[1].1.len(), usize::from(vision));
            if !vision {
                assert!(results[1].0.contains("does not support visual input"));
            }
            assert_eq!(
                run.events()
                    .iter()
                    .filter_map(|e| match &e.event {
                        RunEvent::ToolCallSucceeded { images, .. } => Some(images.len()),
                        _ => None,
                    })
                    .sum::<usize>(),
                2
            );
            assert!(
                !serde_json::to_string(
                    &run.events()
                        .iter()
                        .map(|e| e.event.without_provider_state())
                        .collect::<Vec<_>>()
                )
                .unwrap()
                .contains(&image().data_base64)
            );
        }
        let full_history = runtime.event_store().load(run.id()).await.unwrap().unwrap();
        let latest_only = runtime
            .event_store()
            .load_for_runtime(run.id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(full_history, latest_only);
        assert_eq!(inline_image_count(&full_history.events), 2);
        assert_eq!(inline_image_count(&latest_only.events), 1);
        assert_eq!(inline_image_count(run.events()), 1);
        if !vision {
            let mut metadata = ModelContextMetadata::new("test", "text-only", 4096).unwrap();
            metadata.max_output_tokens = Some(256);
            let input = ContextInput::new("images", "", run.events().to_vec(), vec![])
                .with_image_input(false)
                .with_model_metadata(metadata);
            // An image allowance alone exceeds this model window. Filtering
            // after preparation would unnecessarily compact or fail the run.
            let prepared = support::test_context_manager()
                .prepare(input.as_ref())
                .await
                .unwrap();
            assert!(ApproximateTokenCounter::default().count_tokens(&prepared.request) < 1024);
        }
    }
}

#[tokio::test]
async fn durable_images_use_checked_sidecars_and_survive_reopening_the_store() {
    let directory = std::env::temp_dir().join(format!(
        "openharness-images-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = JsonlEventStore::new(&directory);
    let agent = Agent::new("a", "a", "", "");
    let mut run = agent.start_run("persisted-image", "inspect");
    store
        .append(run.id(), run.agent_id(), &run.events()[0])
        .await
        .unwrap();
    run.begin_step().unwrap();
    run.record(RunEvent::ModelResponseReceived {
        content: "".into(),
        response: None,
        provider_state: None,
        provider_continuation: None,
        token_usage: None,
    })
    .unwrap();
    run.record(RunEvent::ToolCallRequested {
        call_id: "c1".into(),
        tool_name: "screenshot".into(),
        arguments: "{}".into(),
    })
    .unwrap();
    run.record(RunEvent::ToolCallSucceeded {
        call_id: "c1".into(),
        output: "observed".into(),
        images: vec![image()],
    })
    .unwrap();
    for event in &run.events()[1..] {
        store.append(run.id(), run.agent_id(), event).await.unwrap();
    }
    let log = std::fs::read_dir(&directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .unwrap();
    let jsonl = std::fs::read_to_string(log).unwrap();
    assert!(jsonl.contains("sha256:"));
    assert!(!jsonl.contains(&image().data_base64));
    let reopened = JsonlEventStore::new(&directory);
    assert_eq!(
        reopened.load(run.id()).await.unwrap().unwrap().events,
        run.events()
    );
    assert_eq!(
        reopened
            .append(run.id(), run.agent_id(), run.events().last().unwrap())
            .await
            .unwrap(),
        AppendOutcome::AlreadyPresent
    );
    let sidecar = std::fs::read_dir(directory.join("images"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(sidecar, b"corrupted").unwrap();
    assert!(reopened.load(run.id()).await.is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn image_budgets_count_image_allowances_instead_of_base64_characters() {
    let mut request = ModelRequest {
        instructions: String::new(),
        messages: vec![ModelMessage::tool_result(
            "c".into(),
            "observed".into(),
            false,
        )],
        tools: vec![],
        max_output_tokens: None,
        provider_context: None,
    };
    let counter = ApproximateTokenCounter::default();
    let baseline = counter.count_tokens(&request);
    if let ContentBlock::ToolResult { images, .. } = &mut request.messages[0].content[0] {
        images.push(image());
    }
    assert_eq!(
        counter.count_tokens(&request) - baseline,
        agent_core::IMAGE_TOKEN_ALLOWANCE
    );
}

fn inline_image_count(events: &[agent_core::agent::RunEventEnvelope]) -> usize {
    events
        .iter()
        .flat_map(|e| match &e.event {
            RunEvent::ToolCallSucceeded { images, .. } => images.as_slice(),
            _ => &[],
        })
        .filter(|image| image.reference_id().is_none())
        .count()
}

#[tokio::test]
async fn long_screenshot_runs_release_old_rasters_and_restore_without_losing_evidence() {
    let directory = std::env::temp_dir().join(format!(
        "openharness-image-memory-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let runtime = AgentRuntime::with_event_store(
        InspectModel {
            requests: Mutex::new(vec![]),
            vision: true,
            screenshot_count: 40,
        },
        ScreenshotTools(
            vec![ToolDefinition {
                name: "screenshot".into(),
                description: "observe".into(),
                input_schema: "{}".into(),
            }],
            256 * 1024,
        ),
        support::test_context_manager(),
        JsonlEventStore::new(&directory),
        Some(42),
    );
    let agent = Agent::new("a", "a", "", "");
    // Keep an intentionally slow subscriber alive throughout all screenshots.
    let mut receiver = runtime.subscribe();
    let mut run = runtime
        .start_run(&agent, "many-images", "inspect repeatedly")
        .await
        .unwrap();
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    let mut queued = Vec::new();
    while let Ok(notification) = receiver.try_recv() {
        queued.push(notification.envelope);
    }
    assert_eq!(queued, run.events());
    assert_eq!(inline_image_count(&queued), 0);
    assert_eq!(inline_image_count(run.events()), 1);
    let image_data = run
        .events()
        .iter()
        .flat_map(|e| match &e.event {
            RunEvent::ToolCallSucceeded { images, .. } => images.as_slice(),
            _ => &[],
        })
        .collect::<Vec<_>>();
    assert_eq!(image_data.len(), 40);
    assert!(
        image_data
            .iter()
            .map(|image| image.data_base64.len())
            .sum::<usize>()
            < 360_000
    );
    let historical_id = image_data[0].reference_id().unwrap().to_owned();
    {
        let requests = runtime.model().requests.lock().unwrap();
        for request in requests.iter() {
            for block in request.messages.iter().flat_map(|m| &m.content) {
                if let ContentBlock::ToolResult { images, .. } = block {
                    for image in images {
                        assert!(image.reference_id().is_none());
                        image.validate().unwrap();
                    }
                }
            }
        }
    }
    let archive = runtime.event_store().load(run.id()).await.unwrap().unwrap();
    assert_eq!(inline_image_count(&archive.events), 40);
    assert_eq!(archive.events, run.events());
    let reopened = JsonlEventStore::new(&directory);
    let restored = reopened.load_for_runtime(run.id()).await.unwrap().unwrap();
    assert_eq!(inline_image_count(&restored.events), 1);
    assert_eq!(restored.events, archive.events);
    let historical_sequence = archive
        .events
        .iter()
        .find(|e| matches!(&e.event, RunEvent::ToolCallSucceeded { .. }))
        .unwrap()
        .sequence;
    let range = reopened
        .load_range(run.id(), historical_sequence..historical_sequence + 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inline_image_count(&range.events), 1);
    assert!(range.events[0] == archive.events[historical_sequence as usize]);
    let mut restored = runtime.restore_run(run.id()).await.unwrap().unwrap();
    assert_eq!(inline_image_count(restored.events()), 1);
    runtime.resume_run(&mut restored, "continue").await.unwrap();
    runtime
        .run_until_blocked(&agent, &mut restored)
        .await
        .unwrap();
    assert_eq!(inline_image_count(restored.events()), 1);
    // Old pixels are not read on every round. Actual archive reads must still
    // catch damaged content, and missing files cannot be presented as evidence.
    let historical_path = directory.join("images").join(historical_id);
    std::fs::write(&historical_path, b"damaged").unwrap();
    assert!(reopened.load_for_runtime(run.id()).await.is_ok());
    assert!(
        reopened
            .load_range(run.id(), historical_sequence..historical_sequence + 1)
            .await
            .is_err()
    );
    assert!(reopened.load(run.id()).await.is_err());
    std::fs::remove_file(historical_path).unwrap();
    assert!(reopened.load_for_runtime(run.id()).await.is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn user_images_survive_sidecars_restart_followups_and_model_changes() {
    let directory = std::env::temp_dir().join(format!(
        "openharness-user-images-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let agent = Agent::new("a", "a", "", "");
    let runtime = AgentRuntime::with_event_store(
        InspectModel {
            requests: Mutex::new(vec![]),
            vision: true,
            screenshot_count: 0,
        },
        ScreenshotTools(vec![], 0),
        support::test_context_manager(),
        JsonlEventStore::new(&directory),
        Some(4),
    );
    let mut run = runtime
        .start_run_with_files(
            &agent,
            "user-image",
            "",
            vec![agent_core::FileReference {
                attachment: None,
                name: "screenshot.png".into(),
                path: None,
                text: String::new(),
                image: Some(image()),
            }],
        )
        .await
        .unwrap();
    runtime.run_until_blocked(&agent, &mut run).await.unwrap();
    let events = run.events().to_vec();
    let log = std::fs::read_dir(&directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .unwrap();
    let serialized = std::fs::read_to_string(log).unwrap();
    assert!(serialized.contains("sha256:"));
    assert!(!serialized.contains(&image().data_base64));
    let store = JsonlEventStore::new(&directory);
    let loaded = store.load_for_runtime("user-image").await.unwrap().unwrap();
    assert_eq!(loaded.events, events);
    assert_eq!(loaded.events[0].event.images()[0].reference_id(), None);
    assert_eq!(
        store.append("user-image", "a", &events[0]).await.unwrap(),
        AppendOutcome::AlreadyPresent
    );
    let mut restored = runtime.restore_run("user-image").await.unwrap().unwrap();
    runtime
        .resume_run(&mut restored, "What did the screenshot show?")
        .await
        .unwrap();
    runtime
        .run_until_blocked(&agent, &mut restored)
        .await
        .unwrap();
    {
        let requests = runtime.model().requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            let images = request
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .filter(|b| matches!(b, ContentBlock::Image { .. }))
                .count();
            assert_eq!(images, 1);
            assert!(
                !request
                    .messages
                    .iter()
                    .any(|m| m.text().contains(&image().data_base64))
            );
        }
    }
    let mut metadata = ModelContextMetadata::new("test", "text-model", 4096).unwrap();
    metadata.max_output_tokens = Some(256);
    let prepared = support::test_context_manager()
        .prepare_owned(
            ContextInput::new("user-image", "", restored.events().to_vec(), vec![])
                .with_image_input(false)
                .with_model_metadata(metadata),
        )
        .await
        .unwrap();
    assert!(
        !prepared
            .request
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .any(|b| matches!(b, ContentBlock::Image { .. }))
    );
    assert!(
        prepared
            .request
            .messages
            .iter()
            .any(|m| m.text().contains("Do not infer"))
    );
    assert!(
        store.load("user-image").await.unwrap().unwrap().events[0]
            .event
            .images()[0]
            .validate()
            .is_ok()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn user_image_token_cost_is_independent_of_base64_text_length() {
    let mut request = ModelRequest {
        instructions: String::new(),
        messages: vec![ModelMessage::user("inspect".into())],
        tools: vec![],
        max_output_tokens: None,
        provider_context: None,
    };
    let counter = ApproximateTokenCounter::default();
    let baseline = counter.count_tokens(&request);
    request.messages[0]
        .content
        .push(ContentBlock::Image { image: image() });
    assert_eq!(
        counter.count_tokens(&request) - baseline,
        agent_core::IMAGE_TOKEN_ALLOWANCE
    );
    let mut pixels = image().decoded_bytes().unwrap();
    pixels.resize(1024 * 1024, 0);
    request.messages[0].content[1] = ContentBlock::Image {
        image: ModelImage::from_bytes("image/png", &pixels).unwrap(),
    };
    assert_eq!(
        counter.count_tokens(&request) - baseline,
        agent_core::IMAGE_TOKEN_ALLOWANCE
    );
}
