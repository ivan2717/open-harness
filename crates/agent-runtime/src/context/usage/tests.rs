use super::*;
use crate::{
    ApproximateTokenCounter, ModelResponse, ModelUsage, ProviderContextIdentity,
    ProviderContextWindow, ProviderReplayScope, ToolCall, ToolDefinition,
};
use agent_adapters::WireProtocol;

fn request() -> ModelRequest {
    ModelRequest {
        instructions: "Preserve the constraints.".into(),
        messages: vec![ModelMessage::user("Investigate this code.".into())],
        tools: vec![ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: "{}".into(),
        }],
        max_output_tokens: Some(8192),
        provider_context: None,
    }
}

fn metadata() -> ModelContextMetadata {
    ModelContextMetadata::new("provider", "model", 1_000_000).unwrap()
}

fn output(input_tokens: usize, output_tokens: usize) -> ModelOutput {
    ModelOutput {
        response: ModelResponse::tool_calls(
            "Checking the file".into(),
            vec![ToolCall {
                id: "read-1".into(),
                name: "read".into(),
                arguments: "{}".into(),
            }],
        ),
        usage: Some(ModelUsage {
            input_tokens,
            output_tokens,
            cached_input_tokens: Some(input_tokens),
        }),
        provider_state: None,
        provider_context: None,
        context_compacted: false,
    }
}

fn append_response(request: &mut ModelRequest, response: &ModelResponse) {
    request.messages.push(ModelMessage::assistant_blocks(
        response.content.clone(),
        None,
    ));
}

fn calibrated() -> (ContextUsageTracker, ModelRequest, ModelContextMetadata) {
    let tracker = ContextUsageTracker::default();
    let mut request = request();
    let metadata = metadata();
    let output = output(1000, 25);
    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &output);
    append_response(&mut request, &output.response);
    (tracker, request, metadata)
}

#[test]
fn actual_usage_plus_tail_estimate_counts_framing_once_and_ignores_cached_subset() {
    let counter = ApproximateTokenCounter::default();
    let input_estimate = counter.count_tokens(&request());
    let (tracker, mut request, metadata) = calibrated();
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        1000 + counter.count_tokens(&request) - input_estimate
    );
    request.messages.push(ModelMessage::tool_result(
        "read-1".into(),
        "新的内容🙂".into(),
        false,
    ));
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        1000 + counter.count_tokens(&request) - input_estimate
    );
}

#[test]
fn discarded_reasoning_output_is_not_charged_as_replayed_input() {
    struct MessageCounter;
    impl TokenCounter for MessageCounter {
        fn count_tokens(&self, request: &ModelRequest) -> usize {
            40 + request
                .messages
                .iter()
                .map(|message| message.content.len())
                .sum::<usize>()
                * 10
        }
    }

    let tracker = ContextUsageTracker::default();
    let metadata = metadata();
    let mut request = request();
    // Of the 113 generated tokens, only 20 are replayed as the assistant
    // message and tool call; 93 reasoning tokens are discarded by the provider.
    let output = output(70, 113);
    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &output);
    append_response(&mut request, &output.response);
    request.messages.push(ModelMessage::tool_result(
        "read-1".into(),
        "file contents".into(),
        false,
    ));
    // Replayed response plus tool result costs 30, producing 100 rather than
    // the old 70 input + 113 generated output + 10 tool result = 193.
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &MessageCounter),
        100
    );

    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &self::output(100, 2000));
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &MessageCounter),
        100
    );
}

#[test]
fn response_projection_changes_only_the_estimated_extension() {
    let counter = ApproximateTokenCounter::default();
    let input_estimate = counter.count_tokens(&request());
    let (tracker, mut request, metadata) = calibrated();
    request.messages.truncate(1);
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        1000
    );
    request.messages.push(ModelMessage::assistant(
        "Only the visible response is replayed.".into(),
        None,
    ));
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        1000 + counter.count_tokens(&request) - input_estimate
    );
}

#[test]
fn changed_provider_state_invalidates_an_otherwise_identical_input_prefix() {
    let tracker = ContextUsageTracker::default();
    let metadata = metadata();
    let counter = ApproximateTokenCounter::default();
    let mut request = request();
    let state = WireProtocol::ChatCompletions
        .capture_state(
            ProviderContextIdentity {
                provider: "provider".into(),
                model: "model".into(),
                format: "v1".into(),
            },
            ProviderReplayScope::Response,
            serde_json::json!({
                "role": "assistant",
                "content": "Visible response",
                "reasoning_content": "Original reasoning"
            }),
            100,
        )
        .unwrap();
    request.messages.push(ModelMessage::assistant(
        "Visible response".into(),
        Some(state),
    ));
    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &output(1000, 25));
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        1000
    );

    let mut changed = request.clone();
    let state = changed.messages[1].provider_state.as_mut().unwrap();
    state.payload["reasoning_content"] = "Different reasoning".into();
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &changed, &counter),
        counter.count_tokens(&changed)
    );
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        1000
    );
}

#[test]
fn changed_requests_invalidate_only_when_recorded_as_the_actual_request() {
    let counter = ApproximateTokenCounter::default();
    let input_estimate = counter.count_tokens(&request());
    for change in 0..5 {
        let (tracker, request, metadata) = calibrated();
        let mut changed = request.clone();
        match change {
            0 => changed.instructions.push_str(" Updated."),
            1 => changed.tools[0].input_schema = r#"{"type":"object"}"#.into(),
            2 => {
                changed.messages[0] = ModelMessage::user("Different input".into());
            }
            3 => changed.messages.clear(),
            4 => {
                changed.messages[0] =
                    ModelMessage::summary("checkpoint".into(), "Investigate this code.".into());
            }
            _ => unreachable!(),
        }
        assert_eq!(
            tracker.estimate("run", Some(&metadata), &changed, &counter),
            counter.count_tokens(&changed)
        );
        // A hypothetical compaction or fallback candidate must not erase
        // calibration for the original request.
        assert_eq!(
            tracker.estimate("run", Some(&metadata), &request, &counter),
            1000 + counter.count_tokens(&request) - input_estimate
        );
        tracker.record_request("run", Some(&metadata), &changed);
        assert_eq!(
            tracker.estimate("run", Some(&metadata), &request, &counter),
            counter.count_tokens(&request)
        );
    }
}

#[test]
fn changed_model_or_budget_metadata_invalidates_the_baseline() {
    let counter = ApproximateTokenCounter::default();
    for change in 0..4 {
        let (tracker, request, mut metadata) = calibrated();
        match change {
            0 => metadata.provider = "another provider".into(),
            1 => metadata.model = "another model".into(),
            2 => metadata.context_window = 500_000,
            3 => metadata.effective_context_window_percent = 90,
            _ => unreachable!(),
        }
        assert_eq!(
            tracker.estimate("run", Some(&metadata), &request, &counter),
            counter.count_tokens(&request)
        );
    }
    let (tracker, request, _) = calibrated();
    assert_eq!(
        tracker.estimate("run", None, &request, &counter),
        counter.count_tokens(&request)
    );
}

#[test]
fn missing_model_identity_never_reuses_provider_usage() {
    let counter = ApproximateTokenCounter::default();
    let input_estimate = counter.count_tokens(&request());
    let (tracker, request, metadata) = calibrated();
    assert_eq!(
        tracker.estimate("run", None, &request, &counter),
        counter.count_tokens(&request)
    );
    // Speculation without metadata leaves a known model's baseline untouched.
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        1000 + counter.count_tokens(&request) - input_estimate
    );
    tracker.record_request("run", None, &request);
    tracker.observe_response("run", None, &output(10_000, 100));
    assert_eq!(
        tracker.estimate("run", None, &request, &counter),
        counter.count_tokens(&request)
    );
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        counter.count_tokens(&request)
    );
}

#[test]
fn next_response_cap_does_not_change_existing_input_cost() {
    let counter = ApproximateTokenCounter::default();
    let input_estimate = counter.count_tokens(&request());
    let (tracker, mut request, metadata) = calibrated();
    request.max_output_tokens = Some(32_768);
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter,),
        1000 + counter.count_tokens(&request) - input_estimate
    );
}

#[test]
fn missing_usage_preserves_only_the_last_matching_baseline() {
    let counter = ApproximateTokenCounter::default();
    let input_estimate = counter.count_tokens(&request());
    let (tracker, mut request, metadata) = calibrated();
    tracker.record_request("run", Some(&metadata), &request);
    let mut next_output = output(1, 1);
    next_output.usage = None;
    tracker.observe_response("run", Some(&metadata), &next_output);
    append_response(&mut request, &next_output.response);
    let expected = 1000 + counter.count_tokens(&request) - input_estimate;
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        expected
    );
    // A second observation cannot attach usage to the consumed request.
    tracker.observe_response("run", Some(&metadata), &output(1, 1));
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        expected
    );
    request.instructions.push_str(" Changed.");
    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &next_output);
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        counter.count_tokens(&request)
    );
}

#[test]
fn zero_input_usage_is_not_applied_to_nonempty_input() {
    let counter = ApproximateTokenCounter::default();
    let (tracker, mut request, metadata) = calibrated();
    let invalid = output(0, 10);
    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &invalid);
    append_response(&mut request, &invalid.response);
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        counter.count_tokens(&request)
    );
}

#[test]
fn output_usage_does_not_change_the_input_baseline() {
    let counter = ApproximateTokenCounter::default();
    for (input_tokens, output_tokens) in [(100, 0), (100, usize::MAX), (usize::MAX, 1)] {
        let tracker = ContextUsageTracker::default();
        let metadata = metadata();
        let request = request();
        tracker.record_request("run", Some(&metadata), &request);
        tracker.observe_response("run", Some(&metadata), &output(input_tokens, output_tokens));
        assert_eq!(
            tracker.estimate("run", Some(&metadata), &request, &counter),
            input_tokens
        );
    }
}

#[test]
fn zero_output_usage_still_estimates_replayed_responses() {
    let counter = ApproximateTokenCounter::default();
    for response in [
        ModelResponse::final_answer(String::new()),
        ModelResponse::input_required("Please clarify.".into()),
        ModelResponse::tool_calls(
            String::new(),
            vec![ToolCall {
                id: "id".into(),
                name: "read".into(),
                arguments: "{}".into(),
            }],
        ),
    ] {
        let tracker = ContextUsageTracker::default();
        let metadata = metadata();
        let mut request = request();
        let input_estimate = counter.count_tokens(&request);
        let mut output = output(100, 0);
        output.response = response;
        tracker.record_request("run", Some(&metadata), &request);
        tracker.observe_response("run", Some(&metadata), &output);
        append_response(&mut request, &output.response);
        assert_eq!(
            tracker.estimate("run", Some(&metadata), &request, &counter,),
            100 + counter.count_tokens(&request) - input_estimate
        );
    }
}

fn native_window() -> ProviderContextWindow {
    ProviderContextWindow {
        prefix_fingerprint: None,
        identity: ProviderContextIdentity {
            provider: "provider".into(),
            model: "model".into(),
            format: "v1".into(),
        },
        items: vec![serde_json::json!({"opaque": "x".repeat(10_000)})],
        input_tokens: 500,
    }
}

#[test]
fn native_requests_and_responses_use_native_count_without_usage_calibration() {
    let counter = ApproximateTokenCounter::default();
    let (tracker, mut request, metadata) = calibrated();
    request.provider_context = Some(native_window());
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        counter.count_tokens(&request)
    );
    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &output(1, 1));
    request.provider_context = None;
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        counter.count_tokens(&request)
    );
    for compacted in [false, true] {
        let (tracker, request, metadata) = calibrated();
        tracker.record_request("run", Some(&metadata), &request);
        let mut output = output(1, 1);
        output.context_compacted = compacted;
        if !compacted {
            output.provider_context = Some(native_window());
        }
        tracker.observe_response("run", Some(&metadata), &output);
        assert_eq!(
            tracker.estimate("run", Some(&metadata), &request, &counter),
            counter.count_tokens(&request)
        );
    }
}

#[test]
fn independent_runs_can_share_clones_and_update_concurrently() {
    let tracker = ContextUsageTracker::default();
    let threads = (0..16)
        .map(|index| {
            let tracker = tracker.clone();
            std::thread::spawn(move || {
                let run_id = format!("run-{index}");
                let metadata = metadata();
                let mut request = request();
                let counter = ApproximateTokenCounter::default();
                let input_estimate = counter.count_tokens(&request);
                let output = output(100 + index, 0);
                tracker.record_request(&run_id, Some(&metadata), &request);
                tracker.observe_response(&run_id, Some(&metadata), &output);
                append_response(&mut request, &output.response);
                assert_eq!(
                    tracker.estimate(&run_id, Some(&metadata), &request, &counter,),
                    100 + index + counter.count_tokens(&request) - input_estimate
                );
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(tracker.cache.lock().unwrap().runs.len(), 16);
}

#[test]
fn cache_is_bounded_and_explicit_forget_discards_usage() {
    let (tracker, request, metadata) = calibrated();
    tracker.forget("run");
    let counter = ApproximateTokenCounter::default();
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        counter.count_tokens(&request)
    );
    for index in 0..=MAX_TRACKED_RUNS {
        tracker.record_request(&format!("run-{index}"), Some(&metadata), &request);
    }
    let cache = tracker.cache.lock().unwrap();
    assert_eq!(cache.runs.len(), MAX_TRACKED_RUNS);
    assert!(!cache.runs.contains_key("run-0"));
}

#[test]
fn poisoned_cache_falls_back_without_panicking() {
    let (tracker, request, metadata) = calibrated();
    let copy = tracker.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = copy.cache.lock().unwrap();
            panic!("poison accounting cache");
        })
        .join()
        .is_err()
    );
    let counter = ApproximateTokenCounter::default();
    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &output(1, 1));
    tracker.forget("run");
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        counter.count_tokens(&request)
    );
}

#[test]
fn calibrated_extension_saturates_and_non_monotonic_counters_fall_back() {
    let tracker = ContextUsageTracker::default();
    let metadata = metadata();
    let mut request = request();
    let output = output(usize::MAX - 2, 0);
    tracker.record_request("run", Some(&metadata), &request);
    tracker.observe_response("run", Some(&metadata), &output);
    append_response(&mut request, &output.response);
    request.messages.push(ModelMessage::tool_result(
        "read-1".into(),
        "long tool output".into(),
        false,
    ));
    assert_eq!(
        tracker.estimate(
            "run",
            Some(&metadata),
            &request,
            &ApproximateTokenCounter::default(),
        ),
        usize::MAX
    );
    struct NonMonotonicCounter;
    impl TokenCounter for NonMonotonicCounter {
        fn count_tokens(&self, request: &ModelRequest) -> usize {
            1000 / request.messages.len().max(1)
        }
    }
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &NonMonotonicCounter),
        NonMonotonicCounter.count_tokens(&request)
    );
}
