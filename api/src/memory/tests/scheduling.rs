use super::*;
use agent_memory::{WikiSourceCursor, WikiUpdate};
use tokio::time::{sleep, timeout};

async fn started(gate: &Gate) {
    timeout(Duration::from_secs(3), gate.started.notified())
        .await
        .expect("memory request should start");
}

async fn committed(host: &ExtensionHost, run: &str) {
    timeout(Duration::from_secs(3), async {
        loop {
            if host.memory.load(run).unwrap().is_some() {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("memory revision should be committed");
}

#[tokio::test]
async fn enqueue_is_immediate_and_debounce_coalesces_to_the_latest_snapshot() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let gate = Arc::new(Gate::default());
    let mut fixture = Fixture::new();
    fixture.gate = Some(gate.clone());
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let mut events = source_history("first");
    memory.schedule(input("queue", &events).as_ref(), UpdateReason::Compaction);
    user(&mut events, "latest correction");
    memory.schedule(input("queue", &events).as_ref(), UpdateReason::Progress);
    assert!(fixture.requests().is_empty());
    sleep(Duration::from_millis(50)).await;
    assert!(fixture.requests().is_empty());
    started(&gate).await;
    assert_eq!(fixture.requests().len(), 1);
    let request = payload(&fixture.requests()[0]);
    let sources = request["new_source_events"].as_array().unwrap();
    assert_eq!(sources.last().unwrap()["text"], "latest correction");
    // Scheduling more work must not wait for this blocked request.
    user(&mut events, "another correction");
    memory.schedule(input("queue", &events).as_ref(), UpdateReason::Compaction);
    gate.release.notify_one();
    committed(&host, "queue").await;
    sleep(Duration::from_millis(550)).await;
    assert_eq!(
        fixture.requests().len(),
        1,
        "background calls are rate limited"
    );
    memory.stop_background().await;
}

#[tokio::test]
async fn below_threshold_scans_do_not_delay_the_first_real_request() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let mut events = source_history("short task");
    memory.schedule(input("idle", &events).as_ref(), UpdateReason::Progress);
    sleep(Duration::from_millis(650)).await;
    assert!(fixture.requests().is_empty());
    assert!(RunMemoryView::load(&host, "idle", &events).pending);
    user(&mut events, &"response evidence ".repeat(300));
    memory.schedule(input("idle", &events).as_ref(), UpdateReason::Response);
    committed(&host, "idle").await;
    assert_eq!(fixture.requests().len(), 1);
    memory.stop_background().await;
}

#[tokio::test]
async fn catch_up_batches_exactly_to_the_boundary_and_new_notes_are_readable_immediately() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let text = "original source ".repeat(3_200);
    let mut events = source_history(&text);
    user(&mut events, "AFTER_BOUNDARY remains for later");
    let input = input("boundary", &events);
    let mut budget = CatchUpBudget::default();
    assert!(memory.ensure_coverage(input.as_ref(), 0, &mut budget).await);
    let requests = fixture.requests();
    assert!(requests.len() > 1);
    let reconstructed: String = requests
        .iter()
        .flat_map(|request| {
            payload(request)["new_source_events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|source| {
                    assert_eq!(source["sequence"], 0);
                    source["text"].as_str().unwrap().to_owned()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(reconstructed, text);
    let state = host.memory.load("boundary").unwrap().unwrap();
    assert_eq!(
        state.source_cursor,
        Some(WikiSourceCursor {
            sequence: 1,
            offset: 0
        })
    );
    let recalled = memory.messages(input.as_ref());
    assert!(
        recalled[0]
            .text()
            .contains(&format!("Wiki revision {}", state.revision))
    );
    // The latest question concerns later evidence; unrelated earlier content
    // stays immediately readable without being injected into every request.
    let page: Value = serde_json::from_str(
        &host
            .memory
            .query_validated("boundary", &events, r#"{"action":"read","page_id":"work"}"#)
            .unwrap(),
    )
    .unwrap();
    assert!(
        page["content"]
            .as_str()
            .unwrap()
            .contains("original source")
    );
    assert!(!recalled[0].text().contains("AFTER_BOUNDARY"));
    assert!(memory.ensure_coverage(input.as_ref(), 1, &mut budget).await);
    assert_eq!(fixture.requests().len(), 4);
    assert!(
        memory.messages(input.as_ref())[0]
            .text()
            .contains("AFTER_BOUNDARY")
    );
    memory.stop_background().await;
}

#[tokio::test]
async fn request_budget_is_shared_and_partial_ingestion_does_not_claim_coverage() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history(&"large source ".repeat(4_000));
    let input = input("partial", &events);
    let mut budget = CatchUpBudget::new(Duration::from_secs(2), 1);
    assert!(!memory.ensure_coverage(input.as_ref(), 0, &mut budget).await);
    let first = host.memory.load("partial").unwrap().unwrap();
    assert_eq!(first.source_cursor.unwrap().sequence, 0);
    assert!(first.source_cursor.unwrap().offset > 0);
    assert_eq!(first.through_sequence, 0);
    assert!(!memory.ensure_coverage(input.as_ref(), 0, &mut budget).await);
    assert_eq!(fixture.requests().len(), 1);
    assert!(RunMemoryView::load(&host, "partial", &events).pending);
    assert!(
        memory
            .ensure_coverage(input.as_ref(), 0, &mut CatchUpBudget::default())
            .await
    );
    assert_eq!(fixture.requests().len(), 3);
    memory.stop_background().await;
}

#[tokio::test]
async fn failures_and_cooldowns_stay_pending_without_advancing_the_cursor() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    fixture.mode.store(1, Ordering::Relaxed);
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history("remember this requirement");
    let input = input("failure", &events);
    assert!(
        !memory
            .ensure_coverage(input.as_ref(), 0, &mut CatchUpBudget::default())
            .await
    );
    assert!(
        !memory
            .ensure_coverage(input.as_ref(), 0, &mut CatchUpBudget::default())
            .await
    );
    assert_eq!(fixture.requests().len(), 1);
    assert!(host.memory.load("failure").unwrap().is_none());
    assert!(RunMemoryView::load(&host, "failure", &events).pending);
    host.set_plugin_enabled(PLUGIN_ID, false).unwrap();
    assert!(
        memory
            .ensure_coverage(input.as_ref(), 0, &mut CatchUpBudget::default())
            .await
    );
    assert_eq!(fixture.requests().len(), 1);
    memory.stop_background().await;
}

#[tokio::test]
async fn catch_up_timeout_cancels_the_request_and_never_commits_late() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let gate = Arc::new(Gate::default());
    let mut fixture = Fixture::new();
    fixture.gate = Some(gate.clone());
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history("timeout evidence");
    let input = input("timeout", &events);
    let mut budget = CatchUpBudget::new(Duration::from_millis(30), 4);
    assert!(!memory.ensure_coverage(input.as_ref(), 0, &mut budget).await);
    assert_eq!(fixture.requests().len(), 1);
    gate.release.notify_one();
    memory.stop_background().await;
    sleep(Duration::from_millis(30)).await;
    assert!(host.memory.load("timeout").unwrap().is_none());
}

#[tokio::test]
async fn foreground_takes_over_a_blocked_background_compiler() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let gate = Arc::new(Gate::default());
    let mut fixture = Fixture::new();
    fixture.gate = Some(gate.clone());
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history("same canonical source");
    let input = input("takeover", &events);
    memory.schedule(input.as_ref(), UpdateReason::Compaction);
    started(&gate).await;
    let mut budget = CatchUpBudget::new(Duration::from_secs(1), 1);
    let catch_up = memory.ensure_coverage(input.as_ref(), 0, &mut budget);
    let release_foreground = async {
        started(&gate).await;
        gate.release.notify_one();
    };
    let (ready, ()) = tokio::join!(catch_up, release_foreground);
    assert!(ready);
    assert_eq!(fixture.requests().len(), 2);
    assert_eq!(host.memory.load("takeover").unwrap().unwrap().revision, 1);
    memory.stop_background().await;
}

#[tokio::test]
async fn stop_and_drop_cancel_in_flight_jobs_without_a_late_write() {
    for drop_owner in [false, true] {
        let dir = Directory::new();
        let host = ExtensionHost::open(&dir.0).unwrap();
        let gate = Arc::new(Gate::default());
        let mut fixture = Fixture::new();
        fixture.gate = Some(gate.clone());
        let memory = WikiMemory::new(host.clone(), fixture.clone());
        let events = source_history("cancel this queued source");
        memory.schedule(input("cancel", &events).as_ref(), UpdateReason::Compaction);
        started(&gate).await;
        if drop_owner {
            drop(memory);
        } else {
            memory.stop_background().await;
        }
        gate.release.notify_one();
        sleep(Duration::from_millis(30)).await;
        assert_eq!(fixture.requests().len(), 1);
        assert!(host.memory.load("cancel").unwrap().is_none());
    }
}

#[tokio::test]
async fn disabled_queued_work_is_discarded_before_any_model_request() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history("queued before disabling");
    let input = input("disabled", &events);
    memory.schedule(input.as_ref(), UpdateReason::Compaction);
    host.set_plugin_enabled(PLUGIN_ID, false).unwrap();
    memory.abort_background();
    sleep(Duration::from_millis(600)).await;
    assert!(fixture.requests().is_empty());
    assert!(host.memory.load("disabled").unwrap().is_none());
    assert!(
        memory
            .ensure_coverage(input.as_ref(), 0, &mut CatchUpBudget::default())
            .await
    );
}

#[tokio::test]
async fn unrelated_settings_changes_cannot_turn_a_skipped_commit_into_coverage() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let gate = Arc::new(Gate::default());
    let mut fixture = Fixture::new();
    fixture.gate = Some(gate.clone());
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let events = source_history("do not lose this source on configuration changes");
    let input = input("settings", &events);
    let mut budget = CatchUpBudget::default();
    let catch_up = memory.ensure_coverage(input.as_ref(), 0, &mut budget);
    let change_settings = async {
        started(&gate).await;
        host.save(host.settings().config).unwrap();
        assert!(host.memory_enabled());
        gate.release.notify_one();
    };
    let (ready, ()) = tokio::join!(catch_up, change_settings);
    assert!(!ready);
    assert_eq!(fixture.requests().len(), 1);
    assert!(host.memory.load("settings").unwrap().is_none());
    assert!(RunMemoryView::load(&host, "settings", &events).pending);
    memory.stop_background().await;
}

#[tokio::test]
async fn coverage_skips_bookkeeping_and_validates_fingerprints_against_the_full_journal() {
    let dir = Directory::new();
    let host = ExtensionHost::open(&dir.0).unwrap();
    let fixture = Fixture::new();
    let memory = WikiMemory::new(host.clone(), fixture.clone());
    let mut events = source_history("backfill this earlier source");
    append(&mut events, RunEvent::ModelRequestStarted);
    user(&mut events, "later source previously fingerprinted");
    host.memory
        .apply_progress(
            "backfill",
            &events,
            2,
            WikiSourceCursor {
                sequence: 0,
                offset: 1,
            },
            WikiUpdate {
                overview: "Existing overview".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let input = input("backfill", &events);
    let mut budget = CatchUpBudget::default();
    assert!(memory.ensure_coverage(input.as_ref(), 0, &mut budget).await);
    assert!(memory.ensure_coverage(input.as_ref(), 1, &mut budget).await);
    assert_eq!(fixture.requests().len(), 1);
    let state = host.memory.load("backfill").unwrap().unwrap();
    assert_eq!(
        state.through_sequence, 2,
        "fingerprint boundary is not the coverage cursor"
    );
    assert_eq!(
        state.source_cursor,
        Some(WikiSourceCursor {
            sequence: 1,
            offset: 0
        })
    );
    assert!(RunMemoryView::load(&host, "backfill", &events).pending);
    memory.stop_background().await;
}
