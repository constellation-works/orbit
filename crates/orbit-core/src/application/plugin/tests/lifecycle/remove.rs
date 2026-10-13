//! `orbit plugin remove --purge` stays inside the plugin's own state, and a
//! failed delete of the install tree leaves the row for a retry.

use orbit_common::OrbitError;

use crate::application::plugin::build::build_log_path;
use crate::runtime::plugin::build_witness::plugin_build_witness_path;
use crate::runtime::plugin::grants::plugin_grant_witness_path;
use crate::runtime::plugin::paths::plugin_namespace_dir;

use super::super::super::lifecycle::FAIL_TREE_DELETE;
use super::super::super::{
    PluginAddOptions, PluginEnableOptions, PluginRemoveOptions, enable_plugin, install_plugin,
    remove_plugin,
};
use super::super::build::{attach_build, build_record};
use super::super::fixture::{PluginFixture, PluginSpecFixture};

/// A tree delete that fails must leave the row, grant witness and build
/// witness for a retry, and the retry must finish the removal. Both trees the
/// removal deletes before dropping the row are covered: the namespace install
/// and the build log.
#[test]
fn a_failed_tree_delete_keeps_the_row_for_a_retry_that_finishes_it() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "a_failed_tree_delete_keeps_the_row_for_a_retry_that_finishes_it",
    ) {
        return;
    }

    for fail_build_log in [false, true] {
        let fixture = PluginFixture::new();
        let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
        install_plugin(
            &fixture.runtime,
            source.to_str().expect("utf8 path"),
            &PluginAddOptions::default(),
        )
        .expect("install");
        enable_plugin(&fixture.runtime, "demo", &PluginEnableOptions::default()).expect("enable");
        attach_build(&fixture, &build_record(Vec::new()));

        let namespace = plugin_namespace_dir(&fixture.global_root, "demo");
        let log_dir = build_log_path(&fixture.global_root, "demo")
            .parent()
            .expect("build log directory")
            .to_path_buf();
        std::fs::create_dir_all(&log_dir).expect("build log directory");
        std::fs::write(log_dir.join("build.log"), "built\n").expect("build log");
        let doomed = if fail_build_log {
            log_dir.clone()
        } else {
            namespace.clone()
        };
        FAIL_TREE_DELETE.with(|armed| *armed.borrow_mut() = Some(doomed));

        let error = remove_plugin(&fixture.runtime, "demo", &PluginRemoveOptions::default())
            .expect_err("the injected tree delete must fail the removal");
        assert!(matches!(error, OrbitError::Io(_)), "{error}");
        assert!(
            fixture
                .runtime
                .stores()
                .plugins()
                .get_plugin("demo")
                .expect("read the row")
                .is_some(),
            "a failed tree delete must leave the row for a retry"
        );
        assert!(
            plugin_grant_witness_path(&fixture.global_root, "demo").exists(),
            "the grant witness must outlive a failed tree delete"
        );
        assert!(
            plugin_build_witness_path(&fixture.global_root, "demo").exists(),
            "the build witness must outlive a failed tree delete"
        );

        remove_plugin(&fixture.runtime, "demo", &PluginRemoveOptions::default())
            .expect("a retry must finish the removal");
        assert!(
            !namespace.exists(),
            "the retry must delete the install tree"
        );
        assert!(!log_dir.exists(), "the retry must delete the build log");
        assert!(
            fixture
                .runtime
                .stores()
                .plugins()
                .get_plugin("demo")
                .expect("read the row")
                .is_none(),
            "the retry must delete the row last"
        );
        assert!(!plugin_grant_witness_path(&fixture.global_root, "demo").exists());
        assert!(!plugin_build_witness_path(&fixture.global_root, "demo").exists());
    }
}
