use super::*;

#[test]
fn call_time_fs_write_refuses_protected_global_paths_but_allows_plugin_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let root = global_root.join("plugins/demo/1.0.0");
    let state_dir = global_root.join("state/plugins/demo");
    std::fs::create_dir_all(&root).expect("plugin root");
    std::fs::create_dir_all(&state_dir).expect("plugin state");

    let mut covering = PluginPermissions::default();
    covering.fs.write = vec!["{{plugin_root}}".into()];
    let mut covering_spec = (*spec(root.join("bin"), &root, covering, &[PluginGrant::Fs])).clone();
    covering_spec.global_root.clone_from(&global_root);
    covering_spec.state_dir.clone_from(&state_dir);
    let error = covering_spec.sandbox_profile(None).unwrap_err();
    // The sandbox boundary refusing a covering write is a policy refusal —
    // it must reach a caller as `PolicyDenied`, not `InvalidInput`, so an
    // agent can tell "your call was refused by policy" from "you sent
    // malformed input" [ORB-12837].
    assert!(
        matches!(error, orbit_common::OrbitError::PolicyDenied(_)),
        "{error:?}"
    );
    let error = error.to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]") && error.contains("plugin install root"),
        "{error}"
    );

    for declared in [
        global_root.join("bin").to_string_lossy().into_owned(),
        global_root
            .join("plugins/.grants")
            .to_string_lossy()
            .into_owned(),
        global_root
            .join("plugins/other/1.0.0")
            .to_string_lossy()
            .into_owned(),
        "{{plugin_state}}/../../../plugins/.grants".to_string(),
    ] {
        let mut permissions = PluginPermissions::default();
        permissions.fs.write = vec![declared.clone()];
        let mut protected =
            (*spec(root.join("bin"), &root, permissions, &[PluginGrant::Fs])).clone();
        protected.global_root.clone_from(&global_root);
        protected.state_dir.clone_from(&state_dir);
        let error = protected.sandbox_profile(None).unwrap_err().to_string();
        assert!(
            error.contains("spec.permissions.fs.write[0]")
                && error.contains("protected path beneath Orbit global root"),
            "{declared}: {error}"
        );
    }

    let mut deferred_permissions = PluginPermissions::default();
    deferred_permissions.fs.write = vec!["{{workspace}}/plugins/.grants".into()];
    let mut deferred = (*spec(
        root.join("bin"),
        &root,
        deferred_permissions,
        &[PluginGrant::Fs],
    ))
    .clone();
    deferred.global_root.clone_from(&global_root);
    deferred.state_dir.clone_from(&state_dir);
    let error = deferred
        .sandbox_profile(Some(&global_root))
        .expect_err("a deferred workspace path into the global root must be refused")
        .to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]")
            && error.contains("protected path beneath Orbit global root"),
        "{error}"
    );

    let mut allowed = (*spec(
        root.join("bin"),
        &root,
        fs_state_permissions(),
        &[PluginGrant::Fs],
    ))
    .clone();
    allowed.global_root = global_root;
    allowed.state_dir.clone_from(&state_dir);
    let profile = allowed
        .sandbox_profile(None)
        .expect("the current plugin_state tree remains writable");
    assert_eq!(profile.write, vec![state_dir]);
}

/// A declared write root whose tail does not exist yet is judged where its
/// existing ancestors physically live, so an alias a backend planted inside
/// its own writable state cannot present the plugin's protected install
/// namespace as plugin state. The refusal lands on `sandbox_profile`, which
/// is what both platforms compile their write rules from, and it lands
/// before anything is created or any rule is bound [ORB-12799].
#[cfg(unix)]
#[test]
fn a_call_time_write_tail_below_a_state_symlink_is_refused_before_creation() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let install_root = global_root.join("plugins/demo");
    let root = install_root.join("1.0.0");
    let state_dir = global_root.join("state/plugins/demo");
    std::fs::create_dir_all(&root).expect("plugin root");
    std::fs::create_dir_all(&state_dir).expect("plugin state");
    symlink(&install_root, state_dir.join("alias")).expect("state alias");

    let mut permissions = PluginPermissions::default();
    permissions.fs.write = vec!["{{plugin_state}}/alias/9.0.0".into()];
    let mut aliased = (*spec(root.join("bin"), &root, permissions, &[PluginGrant::Fs])).clone();
    aliased.global_root.clone_from(&global_root);
    aliased.state_dir.clone_from(&state_dir);

    let error = aliased
        .sandbox_profile(None)
        .expect_err("an alias into the install namespace must be refused")
        .to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]")
            && error.contains("protected path beneath Orbit global root"),
        "{error}"
    );
    assert!(
        !install_root.join("9.0.0").exists(),
        "no version tree is materialised inside the protected install namespace"
    );

    // An absent directory inside the real state tree still compiles.
    let mut allowed_permissions = PluginPermissions::default();
    allowed_permissions.fs.write = vec!["{{plugin_state}}/cache/runs".into()];
    let mut allowed = (*spec(
        root.join("bin"),
        &root,
        allowed_permissions,
        &[PluginGrant::Fs],
    ))
    .clone();
    allowed.global_root.clone_from(&global_root);
    allowed.state_dir.clone_from(&state_dir);
    let profile = allowed
        .sandbox_profile(None)
        .expect("an absent directory inside plugin state stays writable");
    assert_eq!(profile.write, vec![state_dir.join("cache/runs")]);
}

#[test]
fn env_pass_cannot_forward_a_privilege_bearing_orbit_name_even_if_requested() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let permissions = PluginPermissions {
        env_pass: vec!["ORBIT_OPERATOR".into(), "DATABASE_URL".into()],
        ..PluginPermissions::default()
    };
    let granted = spec(
        root.join("bin"),
        &root,
        permissions,
        &[PluginGrant::EnvPass],
    );

    // Set in the real process env for the duration of this test (the
    // process-locked, race-free way this crate does that — see
    // `orbit_common::test_env`), simulating an operator session
    // (`ORBIT_OPERATOR=1`) whose plugin call requests it by name via
    // `env_pass`. The whole point of ORB-12768 is that it must not survive
    // that request even though it is genuinely present in the parent.
    let _env_guard = orbit_common::test_env::scoped([
        ("ORBIT_OPERATOR", Some("1")),
        (
            "DATABASE_URL",
            Some("postgres://svc:hunter2@db.internal/prod"),
        ),
    ]);
    let ctx = context(temp.path());

    let env = granted.child_environment(&ctx, "/tmp", None);

    assert!(
        !env.iter().any(|(key, _)| key == "ORBIT_OPERATOR"),
        "a privilege-bearing ORBIT_* name must never reach a plugin child, requested or not: {env:?}"
    );
    assert_eq!(
        env.iter()
            .find(|(key, _)| key == "DATABASE_URL")
            .map(|(_, value)| value.as_str()),
        Some("postgres://svc:hunter2@db.internal/prod"),
        "an ordinary requested name is still forwarded"
    );
}
