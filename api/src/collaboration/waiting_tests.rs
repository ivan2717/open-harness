use super::*;
use crate::contracts::{ObservedTokenUsage, TimelineEvent};

struct Fixture {
    state: AppState,
    directory: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "openharness-collaboration-waiting-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let state = AppState::open_with_delay(directory.clone(), None, Duration::ZERO)
            .await
            .unwrap();
        let fixture = Self { state, directory };
        fixture.member("parent", None, RunStatus::Running);
        fixture
    }

    fn member(&self, id: &str, parent: Option<(&str, &str)>, status: RunStatus) -> Arc<RunEntry> {
        let now = time::now();
        let detail = RunDetail {
            summary: RunSummary {
                reasoning_settings_version: 1,
                reasoning_effort: None,
                model_history_through: None,
                delegation: parent.map(|(parent, name)| Delegation {
                    parent_run_id: parent.into(),
                    root_run_id: parent.into(),
                    task_name: name.into(),
                    write_paths: None,
                    model_override: None,
                    spawn_call_id: format!("spawn-{id}"),
                }),
                execution_policy: Default::default(),
                id: id.into(),
                revision: 0,
                title: id.into(),
                prompt: "Test collaboration mechanics without a model request.".into(),
                status,
                compacting: false,
                model_id: MODEL_ID.into(),
                workspace_path: None,
                execution_directory: None,
                permission_mode: Default::default(),
                created_at: now.clone(),
                updated_at: now,
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
        };
        // A persisted Running summary marks the peer active without launching
        // a model. Tests control delivery and transitions explicitly.
        let entry = self.state.make_entry(detail, 0).unwrap();
        self.state
            .inner
            .runs
            .write()
            .unwrap()
            .insert(id.into(), entry.clone());
        entry
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn decoded(output: String) -> serde_json::Value {
    serde_json::from_str(&output).unwrap()
}

#[tokio::test]
async fn unrelated_mailbox_notifications_neither_wake_nor_extend_the_wait() {
    let fixture = Fixture::new().await;
    fixture.member("child", Some(("parent", "reader")), RunStatus::Running);
    fixture.member("unrelated", None, RunStatus::Running);
    let started = Instant::now();
    let state = fixture.state.clone();
    let waiting =
        tokio::spawn(async move { state.wait_for_agent_activity("parent", "parent", 1).await });
    tokio::task::yield_now().await;
    let state = fixture.state.clone();
    let noise = tokio::spawn(async move {
        // Keep notifying past the original deadline. Restarting a one-second
        // timeout after each unrelated notification would take about 2.8 s.
        for index in 0..18 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            state
                .inner
                .mailboxes
                .send("unrelated", "unrelated-child", format!("unrelated {index}"))
                .unwrap();
        }
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    let finished_early = waiting.is_finished();
    let result = tokio::time::timeout_at(started + Duration::from_secs(2), waiting).await;
    noise.abort();
    let _ = noise.await;
    assert!(!finished_early, "unrelated mailbox delivery woke this task");
    let output = decoded(
        result
            .expect("unrelated delivery extended the deadline")
            .unwrap()
            .unwrap(),
    );
    assert_eq!(output["wait"]["outcome"], "timeout");
    assert!(output["wait"]["waited_ms"].as_u64().unwrap() >= 1000);
}

#[tokio::test]
async fn delivering_to_the_waiting_member_wakes_it_with_a_new_activity_token() {
    let fixture = Fixture::new().await;
    fixture.member("child", Some(("parent", "reader")), RunStatus::Running);
    let before = fixture
        .state
        .collaboration_progress("parent", "parent")
        .unwrap()
        .0;
    let state = fixture.state.clone();
    let waiting =
        tokio::spawn(async move { state.wait_for_agent_activity("parent", "parent", 30).await });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    fixture
        .state
        .inner
        .mailboxes
        .send("parent", "child", "Evidence ready".into())
        .unwrap();
    let output = decoded(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    assert_eq!(output["wait"]["outcome"], "message");
    assert_ne!(output["wait"]["progress_token"], before);
    assert_eq!(
        fixture
            .state
            .inner
            .mailboxes
            .pending("parent")
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn idle_peers_return_no_progress_after_their_results_have_been_delivered() {
    let fixture = Fixture::new().await;
    fixture.member("child", Some(("parent", "reader")), RunStatus::Completed);
    fixture
        .state
        .reconcile_child_results("parent")
        .await
        .unwrap();
    let messages = fixture.state.inner.mailboxes.pending("parent").unwrap();
    assert_eq!(messages.len(), 1);
    fixture
        .state
        .inner
        .mailboxes
        .acknowledge("parent", &[messages[0].id.clone()])
        .unwrap();
    let output = decoded(
        tokio::time::timeout(
            Duration::from_millis(250),
            fixture
                .state
                .wait_for_agent_activity("parent", "parent", 30),
        )
        .await
        .expect("idle peers cannot produce a future wakeup")
        .unwrap(),
    );
    assert_eq!(output["wait"]["outcome"], "no_progress");
    let child = output["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["agent_id"] == "child")
        .unwrap();
    assert_eq!(child["active"], false);
}

#[tokio::test]
async fn peer_status_changes_wake_waiters_without_requiring_a_message() {
    let fixture = Fixture::new().await;
    fixture.member("child", Some(("parent", "reader")), RunStatus::Running);
    let state = fixture.state.clone();
    let waiting =
        tokio::spawn(async move { state.wait_for_agent_activity("child", "parent", 30).await });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    fixture
        .state
        .entry("parent")
        .unwrap()
        .snapshot
        .update(|detail| {
            detail.summary.status = RunStatus::WaitingForInput;
        })
        .unwrap();
    fixture.state.inner.mailboxes.notify();
    let output = decoded(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    assert_eq!(output["wait"]["outcome"], "activity");
}

#[tokio::test]
async fn targeted_wait_ignores_other_members_without_extending_its_deadline() {
    let fixture = Fixture::new().await;
    fixture.member(
        "caller",
        Some(("parent", "coordinator")),
        RunStatus::Running,
    );
    fixture.member("target", Some(("parent", "reader")), RunStatus::Running);
    let before = fixture
        .state
        .target_collaboration_progress("caller", "parent", Some("target"))
        .unwrap()
        .0;
    let state = fixture.state.clone();
    let started = Instant::now();
    let waiting = tokio::spawn(async move {
        state
            .wait_for_target_activity("caller", "parent", 1, Some("target"))
            .await
    });
    tokio::task::yield_now().await;
    let state = fixture.state.clone();
    let noise = tokio::spawn(async move {
        for index in 0..18 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            state
                .entry("parent")
                .unwrap()
                .snapshot
                .update(|detail| {
                    detail.summary.status = if index % 2 == 0 {
                        RunStatus::WaitingForInput
                    } else {
                        RunStatus::Running
                    };
                })
                .unwrap();
            state.inner.mailboxes.notify();
        }
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    let finished_early = waiting.is_finished();
    let result = tokio::time::timeout_at(started + Duration::from_secs(2), waiting).await;
    noise.abort();
    let _ = noise.await;
    assert!(
        !finished_early,
        "another member's status woke a targeted wait"
    );
    let output = decoded(
        result
            .expect("unrelated changes extended the deadline")
            .unwrap()
            .unwrap(),
    );
    assert_eq!(output["wait"]["outcome"], "timeout");
    assert_eq!(output["wait"]["progress_token"], before);
    assert!(output["wait"]["waited_ms"].as_u64().unwrap() >= 1000);
}

#[tokio::test]
async fn idle_target_returns_no_progress_even_if_another_member_is_running() {
    let fixture = Fixture::new().await;
    fixture.member(
        "caller",
        Some(("parent", "coordinator")),
        RunStatus::Running,
    );
    fixture.member(
        "target",
        Some(("parent", "reader")),
        RunStatus::WaitingForInput,
    );
    let output = decoded(
        tokio::time::timeout(
            Duration::from_millis(250),
            fixture
                .state
                .wait_for_target_activity("caller", "parent", 30, Some("target")),
        )
        .await
        .expect("an idle target cannot advance work")
        .unwrap(),
    );
    assert_eq!(output["wait"]["outcome"], "no_progress");
}

#[tokio::test]
async fn targeted_wait_still_wakes_for_messages_from_other_members() {
    let fixture = Fixture::new().await;
    fixture.member(
        "caller",
        Some(("parent", "coordinator")),
        RunStatus::Running,
    );
    fixture.member("target", Some(("parent", "reader")), RunStatus::Running);
    let before = fixture
        .state
        .target_collaboration_progress("caller", "parent", Some("target"))
        .unwrap()
        .0;
    let state = fixture.state.clone();
    let waiting = tokio::spawn(async move {
        state
            .wait_for_target_activity("caller", "parent", 30, Some("target"))
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    fixture
        .state
        .inner
        .mailboxes
        .send("caller", "parent", "Updated requirement".into())
        .unwrap();
    let output = decoded(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    assert_eq!(output["wait"]["outcome"], "message");
    assert_ne!(output["wait"]["progress_token"], before);
}

#[tokio::test]
async fn targeted_wait_wakes_when_the_selected_member_changes_status() {
    let fixture = Fixture::new().await;
    fixture.member(
        "caller",
        Some(("parent", "coordinator")),
        RunStatus::Running,
    );
    let target = fixture.member("target", Some(("parent", "reader")), RunStatus::Running);
    let state = fixture.state.clone();
    let waiting = tokio::spawn(async move {
        state
            .wait_for_target_activity("caller", "parent", 30, Some("target"))
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    target
        .snapshot
        .update(|detail| detail.summary.status = RunStatus::WaitingForInput)
        .unwrap();
    fixture.state.inner.mailboxes.notify();
    let output = decoded(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    assert_eq!(output["wait"]["outcome"], "activity");
}

#[tokio::test]
async fn acknowledgements_and_unrelated_deliveries_do_not_advance_receipt_revision() {
    let fixture = Fixture::new().await;
    let mailboxes = &fixture.state.inner.mailboxes;
    assert_eq!(mailboxes.delivery_revision("parent").unwrap(), 0);
    let message = mailboxes
        .send("parent", "child", "One result".into())
        .unwrap();
    assert_eq!(mailboxes.delivery_revision("parent").unwrap(), 1);
    mailboxes.acknowledge("parent", &[message]).unwrap();
    mailboxes
        .send("unrelated", "other", "Other result".into())
        .unwrap();
    mailboxes.notify();
    assert_eq!(mailboxes.delivery_revision("parent").unwrap(), 1);
    mailboxes
        .completion("parent", "child", 10, "Completed".into())
        .unwrap();
    mailboxes
        .completion("parent", "child", 10, "Duplicate completion".into())
        .unwrap();
    assert_eq!(mailboxes.delivery_revision("parent").unwrap(), 2);
    let reopened = Mailboxes::open(fixture.directory.join("collaboration.json")).unwrap();
    assert_eq!(reopened.delivery_revision("parent").unwrap(), 2);
}

#[tokio::test]
async fn waiting_call_events_and_token_accounting_do_not_manufacture_progress() {
    let fixture = Fixture::new().await;
    let child = fixture.member("child", Some(("parent", "reader")), RunStatus::Running);
    let state = &fixture.state;
    let before = state.collaboration_progress("parent", "parent").unwrap().0;
    for sequence in 0..3 {
        state
            .entry("parent")
            .unwrap()
            .snapshot
            .update(|detail| {
                detail.summary.revision += 1;
                detail.events.push(TimelineEvent {
                    sequence,
                    step: sequence as u32,
                    r#type: "tool_call_succeeded".into(),
                    label: "agent_control wait".into(),
                    detail: "Waiting for a child".into(),
                    timestamp: time::now(),
                });
                detail.context.as_mut().unwrap().observed_usage = Some(ObservedTokenUsage {
                    input_tokens: (sequence as usize + 1) * 1000,
                    output_tokens: 100,
                    responses: sequence as usize + 1,
                });
            })
            .unwrap();
        child
            .snapshot
            .update(|detail| {
                detail.summary.revision += 1;
                detail.context.as_mut().unwrap().observed_usage = Some(ObservedTokenUsage {
                    input_tokens: (sequence as usize + 1) * 2000,
                    output_tokens: 100,
                    responses: sequence as usize + 1,
                });
            })
            .unwrap();
        state.inner.mailboxes.notify();
        let output = decoded(
            state
                .wait_for_agent_activity("parent", "parent", 0)
                .await
                .unwrap(),
        );
        assert_eq!(output["wait"]["outcome"], "timeout");
        assert_eq!(output["wait"]["progress_token"], before);
    }
}

#[tokio::test]
async fn stable_targets_resolve_from_membership_and_never_cross_root_boundaries() {
    let fixture = Fixture::new().await;
    let child = fixture.member("child", Some(("parent", "reader")), RunStatus::Running);
    fixture.member("other-root", None, RunStatus::Running);
    fixture.member(
        "other-child",
        Some(("other-root", "reader")),
        RunStatus::Running,
    );
    let state = &fixture.state;
    for (caller, address, expected) in [
        ("child", "parent", "parent"),
        ("child", "self", "child"),
        ("child", "root", "parent"),
        ("child", "/root", "parent"),
        ("parent", "reader", "child"),
        ("parent", "/root/reader", "child"),
        ("parent", "child", "child"),
        ("other-root", "reader", "other-child"),
    ] {
        assert_eq!(
            state.resolve_agent_target(caller, address).unwrap(),
            expected
        );
    }
    assert!(state.resolve_agent_target("parent", "parent").is_err());
    assert!(state.resolve_agent_target("child", "other-root").is_err());
    assert!(state.resolve_agent_target("child", "other-child").is_err());
    assert!(
        state
            .resolve_agent_target("child", "context:parent:17")
            .is_err()
    );
    child
        .snapshot
        .update(|detail| {
            detail.summary.title = "A renamed conversation".into();
        })
        .unwrap();
    assert_eq!(
        state.resolve_agent_target("parent", "reader").unwrap(),
        "child"
    );
    let identity = super::super::identity::instructions(&child.snapshot.get().summary);
    assert!(identity.contains("\"self\":\"/root/reader\""));
    assert!(identity.contains("\"parent\":\"/root\""));
}
