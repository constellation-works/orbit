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

/// Prints `<path>=<contents or denied>` for each argument, joined by `|`.
#[cfg(unix)]
const STORE_PROBE_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nseen=\nfor path in \"$@\"; do\n  body=$(cat \"$path\" 2>/dev/null) || body=denied\n  seen=\"$seen${seen:+|}$body\"\ndone\nprintf '{\"ok\":true,\"output\":{\"seen\":\"%s\"}}\\n' \"$seen\"\n";

/// The decision-channel store mirrors every channel's token key, in
/// `channels.json` and in the staged file a registration renames over it. An
/// unrelated plugin holding `orbit_tools` is granted the whole global root,
/// so the store must be carved out of that grant as the plugin secret store
/// is, on the direct and the brokered launch path alike, and on both
/// platforms' read boundaries [ORB-19338].
#[cfg(unix)]
#[test]
fn an_orbit_tools_plugin_cannot_read_the_decision_channel_key_store() {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    use orbit_tools::plugin::{
        BrokeredCaller, DECISION_CHANNEL_STORE_DIR, PLUGIN_SECRET_STORE_DIR,
    };
    use orbit_types::policy::ResolvedFsProfile;

    use super::support::stub_backend;

    let temp = tempfile::tempdir().expect("tempdir");
    // Physical, so a macOS `/var` alias cannot split a rule from its path.
    let base = temp.path().canonicalize().expect("canonical tempdir");
    let global_root = base.join("relocated/orbit-global");
    let store = global_root.join(DECISION_CHANNEL_STORE_DIR);
    let canaries = [
        (store.join("channels.json"), "CHANNEL-STORE-CANARY"),
        (
            store.join(".channels.json.0123abcd.tmp"),
            "STAGED-STORE-CANARY",
        ),
        (
            global_root
                .join(PLUGIN_SECRET_STORE_DIR)
                .join("decision-mail.json"),
            "PLUGIN-SECRET-CANARY",
        ),
    ];
    for (path, canary) in &canaries {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().expect("parent"))
            .expect("private store dir");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| file.write_all(canary.as_bytes()))
            .expect("synthetic key-bearing file");
    }
    let plugin_root = global_root.join("plugins/demo/1.0.0");
    let worktree = base.join("checkout");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let mut unrelated = (*spec(
        stub_backend(&plugin_root, STORE_PROBE_BACKEND),
        &plugin_root,
        PluginPermissions::default(),
        &[PluginGrant::OrbitTools],
    ))
    .clone();
    unrelated.global_root.clone_from(&global_root);
    unrelated.state_dir = global_root.join("state/plugins/demo");
    unrelated.args = canaries
        .iter()
        .map(|(path, _)| path.to_string_lossy().into_owned())
        .collect();
    let caller = BrokeredCaller {
        worktree: worktree.clone(),
        fs_profile: ResolvedFsProfile {
            name: "implementer".to_string(),
            read: vec!["**".to_string()],
            modify: Vec::new(),
        },
        proc_allowed_programs: Vec::new(),
        proc_disallowed_programs: None,
    };

    for (launch, profile) in [
        ("direct", unrelated.sandbox_profile(None)),
        ("brokered", unrelated.brokered_sandbox_profile(&caller)),
    ] {
        let profile = profile.unwrap_or_else(|error| panic!("{launch} profile: {error}"));
        assert!(
            profile.read.contains(&global_root),
            "{launch}: orbit_tools is granted the global root, the reproduced exposure: {:?}",
            profile.read
        );
        assert!(
            profile.read_denies.contains(&store),
            "{launch}: the decision-channel store is a denied tree: {:?}",
            profile.read_denies
        );
        assert!(
            !profile
                .readable_denied_files()
                .iter()
                .chain(&profile.readable_denied_trees())
                .any(|path| path.starts_with(&store)),
            "{launch}: nothing inside the store is granted back"
        );

        // The macOS read boundary, compiled the way the macOS spawn does: the
        // store's deny is emitted and no later rule re-allows inside it.
        let mut sbpl =
            orbit_exec::compile_macos_sandbox_profile(&profile.macos_fs_rules(), "plugin")
                .expect("compile SBPL");
        orbit_exec::append_macos_read_boundary(
            &mut sbpl,
            &profile.read_denies,
            &profile.readable_denied_trees(),
            &profile.readable_denied_files(),
        );
        let store_rule = format!("\"{}", store.display());
        let deny = format!("(deny file-read* (subpath \"{}\"))", store.display());
        let deny_at = sbpl
            .lines()
            .position(|line| line == deny)
            .unwrap_or_else(|| panic!("{launch}: SBPL denies the store:\n{sbpl}"));
        assert!(
            !sbpl
                .lines()
                .skip(deny_at + 1)
                .any(|line| line.starts_with("(allow") && line.contains(&store_rule)),
            "{launch}: no later SBPL rule re-allows the store:\n{sbpl}"
        );
    }

    #[cfg(target_os = "linux")]
    {
        use orbit_tools::Tool;

        use super::support::tool;

        let probe = orbit_exec::probe_landlock();
        if !probe.available {
            // Reported as the orbit-exec Landlock suite does, never as a pass
            // of the native boundary.
            #[allow(clippy::print_stdout)]
            {
                println!("skipping the native Landlock run: {}", probe.detail);
            }
            return;
        }
        let backend = tool(std::sync::Arc::new(unrelated), None);
        let direct = context(&worktree);
        let brokered = orbit_tools::ToolContext {
            brokered_caller: Some(caller),
            ..direct.clone()
        };
        for (launch, ctx) in [("direct", direct), ("brokered", brokered)] {
            let output = backend
                .execute(&ctx, serde_json::json!({}))
                .unwrap_or_else(|error| panic!("{launch} confined probe: {error}"));
            assert_eq!(
                output["seen"], "denied|denied|denied",
                "{launch}: the confined child reads neither the channel store, its staged \
                 write, nor the plugin secret control"
            );
        }
    }
}
