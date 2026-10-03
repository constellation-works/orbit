//! `orbit plugin doctor` and `validate`.

use super::super::{PluginAddOptions, install_plugin, plugin_doctor, validate_plugin_dir};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::{PluginFixture, PluginSpecFixture};

#[test]
fn doctor_reports_seeded_definitions_older_than_the_installed_plugin() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "doctor_reports_seeded_definitions_older_than_the_installed_plugin",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install and seed");
    let routine = fixture.workspace_root.join("routines/graph-refresh.yaml");
    let raw = std::fs::read_to_string(&routine).expect("read seeded routine");
    std::fs::write(
        &routine,
        raw.replace("plugin:graph@1.0.0", "plugin:graph@0.9.0"),
    )
    .expect("make provenance stale");

    let findings = plugin_doctor(&fixture.reopen()).expect("doctor");
    assert!(
        findings.iter().any(|finding| {
            finding.plugin == "graph"
                && finding
                    .message
                    .contains("lag installed plugin 'graph' v1.0.0")
                && finding.message.contains("graph-refresh.yaml (v0.9.0)")
        }),
        "doctor must report stale workspace provenance: {findings:?}"
    );
}

#[test]
fn validate_reports_an_unsatisfiable_requirement_as_a_warning() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "validate_reports_an_unsatisfiable_requirement_as_a_warning",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let mut spec = PluginSpecFixture::new("future", "future");
    spec.requires_orbit = Some(">=99.0.0");
    let source = fixture.write_plugin(spec);

    let report = validate_plugin_dir(&fixture.runtime, &source, false).expect("validate");
    assert_eq!(report.name, "future");
    assert_eq!(report.tools, ["future.hello"]);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("requires orbit >=99.0.0")),
        "{report:?}"
    );
}

#[test]
fn validate_reports_the_namespaced_skill_discovery_id() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "validate_reports_the_namespaced_skill_discovery_id",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);

    let report = validate_plugin_dir(&fixture.runtime, &source, false).expect("validate");

    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("provider discovery as 'graph-graph'")),
        "validation must expose the skill id before install: {report:?}"
    );
}
