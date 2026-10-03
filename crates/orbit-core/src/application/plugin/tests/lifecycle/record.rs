//! The verified install path every lifecycle verb checks first.

use std::path::Path;

use orbit_common::OrbitError;

use super::super::super::{
    PluginAddOptions, PluginEnableOptions, PluginRemoveOptions, disable_plugin, enable_plugin,
    install_plugin, list_plugins, remove_plugin,
};
use super::super::fixture::{PluginFixture, PluginSpecFixture};

/// The `plugins` row is writable by any backend holding `orbit_tools`, so a
/// lifecycle verb may not act on the path it records without checking it
/// first. `remove` is the dangerous one — the loader's own refusal used to
/// send the operator straight into `remove_dir_all` of whatever the row named
/// — but `enable` seeds from that tree and `disable` selects discovery links
/// by it, so all three refuse together [ORB-12800].
#[test]
fn a_relocated_row_is_refused_by_every_lifecycle_verb_and_leaves_that_tree_alone() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "a_relocated_row_is_refused_by_every_lifecycle_verb_and_leaves_that_tree_alone",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");

    // An operator directory Orbit never installed anything into.
    let sentinel = fixture.repo_root.join("unrelated");
    std::fs::create_dir_all(&sentinel).expect("create the sentinel tree");
    let keep = sentinel.join("keep.txt");
    std::fs::write(&keep, "operator data").expect("write the sentinel file");

    let runtime = fixture.reopen();
    let mut installed = runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    let install_path = installed.install_path.clone();
    installed.install_path = sentinel.to_string_lossy().into_owned();
    runtime
        .stores()
        .plugins()
        .upsert_plugin(&installed)
        .expect("the row write succeeds; the lifecycle verbs are what refuse it");

    let expected = runtime.global_root().join("plugins/demo");
    let refusals = [
        (
            "remove",
            remove_plugin(&runtime, "demo", &PluginRemoveOptions::default())
                .expect_err("remove must not delete a tree this host did not install"),
        ),
        (
            "enable",
            enable_plugin(&runtime, "demo", &PluginEnableOptions::default())
                .map(|_| ())
                .expect_err("enable must not seed definitions out of that tree"),
        ),
        (
            "disable",
            disable_plugin(&runtime, "demo")
                .map(|_| ())
                .expect_err("disable must not select discovery links by that tree"),
        ),
    ];
    for (verb, error) in refusals {
        let message = error.to_string();
        assert!(
            matches!(error, OrbitError::PolicyDenied(_))
                && message.contains(&installed.install_path)
                && message.contains(&expected.display().to_string()),
            "{verb} must name the recorded and the expected path: {message}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&keep).expect("the sentinel file survives"),
        "operator data"
    );
    assert!(
        Path::new(&install_path).is_dir(),
        "a refusal touches neither tree"
    );

    // The refusal recommends the record-only removal, so that command has to
    // still have a row to clear: refusing must not discard the record first.
    assert_eq!(
        list_plugins(&runtime).expect("list").len(),
        1,
        "the refused row survives for the recovery the diagnostic names"
    );
    remove_plugin(
        &runtime,
        "demo",
        &PluginRemoveOptions {
            record_only: true,
            ..PluginRemoveOptions::default()
        },
    )
    .expect("record-only removal clears a row it cannot verify");
    assert!(list_plugins(&runtime).expect("list").is_empty());
    assert!(
        !crate::runtime::plugin::grants::plugin_grant_witness_path(&runtime.global_root(), "demo")
            .exists(),
        "the grant witness goes with the record"
    );
    assert_eq!(
        std::fs::read_to_string(&keep).expect("the sentinel file survives the recovery"),
        "operator data"
    );
    assert!(
        Path::new(&install_path).is_dir(),
        "record-only leaves every installed file where it is"
    );
}
