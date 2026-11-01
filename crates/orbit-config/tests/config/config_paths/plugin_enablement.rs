//! `[plugin_enablement]`: workspace-only, validated, and never a policy layer.

use orbit_config::{
    ConfigRoots, ResolvedConfig, load_workspace_plugin_enablement, workspace_config_sets_policy,
};

use tempfile::tempdir;

fn write_config(root: &std::path::Path, body: &str) {
    std::fs::write(root.join("config.toml"), body).expect("write config");
}

const PERMISSIVE_GLOBAL: &str = r#"
[execution.codex]
sandbox = "danger-full-access"

[execution.env]
pass = ["GLOBAL_TOKEN"]
"#;

#[test]
fn global_plugin_enablement_is_refused_at_load() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        "[machine]\nname = \"owner-host\"\n\n[plugin_enablement]\ngraph = false\n",
    );

    let error = ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect_err("a global toggle table is refused");
    // [ORB-20766] The refusal is a typed host-configuration error naming the
    // host and file, not the caller's invalid_input: a follower whose remote
    // call the owner serves did nothing wrong.
    assert!(
        matches!(error, orbit_common::OrbitError::HostConfiguration { .. }),
        "{error:?}"
    );
    assert!(error.is_host_configuration(), "{error:?}");
    let (host, path, message) = error
        .host_configuration_details()
        .expect("host configuration details");
    assert_eq!(host, "owner-host");
    assert!(path.ends_with("config.toml"), "{path}");
    assert!(message.contains("[plugin_enablement]"), "{message}");
    assert!(message.contains("--scope workspace"), "{message}");

    // The wire form a follower receives, and the final-recovery
    // classification of the ORB-20736 tag-instructions failure.
    let remote = orbit_common::OrbitError::RemoteTool {
        code: orbit_common::RemoteToolCode::from("host_configuration"),
        message: error.to_string(),
        payload: serde_json::json!({}),
    };
    assert!(remote.is_host_configuration(), "{remote:?}");
    assert_eq!(
        orbit_types::workflow::TransientInfrastructure::of_action_failure(
            "agent_implement",
            &format!("load tag instructions for `agent_implement`: {remote}"),
        ),
        Some(orbit_types::workflow::TransientInfrastructure::HostConfiguration)
    );
    // A plain invalid_input remote failure is not infrastructure.
    assert_eq!(
        orbit_types::workflow::TransientInfrastructure::of_action_failure(
            "agent_implement",
            "load tag instructions: remote tool failed (invalid_input): bad request",
        ),
        None
    );

    // The doctor-facing probe finds the same table without loading config.
    let misplaced = orbit_config::misplaced_global_plugin_enablement(
        &global.path().join("config.toml"),
    )
    .expect("probe")
    .expect("misplaced");
    assert_eq!(misplaced.host, "owner-host");

    // A global-only runtime reads no workspace layer, so it has no toggles
    // even when the one file it reads is the global file.
    let only = tempdir().expect("root tempdir");
    assert!(
        load_workspace_plugin_enablement(&ConfigRoots::global_only(only.path()))
            .expect("toggles")
            .is_empty()
    );
}

#[test]
fn a_toggle_only_workspace_file_keeps_the_security_keys_inherited() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(global.path(), PERMISSIVE_GLOBAL);
    write_config(workspace.path(), "[plugin_enablement]\ngraph = false\n");

    let config =
        ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path())).expect("load");
    assert_eq!(config.codex_execution.sandbox(), "danger-full-access");
    assert!(
        config
            .snapshot
            .execution_env_pass
            .contains(&"GLOBAL_TOKEN".to_string())
    );
    assert!(!workspace_config_sets_policy(
        &workspace.path().join("config.toml")
    ));

    // Any real setting makes the file a policy layer again, and a file with
    // no toggle table stays one however little it holds.
    for body in [
        "[decision_channel]\nenabled = true\n[plugin_enablement]\ngraph = false\n",
        "# operator notes\n",
    ] {
        write_config(workspace.path(), body);
        let config =
            ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path())).expect("load");
        assert_eq!(
            config.codex_execution.sandbox(),
            "workspace-write",
            "{body}"
        );
        assert!(
            workspace_config_sets_policy(&workspace.path().join("config.toml")),
            "{body}"
        );
    }
}
