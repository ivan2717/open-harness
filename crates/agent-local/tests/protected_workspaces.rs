use agent_local::{LocalTools, PermissionMode};

#[test]
fn home_directories_are_rejected_including_windows_userprofile() {
    let state = std::env::temp_dir().join(format!("openharness-home-guard-{}", std::process::id()));
    for home in ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
    {
        let home = std::fs::canonicalize(home).unwrap();
        assert!(
            LocalTools::open(home, state.clone(), PermissionMode::ReadOnly).is_err(),
            "a home directory must not be offered as an unrestricted workspace"
        );
    }
    assert!(
        !state.exists(),
        "the guard should run before workspace setup"
    );
}
