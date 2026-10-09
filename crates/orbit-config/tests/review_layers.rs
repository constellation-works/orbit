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

#[test]
fn global_set_that_breaks_a_cross_layer_throttle_rule_is_refused_before_saving() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let global_path = global.path().join("config.toml");
    let original = "[workflow.resource_throttle]\ncpu_resume_percent = 70\n";
    write_config(global.path(), original);
    write_config(
        workspace.path(),
        "[workflow.resource_throttle]\ncpu_high_percent = 80\n",
    );
    ResolvedConfig::load(&roots).expect("the layers load before the edit");

    // Valid in the global file alone, but not beside the workspace's high.
    let key = "workflow.resource_throttle.cpu_resume_percent";
    let mut store =
        ConfigStore::open(ConfigScope::Global, global_path.clone()).expect("open global store");
    store.set_value(key, "85").expect("stage global set");
    store
        .validate_for_set(key)
        .expect("the global file alone admits the value");
    let error = store
        .validate_global_for_set(key, workspace.path())
        .expect_err("a global set that breaks the workspace's layered load is refused")
        .to_string();
    assert!(
        error.contains("cpu_resume_percent must be less than cpu_high_percent"),
        "{error}"
    );
    store
        .validate_global_with_workspace(workspace.path())
        .expect_err("the layered check alone refuses it too");

    // Refused before `save`: the global file is untouched and the workspace loads.
    assert_eq!(
        std::fs::read_to_string(&global_path).expect("read global"),
        original
    );
    ResolvedConfig::load(&roots).expect("workspace config stays loadable");

    // A value below the workspace's high is admitted and saves.
    store.set_value(key, "60").expect("stage a lower value");
    store
        .validate_global_for_set(key, workspace.path())
        .expect("resume below the workspace high is admitted");
    store.save().expect("save");
    ResolvedConfig::load(&roots).expect("still loadable after the admitted write");
}

#[test]
fn global_edit_that_removes_a_crew_a_workspace_names_is_refused_before_saving() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let global_path = global.path().join("config.toml");
    let original = "[crews.terra]\nmodel = \"m\"\nprovider = \"codex\"\n";
    write_config(global.path(), original);
    write_config(workspace.path(), "[workflow]\ndefault_crew = \"terra\"\n");
    ResolvedConfig::load(&roots).expect("the workspace names a global crew");

    let mut store =
        ConfigStore::open(ConfigScope::Global, global_path.clone()).expect("open global store");
    assert!(
        store
            .remove_crew_table("terra")
            .expect("stage crew removal")
    );
    store
        .validate()
        .expect("the global file alone has no crew to miss");
    store
        .validate_global_with_workspace(workspace.path())
        .expect_err("removing the crew the workspace default names is refused");

    assert_eq!(
        std::fs::read_to_string(&global_path).expect("read global"),
        original
    );
    ResolvedConfig::load(&roots).expect("workspace config stays loadable");
}

#[test]
fn global_validation_with_one_pinned_root_has_no_workspace_layer_to_check() {
    let root = tempfile::tempdir().expect("root tempdir");
    let global_path = root.path().join("config.toml");
    write_config(root.path(), "");
    let mut store = ConfigStore::open(ConfigScope::Global, global_path).expect("open global store");
    store
        .set_value("workflow.resource_throttle.cpu_resume_percent", "85")
        .expect("stage global set");
    store
        .validate_global_with_workspace(root.path())
        .expect("one pinned root has no second layer");
}
