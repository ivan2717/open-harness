use super::*;

#[test]
fn interrupted_reviews_are_not_restored_as_authority() {
    let directory =
        std::env::temp_dir().join(format!("openharness-review-records-{}", new_run_id()));
    fs::create_dir_all(&directory).unwrap();
    let records = PermissionRecords::open(&directory).unwrap();
    records
        .record(
            "root",
            ApprovalReview {
                id: "request".into(),
                run_id: "child".into(),
                operation: "read".into(),
                status: "reviewing".into(),
                reason: "pending".into(),
                model_id: "test".into(),
                created_at: time::now(),
                usage: None,
            },
        )
        .unwrap();
    let restored = PermissionRecords::open(&directory)
        .unwrap()
        .get("root")
        .unwrap();
    assert_eq!(restored.reviews[0].status, "manual");
    assert!(restored.grants.is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn saved_command_permission_remains_bound_to_conversation_and_workspace() {
    let directory =
        std::env::temp_dir().join(format!("openharness-grant-records-{}", new_run_id()));
    fs::create_dir_all(&directory).unwrap();
    let records = PermissionRecords::open(&directory).unwrap();
    records
        .change("root", |record| {
            record.workspace = Some("/project/approved".into());
            record.grants.push(CommandGrant {
                access: agent_local::CommandAccess::ReadOnly,
                network_access: agent_local::NetworkAccess::Allowed,
            });
            Ok(())
        })
        .unwrap();
    let restored = PermissionRecords::open(&directory).unwrap();
    assert!(restored.get("another-root").unwrap().grants.is_empty());
    assert_eq!(
        restored.get("root").unwrap().workspace.as_deref(),
        Some("/project/approved")
    );
    restored
        .change("root", |record| {
            record.grants.clear();
            Ok(())
        })
        .unwrap();
    assert!(
        PermissionRecords::open(&directory)
            .unwrap()
            .get("root")
            .unwrap()
            .grants
            .is_empty()
    );
    fs::remove_dir_all(directory).unwrap();
}
