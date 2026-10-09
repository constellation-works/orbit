//! `review.before_landing` [ORB-14849]: it resolves through the config layers
//! like `review.before_pr`, and a load with both on fails, because there is
//! one review layer before landing.
#![allow(clippy::expect_used, missing_docs)]

use std::path::Path;

use orbit_config::{
    ConfigRoots, ConfigScope, ConfigStore, OperationLayerSource, ResolvedConfig,
    admit_settable_config_key,
};

fn write_config(root: &Path, body: &str) {
    std::fs::write(root.join("config.toml"), body).expect("write config");
}

#[test]
fn before_landing_resolves_built_in_then_global_then_workspace_with_its_layer() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let load = || {
        ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
            .expect("config loads")
            .operation
            .review_before_landing
    };

    write_config(workspace.path(), "[scoring]\nenabled = false\n");
    let built_in = load();
    assert!(!built_in.value);
    assert_eq!(built_in.source, OperationLayerSource::BuiltIn);

    write_config(global.path(), "[review]\nbefore_landing = true\n");
    let from_global = load();
    assert!(from_global.value);
    assert_eq!(from_global.source, OperationLayerSource::Global);

    write_config(workspace.path(), "[review]\nbefore_landing = false\n");
    let from_workspace = load();
    assert!(!from_workspace.value);
    assert_eq!(from_workspace.source, OperationLayerSource::Workspace);
    admit_settable_config_key("review.before_landing").expect("live key");
}

#[test]
fn before_pr_and_before_landing_both_on_fail_the_load_naming_both_keys() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    // Across layers: each key is named with the layer that turned it on.
    write_config(global.path(), "[review]\nbefore_pr = true\n");
    write_config(workspace.path(), "[review]\nbefore_landing = true\n");
    let error = ResolvedConfig::load(&roots)
        .expect_err("two review layers before landing are refused")
        .to_string();
    assert!(
        error.contains("review.before_pr (global)")
            && error.contains("review.before_landing (workspace)"),
        "{error}"
    );

    // In one file, as `orbit config set` validates an edit.
    write_config(global.path(), "");
    write_config(
        workspace.path(),
        "[review]\nbefore_pr = true\nbefore_landing = true\n",
    );
    let error = ResolvedConfig::load(&roots)
        .expect_err("refused in one file too")
        .to_string();
    assert!(
        error.contains("review.before_pr") && error.contains("review.before_landing"),
        "{error}"
    );
}

#[test]
fn global_set_that_would_turn_on_both_review_layers_is_refused_before_saving() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let global_path = global.path().join("config.toml");

    // Each pairing: the workspace layer already has one switch on, and the
    // global edit turns on the other.
    for (workspace_key, global_key) in [
        ("before_pr", "review.before_landing"),
        ("before_landing", "review.before_pr"),
    ] {
        write_config(
            workspace.path(),
            &format!("[review]\n{workspace_key} = true\n"),
        );
        write_config(global.path(), "");

        let mut store =
            ConfigStore::open(ConfigScope::Global, global_path.clone()).expect("open global store");
        store
            .set_value(global_key, "true")
            .expect("stage global set");
        let error = store
            .validate_global_for_set(global_key, workspace.path())
            .expect_err("a global set that conflicts with the workspace layer is refused")
            .to_string();
        assert!(
            error.contains(&format!("review.{workspace_key} (workspace)"))
                && error.contains(&format!("{global_key} (global)")),
            "{error}"
        );

        // Refused before `save`, so the workspace still loads.
        assert_eq!(
            std::fs::read_to_string(&global_path).expect("read global"),
            ""
        );
        ResolvedConfig::load(&roots).expect("workspace config stays loadable");

        // The other switch off in the workspace layer is not a conflict.
        write_config(
            workspace.path(),
            "[review]\nbefore_pr = false\nbefore_landing = false\n",
        );
        store
            .validate_global_for_set(global_key, workspace.path())
            .expect("no workspace switch is on");
    }

    // With one pinned root there is no workspace layer; the one-file check still applies.
    write_config(global.path(), "[review]\nbefore_pr = true\n");
    let mut store = ConfigStore::open(ConfigScope::Global, global_path).expect("open global store");
    store
        .set_value("review.before_landing", "true")
        .expect("stage global set");
    let error = store
        .validate_global_for_set("review.before_landing", global.path())
        .expect_err("one file alone still refuses both on")
        .to_string();
    assert!(error.contains("review.before_landing"), "{error}");
}
