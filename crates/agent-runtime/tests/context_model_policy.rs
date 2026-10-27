use agent_runtime::{
    ContextPolicy, ContextPolicyError, ModelContextBudget, ModelContextMetadata,
    ModelContextMetadataError,
};

fn model(window: usize) -> ModelContextMetadata {
    ModelContextMetadata::new("test-provider", "test-model", window).unwrap()
}

#[test]
fn million_token_model_scales_the_recent_tail_with_available_input_space() {
    let policy = ContextPolicy::default()
        .resolve(Some(&model(1_000_000)))
        .unwrap();
    assert_eq!(policy.model_budget().context_window(), 1_000_000);
    assert_eq!(policy.input_token_limit(), 950_000);
    assert_eq!(policy.compaction_threshold(), 900_000);
    assert_eq!(policy.compaction_target(), 675_000);
    assert_eq!(policy.recent_tail_tokens(), 152_000);
    assert_eq!(policy.output_token_limit(), Some(8_192));
}

#[test]
fn maximum_window_requires_an_explicit_override_and_caps_excessive_overrides() {
    let mut metadata = model(272_000);
    metadata.max_context_window = Some(872_000);
    let policy = ContextPolicy::default().resolve(Some(&metadata)).unwrap();
    assert_eq!(policy.model_budget().context_window(), 272_000);
    assert_eq!(policy.input_token_limit(), 258_400);
    assert_eq!(policy.compaction_threshold(), 244_800);

    let override_policy = ContextPolicy::builder()
        .model_context_window(1_000_000)
        .build_for_model(&metadata)
        .unwrap();
    assert_eq!(override_policy.model_budget().context_window(), 872_000);
    assert_eq!(override_policy.compaction_threshold(), 784_800);

    metadata.max_context_window = None;
    let known_cap = override_policy.resolve(Some(&metadata)).unwrap();
    assert_eq!(known_cap.model_budget().context_window(), 272_000);
}

#[test]
fn resolving_another_model_does_not_turn_derived_values_into_overrides() {
    let original = ContextPolicy::default();
    let large = original.resolve(Some(&model(1_000_000))).unwrap();
    let small = large.resolve(Some(&model(128_000))).unwrap();
    assert_eq!(small.compaction_threshold(), 115_200);
    assert_eq!(small.compaction_target(), 86_400);
    assert_eq!(large.resolve(None).unwrap(), original);
}

#[test]
fn caller_overrides_are_independent_of_builder_order_and_survive_model_changes() {
    let first = ContextPolicy::builder()
        .compaction_threshold(120_000)
        .compaction_target(80_000)
        .recent_tail_tokens(16_000)
        .model_context_window(200_000)
        .output_token_reserve(4_000)
        .safety_margin(1_000)
        .build()
        .unwrap();
    let second = ContextPolicy::builder()
        .model_context_window(200_000)
        .safety_margin(1_000)
        .output_token_reserve(4_000)
        .recent_tail_tokens(16_000)
        .compaction_target(80_000)
        .compaction_threshold(120_000)
        .build()
        .unwrap();
    assert_eq!(first, second);
    let resolved = first.resolve(Some(&model(1_000_000))).unwrap();
    assert_eq!(resolved.model_budget().context_window(), 200_000);
    assert_eq!(resolved.input_token_limit(), 190_000);
    assert_eq!(resolved.output_token_limit(), Some(4_000));
    assert_eq!(resolved.compaction_threshold(), 120_000);
    assert_eq!(resolved.compaction_target(), 80_000);
    assert_eq!(resolved.recent_tail_tokens(), 16_000);
}

#[test]
fn advertised_trigger_is_capped_by_automatic_percentage_and_input_budget() {
    let mut metadata = model(1_000_000);
    metadata.auto_compact_token_limit = Some(600_000);
    let policy = ContextPolicy::default().resolve(Some(&metadata)).unwrap();
    assert_eq!(policy.compaction_threshold(), 600_000);
    metadata.auto_compact_token_limit = Some(990_000);
    assert_eq!(
        policy
            .resolve(Some(&metadata))
            .unwrap()
            .compaction_threshold(),
        900_000
    );
    metadata.effective_context_window_percent = 80;
    assert_eq!(
        policy
            .resolve(Some(&metadata))
            .unwrap()
            .compaction_threshold(),
        800_000
    );
}

#[test]
fn explicit_threshold_is_not_silently_changed_when_the_model_is_too_small() {
    let policy = ContextPolicy::builder()
        .model_context_window(1_000_000)
        .compaction_threshold(900_000)
        .build()
        .unwrap();
    assert!(matches!(
        policy.resolve(Some(&model(128_000))),
        Err(ContextPolicyError::ThresholdExceedsInputLimit {
            threshold: 900_000,
            input_token_limit: 117_760,
        })
    ));
    assert!(matches!(
        ContextPolicy::builder()
            .input_token_limit(100)
            .compaction_threshold(101)
            .build(),
        Err(ContextPolicyError::ThresholdExceedsInputLimit { .. })
    ));
}

#[test]
fn output_cap_and_reservations_apply_without_double_counting_headroom() {
    let mut metadata = model(1_000_000);
    metadata.max_output_tokens = Some(2_000);
    let policy = ContextPolicy::default().resolve(Some(&metadata)).unwrap();
    assert_eq!(policy.output_token_limit(), Some(2_000));
    assert_eq!(policy.input_token_limit(), 950_000);

    let policy = ContextPolicy::builder()
        .model_budget(ModelContextBudget::new(1_000_000, 200_000, 10_000).unwrap())
        .build_for_model(&model(1_000_000))
        .unwrap();
    assert_eq!(policy.input_token_limit(), 790_000);
    assert_eq!(policy.compaction_threshold(), 790_000);

    let policy = ContextPolicy::builder()
        .model_budget(
            ModelContextBudget::new(1_000_000, 8_192, 2_048)
                .unwrap()
                .with_effective_context_window_percent(80)
                .unwrap(),
        )
        .build_for_model(&metadata)
        .unwrap();
    assert_eq!(policy.input_token_limit(), 800_000);
    assert_eq!(policy.output_token_limit(), Some(2_000));

    let standalone_budget = ModelContextBudget::new(1_000_000, 8_192, 2_048).unwrap();
    let standalone = ContextPolicy::builder()
        .model_budget(standalone_budget)
        .model_context_window(2_000_000)
        .build()
        .unwrap();
    assert_eq!(
        standalone.model_budget().effective_context_window_percent(),
        100
    );
    assert_eq!(standalone.input_token_limit(), 1_989_760);
}

#[test]
fn unknown_models_use_a_million_token_fallback_and_retain_input_only_compatibility() {
    let fallback = ContextPolicy::default().resolve(None).unwrap();
    assert_eq!(fallback.model_budget().context_window(), 1_000_000);
    assert_eq!(fallback.input_token_limit(), 950_000);
    assert_eq!(fallback.compaction_threshold(), 900_000);
    assert_eq!(fallback.compaction_target(), 675_000);
    assert_eq!(fallback.recent_tail_tokens(), 152_000);
    assert_eq!(fallback.output_token_limit(), Some(8_192));
    assert_eq!(fallback.model_budget().safety_margin(), 2_048);

    let small = ContextPolicy::builder()
        .input_token_limit(100)
        .build()
        .unwrap();
    assert_eq!(small.input_token_limit(), 100);
    assert_eq!(small.compaction_threshold(), 90);
    assert_eq!(small.recent_tail_tokens(), 16);
    assert_eq!(small.output_token_limit(), None);
}

#[test]
fn small_windows_have_typed_errors_and_defaults_can_shrink_with_explicit_reservations() {
    assert!(matches!(
        ContextPolicy::default().resolve(Some(&model(4_096))),
        Err(ContextPolicyError::ModelBudget(_))
    ));
    let small = ContextPolicy::builder()
        .output_token_reserve(512)
        .safety_margin(128)
        .build_for_model(&model(4_096))
        .unwrap();
    assert_eq!(small.input_token_limit(), 3_456);
    assert_eq!(small.compaction_threshold(), 3_456);
    assert_eq!(small.recent_tail_tokens(), 552);
    assert!(
        ContextPolicy::builder()
            .input_token_limit(1)
            .build()
            .is_err()
    );
}

#[test]
fn maximum_integer_windows_do_not_overflow_percentage_calculation() {
    let policy = ContextPolicy::default()
        .resolve(Some(&model(usize::MAX)))
        .unwrap();
    assert_eq!(
        policy.input_token_limit(),
        usize::MAX / 100 * 95 + usize::MAX % 100 * 95 / 100
    );
    assert_eq!(
        policy.compaction_threshold(),
        usize::MAX / 100 * 90 + usize::MAX % 100 * 90 / 100
    );
    assert!(policy.compaction_target() < policy.compaction_threshold());
}

#[test]
fn invalid_metadata_is_rejected_before_policy_resolution() {
    assert!(matches!(
        ModelContextMetadata::new("", "model", 100_000),
        Err(ModelContextMetadataError::EmptyIdentity)
    ));
    assert!(matches!(
        ModelContextMetadata::new("provider", "model", 0),
        Err(ModelContextMetadataError::ZeroContextWindow)
    ));
    let mut metadata = model(128_000);
    metadata.max_context_window = Some(100_000);
    assert!(matches!(
        ContextPolicy::default().resolve(Some(&metadata)),
        Err(ContextPolicyError::ModelMetadata(
            ModelContextMetadataError::MaximumBelowDefault { .. }
        ))
    ));
    metadata.max_context_window = None;
    for percent in [0, 101] {
        metadata.effective_context_window_percent = percent;
        assert!(matches!(
            metadata.validate(),
            Err(ModelContextMetadataError::InvalidEffectiveWindowPercent(_))
        ));
    }
    metadata.effective_context_window_percent = 95;
    metadata.auto_compact_token_limit = Some(0);
    assert!(matches!(
        metadata.validate(),
        Err(ModelContextMetadataError::ZeroAutoCompactTokenLimit)
    ));
    metadata.auto_compact_token_limit = None;
    metadata.max_output_tokens = Some(0);
    assert!(matches!(
        metadata.validate(),
        Err(ModelContextMetadataError::ZeroMaxOutputTokens)
    ));
}

#[test]
fn recent_tail_ratio_uses_available_messages_and_explicit_tokens_win() {
    let policy = ContextPolicy::builder()
        .input_token_limit(10_000)
        .build()
        .unwrap();
    assert_eq!(policy.recent_tail_tokens_for_message_budget(8_000), 1_280);
    let ratio = ContextPolicy::builder()
        .input_token_limit(10_000)
        .recent_tail_percent(20)
        .build()
        .unwrap();
    assert_eq!(ratio.recent_tail_tokens_for_message_budget(8_000), 1_600);
    let fixed = ContextPolicy::builder()
        .input_token_limit(10_000)
        .recent_tail_percent(20)
        .recent_tail_tokens(500)
        .build()
        .unwrap();
    assert_eq!(fixed.recent_tail_tokens_for_message_budget(8_000), 500);
    for invalid in [0, 100, 255] {
        assert!(matches!(
            ContextPolicy::builder()
                .recent_tail_percent(invalid)
                .build(),
            Err(ContextPolicyError::InvalidRecentTailPercent(_))
        ));
    }
}
