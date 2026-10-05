//! `orbit plugin remove --purge` stays inside the plugin's own state.

use std::path::Path;

use orbit_common::OrbitError;

use super::super::super::{
    PluginAddOptions, PluginRemoveOptions, install_plugin, list_plugins, remove_plugin,
};
use super::super::fixture::{PluginFixture, PluginSpecFixture};

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
