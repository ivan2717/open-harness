use super::*;
use crate::context::request_usage_baseline;
use crate::{
    ApproximateTokenCounter, ModelMessage, ModelResponse, ModelUsage, ProviderContextIdentity,
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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

    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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
fn changed_fixed_headers_invalidate_only_when_recorded_as_the_actual_request() {
    let counter = ApproximateTokenCounter::default();
    let input_estimate = counter.count_tokens(&request());
    for change in 0..2 {
        let (tracker, request, metadata) = calibrated();
        let mut changed = request.clone();
        match change {
            0 => changed.instructions.push_str(" Updated."),
            1 => changed.tools[0].input_schema = r#"{"type":"object"}"#.into(),
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
        tracker.record_request(
            "run",
            Some(&metadata),
            &changed,
            &ApproximateTokenCounter::default(),
        );
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
    tracker.record_request("run", None, &request, &ApproximateTokenCounter::default());
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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
        tracker.record_request(
            "run",
            Some(&metadata),
            &request,
            &ApproximateTokenCounter::default(),
        );
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
        tracker.record_request(
            "run",
            Some(&metadata),
            &request,
            &ApproximateTokenCounter::default(),
        );
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
    tracker.observe_response("run", Some(&metadata), &output(1, 1));
    request.provider_context = None;
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        counter.count_tokens(&request)
    );
    for compacted in [false, true] {
        let (tracker, request, metadata) = calibrated();
        tracker.record_request(
            "run",
            Some(&metadata),
            &request,
            &ApproximateTokenCounter::default(),
        );
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
                tracker.record_request(
                    &run_id,
                    Some(&metadata),
                    &request,
                    &ApproximateTokenCounter::default(),
                );
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
        tracker.record_request(
            &format!("run-{index}"),
            Some(&metadata),
            &request,
            &ApproximateTokenCounter::default(),
        );
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
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

fn usage_event(
    sequence: u64,
    metadata: &ModelContextMetadata,
    request: &ModelRequest,
    input_tokens: usize,
) -> RunEventEnvelope {
    RunEventEnvelope {
        sequence,
        step: sequence as u32,
        occurred_at: std::time::SystemTime::UNIX_EPOCH,
        event: RunEvent::ModelResponseReceived {
            content: "done".into(),
            response: None,
            provider_state: None,
            provider_continuation: None,
            token_usage: Some(Box::new(agent_core::token_usage::ModelUsageRecord {
                usage: ModelUsage {
                    input_tokens,
                    output_tokens: 500,
                    cached_input_tokens: Some(input_tokens),
                },
                baseline: request_usage_baseline(Some(metadata), request),
            })),
        },
    }
}

#[test]
fn durable_usage_round_trip_restores_the_same_prediction_and_margin() {
    let metadata = metadata();
    let request = request();
    let event = usage_event(3, &metadata, &request, 1000);
    let serialized = serde_json::to_string(&event).unwrap();
    assert!(!serialized.contains("Preserve the constraints"));
    assert!(!serialized.contains("Investigate this code"));
    let events = vec![serde_json::from_str(&serialized).unwrap()];
    let counter = ApproximateTokenCounter::default();
    let (live, continued, _) = calibrated();
    let restored = ContextUsageTracker::default();
    restored.restore("run", Some(&metadata), &continued, &events);
    let actual = restored.measurement("run", Some(&metadata), &continued, &counter);
    assert_eq!(
        actual,
        live.measurement("run", Some(&metadata), &continued, &counter)
    );
    assert_eq!(actual.measured_input_tokens, Some(1000));
    assert_eq!(
        actual.source,
        super::super::measurement::ContextMeasurementSource::ProviderCalibrated
    );
    let extension = counter.count_tokens(&continued) - counter.count_tokens(&request);
    assert_eq!(actual.estimated_delta_tokens, extension);
    assert_eq!(
        actual.estimation_margin_tokens,
        (extension * 15).div_ceil(100)
    );
    assert_eq!(
        actual.decision_input_tokens,
        1000 + extension + actual.estimation_margin_tokens
    );
}

#[test]
fn restored_baseline_rejects_changed_prefix_tools_model_and_adapter() {
    let metadata = metadata();
    let request = request();
    let events = vec![usage_event(1, &metadata, &request, 1000)];
    let counter = ApproximateTokenCounter::default();
    for change in 0..6 {
        let mut changed = request.clone();
        let mut changed_metadata = metadata.clone();
        match change {
            0 => changed.instructions.push_str(" changed"),
            1 => changed.tools[0].input_schema = "[]".into(),
            2 => changed.messages[0] = ModelMessage::user("changed".into()),
            3 => changed.messages[0] = ModelMessage::summary("summary".into(), "short".into()),
            4 => changed_metadata.model = "other".into(),
            5 => changed_metadata.accounting_identity = Some("responses-v2".into()),
            _ => unreachable!(),
        }
        let tracker = ContextUsageTracker::default();
        tracker.restore("run", Some(&changed_metadata), &changed, &events);
        let actual = tracker.measurement("run", Some(&changed_metadata), &changed, &counter);
        assert_eq!(actual.measured_input_tokens, None);
        assert_eq!(
            actual.estimated_input_tokens,
            counter.count_tokens(&changed)
        );
    }
}

#[test]
fn restore_preserves_pending_request_and_updates_new_durable_samples() {
    let metadata = metadata();
    let request = request();
    let mut events = vec![usage_event(1, &metadata, &request, 1000)];
    let tracker = ContextUsageTracker::default();
    let counter = ApproximateTokenCounter::default();
    tracker.record_request(
        "run",
        Some(&metadata),
        &request,
        &ApproximateTokenCounter::default(),
    );
    tracker.restore("run", Some(&metadata), &request, &events);
    tracker.observe_response("run", Some(&metadata), &output(2000, 100));
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        2000
    );
    // A diagnostic read must not roll a freshly observed sample back.
    tracker.restore("run", Some(&metadata), &request, &events);
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        2000
    );
    events.push(usage_event(2, &metadata, &request, 3000));
    tracker.restore("run", Some(&metadata), &request, &events);
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &request, &counter),
        3000
    );
}

#[test]
fn missing_durable_usage_keeps_anchor_but_unbound_or_zero_usage_invalidates_it() {
    let metadata = metadata();
    let request = request();
    let counter = ApproximateTokenCounter::default();
    for mode in 0..3 {
        let mut latest = usage_event(2, &metadata, &request, 0);
        if let RunEvent::ModelResponseReceived { token_usage, .. } = &mut latest.event {
            match mode {
                0 => *token_usage = None,
                1 => token_usage.as_mut().unwrap().baseline = None,
                2 => {}
                _ => unreachable!(),
            }
        }
        let events = vec![usage_event(1, &metadata, &request, 1000), latest];
        let tracker = ContextUsageTracker::default();
        tracker.restore("run", Some(&metadata), &request, &events);
        assert_eq!(
            tracker.estimate("run", Some(&metadata), &request, &counter),
            if mode == 0 {
                1000
            } else {
                counter.count_tokens(&request)
            }
        );
    }
}

#[test]
fn native_measurement_only_adds_margin_to_visible_input() {
    let tracker = ContextUsageTracker::default();
    let metadata = metadata();
    let mut request = request();
    let counter = ApproximateTokenCounter::default();
    let visible = counter.count_tokens(&request);
    request.provider_context = Some(native_window());
    let measurement = tracker.measurement("run", Some(&metadata), &request, &counter);
    assert_eq!(measurement.measured_input_tokens, Some(500));
    assert_eq!(measurement.estimated_delta_tokens, visible);
    assert_eq!(
        measurement.estimation_margin_tokens,
        (visible * 15).div_ceil(100)
    );
    assert_eq!(measurement.estimated_input_tokens, visible + 500);
    assert_eq!(
        measurement.source,
        super::super::measurement::ContextMeasurementSource::ProviderNative
    );
}

#[test]
fn measurement_margin_can_be_disabled_recomputed_and_saturates() {
    let measured = ContextMeasurement::estimated(100).with_margin_percent(20);
    assert_eq!(measured.decision_input_tokens, 120);
    assert_eq!(
        measured
            .clone()
            .with_margin_percent(15)
            .decision_input_tokens,
        115
    );
    assert_eq!(measured.with_margin_percent(0).decision_input_tokens, 100);
    assert_eq!(
        ContextMeasurement::estimated(usize::MAX).decision_input_tokens,
        usize::MAX
    );
    let exact = ContextMeasurement::calibrated(100, 2000, 0);
    assert_eq!(exact.decision_input_tokens, 2000);
    assert_eq!(exact.estimation_margin_tokens, 0);
}

#[test]
fn restoring_native_or_unknown_candidates_does_not_erase_live_calibration() {
    let (tracker, request, metadata) = calibrated();
    let counter = ApproximateTokenCounter::default();
    let expected = tracker.measurement("run", Some(&metadata), &request, &counter);
    let events = vec![usage_event(1, &metadata, &self::request(), 1000)];
    let mut native = request.clone();
    native.provider_context = Some(native_window());
    tracker.restore("run", Some(&metadata), &native, &events);
    tracker.restore("run", None, &request, &events);
    assert_eq!(
        tracker.measurement("run", Some(&metadata), &request, &counter),
        expected
    );
}

#[test]
fn spending_only_usage_never_calibrates_requests_or_restored_history() {
    let (tracker, request, metadata) = calibrated();
    let counter = ApproximateTokenCounter::default();
    let events = vec![usage_event(1, &metadata, &self::request(), 1000)];
    let spending_only = metadata.clone().with_input_usage_calibration(false);
    assert!(request_usage_baseline(Some(&spending_only), &request).is_none());
    tracker.restore("run", Some(&spending_only), &request, &events);
    assert_eq!(
        tracker
            .measurement("run", Some(&spending_only), &request, &counter)
            .measured_input_tokens,
        None
    );
    // Inspecting another route preserves the actual route's anchor.
    assert_eq!(
        tracker
            .measurement("run", Some(&metadata), &request, &counter)
            .measured_input_tokens,
        Some(1000)
    );
    tracker.record_request(
        "run",
        Some(&spending_only),
        &request,
        &ApproximateTokenCounter::default(),
    );
    tracker.observe_response("run", Some(&spending_only), &output(10_000, 10));
    assert_eq!(
        tracker
            .measurement("run", Some(&spending_only), &request, &counter)
            .measured_input_tokens,
        None
    );
    assert_eq!(
        tracker
            .measurement("run", Some(&metadata), &request, &counter)
            .measured_input_tokens,
        None
    );
}

#[test]
fn legacy_log_without_samples_preserves_an_observed_live_baseline() {
    let (tracker, request, metadata) = calibrated();
    let counter = ApproximateTokenCounter::default();
    let expected = tracker.measurement("run", Some(&metadata), &request, &counter);
    let mut event = usage_event(1, &metadata, &request, 1000);
    if let RunEvent::ModelResponseReceived { token_usage, .. } = &mut event.event {
        *token_usage = None;
    }
    tracker.restore("run", Some(&metadata), &request, &[event]);
    assert_eq!(
        tracker.measurement("run", Some(&metadata), &request, &counter),
        expected
    );
}

#[test]
fn rewritten_history_retains_conservative_calibration_without_claiming_measurement() {
    let tracker = ContextUsageTracker::default();
    let metadata = metadata();
    let counter = ApproximateTokenCounter::default();
    let mut original = request();
    original.messages.push(ModelMessage::assistant(
        "long prior history".repeat(120),
        None,
    ));
    let anchor_raw = counter.count_tokens(&original);
    tracker.record_request("run", Some(&metadata), &original, &counter);
    tracker.observe_response("run", Some(&metadata), &output(anchor_raw + 400, 9000));
    let mut compacted = original.clone();
    compacted.messages = vec![ModelMessage::summary(
        "checkpoint".into(),
        "short handoff".into(),
    )];
    let raw = counter.count_tokens(&compacted);
    let actual = tracker.measurement("run", Some(&metadata), &compacted, &counter);
    assert_eq!(actual.raw_estimated_input_tokens, raw);
    assert_eq!(actual.estimated_input_tokens, raw + 400);
    assert_eq!(actual.measured_input_tokens, None);
    assert_eq!(actual.estimated_delta_tokens, raw + 400);
    assert_eq!(
        actual.estimation_margin_tokens,
        ((raw + 400) * 15).div_ceil(100)
    );
    assert_eq!(
        actual.source,
        super::super::ContextMeasurementSource::ProviderCalibrated
    );

    // An absent usage sample after a rewrite preserves the original anchor;
    // repeatedly projecting the request cannot compound the correction.
    tracker.record_request("run", Some(&metadata), &compacted, &counter);
    let mut missing = output(1, 1);
    missing.usage = None;
    tracker.observe_response("run", Some(&metadata), &missing);
    assert_eq!(
        tracker.measurement("run", Some(&metadata), &compacted, &counter),
        actual
    );
    compacted
        .messages
        .push(ModelMessage::user("Next step".into()));
    assert_eq!(
        tracker.estimate("run", Some(&metadata), &compacted, &counter),
        counter.count_tokens(&compacted) + 400
    );

    tracker.record_request("run", Some(&metadata), &compacted, &counter);
    tracker.observe_response("run", Some(&metadata), &output(200, 8000));
    let renewed = tracker.measurement("run", Some(&metadata), &compacted, &counter);
    assert_eq!(renewed.measured_input_tokens, Some(200));
    assert_eq!(renewed.estimated_delta_tokens, 0);
}

#[test]
fn rewritten_history_uses_only_identical_durable_counter_identity() {
    let metadata = metadata();
    let counter = ApproximateTokenCounter::default();
    let original = request();
    let mut event = usage_event(1, &metadata, &original, 1000);
    if let RunEvent::ModelResponseReceived { token_usage, .. } = &mut event.event {
        token_usage.as_mut().unwrap().baseline =
            request_usage_baseline_with_counter(Some(&metadata), &original, &counter);
    }
    let encoded = serde_json::to_string(&event).unwrap();
    assert!(!encoded.contains("Preserve the constraints"));
    let events = vec![serde_json::from_str(&encoded).unwrap()];
    let mut compacted = original.clone();
    compacted.messages = vec![ModelMessage::summary("checkpoint".into(), "summary".into())];
    let tracker = ContextUsageTracker::default();
    tracker.restore("run", Some(&metadata), &compacted, &events);
    assert_eq!(
        tracker
            .measurement("run", Some(&metadata), &compacted, &counter)
            .source,
        super::super::ContextMeasurementSource::ProviderCalibrated
    );
    for other in [
        ApproximateTokenCounter::conservative(),
        ApproximateTokenCounter::new(4).unwrap(),
    ] {
        assert_eq!(
            tracker
                .measurement("run", Some(&metadata), &compacted, &other)
                .source,
            super::super::ContextMeasurementSource::Estimated
        );
    }
    struct Unidentified;
    impl TokenCounter for Unidentified {
        fn count_tokens(&self, request: &ModelRequest) -> usize {
            ApproximateTokenCounter::default().count_tokens(request)
        }
    }
    assert_eq!(
        tracker
            .measurement("run", Some(&metadata), &compacted, &Unidentified)
            .source,
        super::super::ContextMeasurementSource::Estimated
    );
    // Legacy samples are still exact-prefix measurements, but cannot be used
    // as a signed-delta anchor when the original estimate was never persisted.
    let legacy = ContextUsageTracker::default();
    legacy.restore(
        "run",
        Some(&metadata),
        &compacted,
        &[usage_event(1, &metadata, &original, 1000)],
    );
    assert_eq!(
        legacy.estimate("run", Some(&metadata), &original, &counter),
        1000
    );
    assert_eq!(
        legacy
            .measurement("run", Some(&metadata), &compacted, &counter)
            .source,
        super::super::ContextMeasurementSource::Estimated
    );
}

#[test]
fn rewritten_calibration_has_a_raw_estimate_floor_and_saturates() {
    for (raw, measured, anchor, expected) in [
        (10, 20, 100, 10),
        (10, 100, 20, 90),
        (100, 50, 20, 130),
        (100, usize::MAX, 20, usize::MAX),
    ] {
        let actual = ContextMeasurement::recalibrated(raw, measured, anchor);
        assert_eq!(actual.estimated_input_tokens, expected);
        assert_eq!(actual.measured_input_tokens, None);
        assert_eq!(actual.estimated_delta_tokens, expected);
    }
}

#[test]
fn persisted_legacy_baseline_without_estimate_fields_is_readable() {
    let baseline = request_usage_baseline(Some(&metadata()), &request()).unwrap();
    let encoded = serde_json::to_string(&baseline).unwrap();
    assert!(!encoded.contains("raw_estimated_input_tokens"));
    assert!(!encoded.contains("estimator_identity"));
    let restored: RequestUsageBaseline = serde_json::from_str(&encoded).unwrap();
    assert_eq!(restored, baseline);
}
