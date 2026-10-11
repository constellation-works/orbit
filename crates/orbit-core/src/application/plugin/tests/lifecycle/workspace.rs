//! Workspace plugin enablement persists only after contribution seeding works.

use orbit_common::OrbitError;
use orbit_config::ConfigRoots;

use super::super::super::{
    PluginAddOptions, PluginEnableOptions, disable_plugin_in_workspace, enable_plugin,
    enable_plugin_in_workspace, install_plugin,
};
use super::super::definition_fixture::DefinitionPlugin;
use super::super::fixture::PluginFixture;

#[test]
fn failed_workspace_enable_keeps_the_previous_disabled_toggle() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "failed_workspace_enable_keeps_the_previous_disabled_toggle",
    ) {
        return;
    }

    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("plugin source path"),
        &PluginAddOptions::default(),
    )
    .expect("install plugin");
    enable_plugin(&fixture.runtime, "graph", &PluginEnableOptions::default())
        .expect("enable plugin on host");
    disable_plugin_in_workspace(&fixture.runtime, "graph").expect("disable in workspace");

    let routines = fixture.workspace_root.join("routines");
    std::fs::remove_dir_all(&routines).expect("remove seeded routines directory");
    std::fs::write(&routines, "not a directory").expect("block routine seeding");

    let roots = ConfigRoots::new(&fixture.global_root, &fixture.workspace_root);
    assert_eq!(
        orbit_config::load_workspace_plugin_enablement(&roots)
            .expect("read workspace toggle")
            .get("graph"),
        Some(&false),
        "the test starts with the workspace plugin disabled"
    );
    let error = enable_plugin_in_workspace(&fixture.runtime, "graph", false)
        .expect_err("routine seeding must fail when its destination is a file");
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error}");
    assert_eq!(
        orbit_config::load_workspace_plugin_enablement(&roots)
            .expect("read workspace toggle after failed enable")
            .get("graph"),
        Some(&false),
        "a seed error must preserve the workspace's previous disabled toggle"
    );

    std::fs::remove_file(&routines).expect("remove routine seeding obstacle");

    let enabled = enable_plugin_in_workspace(&fixture.runtime, "graph", false)
        .expect("enable plugin in workspace");
    assert_eq!(enabled.summary.workspace_toggle, Some(true));
    assert_eq!(enabled.seeded.len(), 2, "routine and auto-task are seeded");
    assert_eq!(
        orbit_config::load_workspace_plugin_enablement(&ConfigRoots::new(
            &fixture.global_root,
            &fixture.workspace_root,
        ))
        .expect("read workspace toggle")
        .get("graph"),
        Some(&true)
    );
}
