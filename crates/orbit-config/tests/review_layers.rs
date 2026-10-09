//! `review.before_landing` [ORB-14849]: it resolves through the config layers
//! like `review.before_pr`, and a load with both on fails, because there is
//! one review layer before landing.
#![allow(clippy::expect_used, missing_docs)]

use std::path::Path;

use orbit_config::{ConfigRoots, OperationLayerSource, ResolvedConfig, admit_settable_config_key};

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
