//! `orbit plugin disable` and `remove`.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::plugin::PluginStatus;

use super::super::super::{
    PluginAddOptions, PluginRemoveOptions, disable_plugin, install_plugin, list_plugins,
    remove_plugin,
};
use super::super::fixture::{PluginFixture, PluginSpecFixture};

#[test]
fn disable_and_remove_take_the_plugin_off_the_surface() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "disable_and_remove_take_the_plugin_off_the_surface",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");
    let install_path = summary.install_path.clone();

    let runtime = fixture.reopen();
    runtime
        .show_tool("demo.hello")
        .expect("registered while enabled");
    disable_plugin(&runtime, "demo").expect("disable");

    let runtime = fixture.reopen();
    assert!(
        runtime.show_tool("demo.hello").is_err(),
        "a disabled plugin registers nothing"
    );
    assert_eq!(
        list_plugins(&runtime).expect("list")[0].status,
        PluginStatus::Disabled
    );

    remove_plugin(&runtime, "demo", &PluginRemoveOptions::default()).expect("remove");
    assert!(list_plugins(&runtime).expect("list").is_empty());
    assert!(
        !Path::new(&install_path).exists(),
        "the install tree is gone"
    );
}

#[test]
fn remove_retains_state_by_default_and_purges_only_its_own_state_when_requested() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "remove_retains_state_by_default_and_purges_only_its_own_state_when_requested",
    ) {
        return;
    }
    for purge_state in [false, true] {
        let fixture = PluginFixture::new();
        let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
        install_plugin(
            &fixture.runtime,
            source.to_str().expect("utf8 path"),
            &PluginAddOptions::default(),
        )
        .expect("install");
        let own_state = fixture.global_root.join("state/plugins/demo");
        let other_state = fixture.global_root.join("state/plugins/other");
        let outside = fixture.repo_root.join("operator-data");
        for dir in [&own_state, &other_state, &outside] {
            std::fs::create_dir_all(dir).expect("create state or outside tree");
            std::fs::write(dir.join("keep.txt"), "keep").expect("write sentinel");
        }

        remove_plugin(
            &fixture.runtime,
            "demo",
            &PluginRemoveOptions {
                purge_state,
                ..PluginRemoveOptions::default()
            },
        )
        .expect("remove");

        assert_eq!(own_state.exists(), !purge_state);
        for dir in [&other_state, &outside] {
            assert_eq!(
                std::fs::read_to_string(dir.join("keep.txt")).expect("sentinel survives"),
                "keep"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn purge_refuses_a_symlinked_state_prefix_before_changing_the_install() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "purge_refuses_a_symlinked_state_prefix_before_changing_the_install",
    ) {
        return;
    }
    use std::os::unix::fs::symlink;

    for prefix in ["state/plugins", "state/plugins/demo"] {
        let fixture = PluginFixture::new();
        let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
        let summary = install_plugin(
            &fixture.runtime,
            source.to_str().expect("utf8 path"),
            &PluginAddOptions::default(),
        )
        .expect("install");
        let outside = fixture.repo_root.join("operator-data");
        std::fs::create_dir_all(&outside).expect("outside tree");
        let keep = outside.join("keep.txt");
        std::fs::write(&keep, "keep").expect("outside sentinel");
        let link = fixture.global_root.join(prefix);
        std::fs::create_dir_all(link.parent().expect("state prefix parent"))
            .expect("state prefix parent");
        symlink(&outside, &link).expect("link state prefix outside");

        let error = remove_plugin(
            &fixture.runtime,
            "demo",
            &PluginRemoveOptions {
                purge_state: true,
                ..PluginRemoveOptions::default()
            },
        )
        .expect_err("symlinked state prefix must refuse removal");
        assert!(
            matches!(error, OrbitError::PolicyDenied(_))
                && error.to_string().contains(&link.display().to_string()),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&keep).expect("sentinel"), "keep");
        assert!(Path::new(&summary.install_path).is_dir());
        assert_eq!(list_plugins(&fixture.runtime).expect("list").len(), 1);
    }
}
