//! `orbit plugin enable` and the contributions it applies.

use orbit_types::plugin::PluginStatus;

use super::super::super::{
    PluginAddOptions, PluginEnableOptions, PluginSeedAction, enable_plugin, install_plugin,
    show_plugin,
};
use super::super::definition_fixture::DefinitionPlugin;
use super::super::fixture::PluginFixture;

#[test]
fn failed_enabled_contributions_leave_the_installed_row_disabled() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "failed_enabled_contributions_leave_the_installed_row_disabled",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("unsafe-default")
        .with_enabled_routine()
        .write(&fixture);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install disabled plugin");

    let error = enable_plugin(
        &fixture.runtime,
        "unsafe-default",
        &PluginEnableOptions::default(),
    )
    .expect_err("enabled shipped schedules are refused")
    .to_string();
    assert!(error.contains("enabled: true"), "{error}");
    let installed = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("unsafe-default")
        .expect("read plugin row")
        .expect("plugin remains installed");
    assert!(
        !installed.enabled,
        "contribution failure must not commit the enable row"
    );
    assert_eq!(
        show_plugin(&fixture.reopen(), "unsafe-default")
            .expect("show")
            .status,
        PluginStatus::Disabled
    );
}

/// `orbit plugin add --enable` used to run the same enable as `orbit plugin
/// enable` and then discard everything it produced beyond the install
/// summary: seeded routines and auto-tasks, linked skills, and warnings
/// (including a grant the manifest did not request) were all invisible on
/// this path [ORB-12807].
#[test]
fn add_enable_carries_the_seeded_skills_and_warnings_report_out_of_install() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "add_enable_carries_the_seeded_skills_and_warnings_report_out_of_install",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);

    let result = fixture
        .runtime
        .add_plugin(
            source.to_str().expect("utf8 path"),
            &PluginAddOptions {
                enable: true,
                grants: vec!["fs".to_string()],
                ..PluginAddOptions::default()
            },
        )
        .expect("install and enable the fixture plugin");

    assert_eq!(result.summary.status, PluginStatus::Active, "{result:?}");

    let seeded_contains = |kind: &str, name: &str| {
        result
            .seeded
            .iter()
            .any(|outcome| outcome.kind == kind && outcome.name == name)
    };
    assert!(
        seeded_contains("routine", "graph-refresh"),
        "{:?}",
        result.seeded
    );
    assert!(
        seeded_contains("auto_task", "graph-reindex"),
        "{:?}",
        result.seeded
    );
    assert!(
        result
            .seeded
            .iter()
            .all(|outcome| outcome.action == PluginSeedAction::Created),
        "a fresh install must seed both definitions as created: {:?}",
        result.seeded
    );

    assert!(
        !result.skills.is_empty()
            && result
                .skills
                .iter()
                .all(|link| link.skill_id == "graph-graph"),
        "the shipped skill must be linked, not dropped, on the add --enable path: {:?}",
        result.skills
    );

    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("grant `fs`") && warning.contains("does not request")),
        "an unrequested grant must warn on add --enable the same way it does on enable: {:?}",
        result.warnings
    );
}
