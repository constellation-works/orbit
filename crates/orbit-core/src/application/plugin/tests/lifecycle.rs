use std::path::Path;

use orbit_types::plugin::PluginStatus;

use super::super::{
    PluginAddOptions, PluginMigrateRequest, disable_plugin, install_plugin, list_plugins,
    migrate_plugin_sidecars, remove_plugin, sync_plugins, validate_plugin_dir,
};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::{PluginFixture, PluginSpecFixture};

#[test]
fn sync_installs_what_the_pin_file_names_and_reports_what_it_cannot() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: demo\n    source: {}\n    enabled: true\n  - name: absent\n    enabled: true\n",
        source.display()
    ));

    let planned = sync_plugins(&fixture.runtime, true).expect("dry run");
    assert_eq!(planned.len(), 2);
    assert!(
        planned[0].message.starts_with("would install"),
        "{planned:?}"
    );

    let outcomes = sync_plugins(&fixture.runtime, false).expect("sync");
    assert_eq!(outcomes[0].status, PluginStatus::Active);
    assert!(
        outcomes[0].message.contains("installed v1.0.0"),
        "{outcomes:?}"
    );
    assert_eq!(outcomes[1].status, PluginStatus::Missing);
    assert!(
        outcomes[1].message.contains("names no `source`"),
        "{outcomes:?}"
    );

    let runtime = fixture.reopen();
    runtime
        .show_tool("demo.hello")
        .expect("synced plugin registers its tool");
}

#[test]
fn disable_and_remove_take_the_plugin_off_the_surface() {
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

    remove_plugin(&runtime, "demo").expect("remove");
    assert!(list_plugins(&runtime).expect("list").is_empty());
    assert!(
        !Path::new(&install_path).exists(),
        "the install tree is gone"
    );
}

#[test]
fn validate_reports_an_unsatisfiable_requirement_as_a_warning() {
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

#[test]
fn migrate_writes_a_v2_manifest_from_v1_sidecars() {
    let fixture = PluginFixture::new();
    let plugin_dir = fixture.sources.join("legacy");
    std::fs::create_dir_all(&plugin_dir).expect("create legacy dir");
    std::fs::write(plugin_dir.join("legacy-tool"), "#!/bin/sh\n").expect("write executable");
    std::fs::write(
        plugin_dir.join("legacy-recommend.orbit-tool.yaml"),
        "schemaVersion: 1\nname: legacy.recommend\ndescription: Recommend things.\nparameters:\n- name: repository\n  description: Repo path.\n  param_type: string\n  required: true\n",
    )
    .expect("write sidecar");
    std::fs::write(
        plugin_dir.join("legacy-status.orbit-tool.yaml"),
        "schemaVersion: 1\nname: legacy.status\ndescription: Report status.\nparameters: []\n",
    )
    .expect("write sidecar");

    let out_dir = fixture.sources.join("legacy-v2");
    let (yaml, path) = migrate_plugin_sidecars(&PluginMigrateRequest {
        backend_command: plugin_dir
            .join("legacy-tool")
            .to_string_lossy()
            .into_owned(),
        sidecars: Vec::new(),
        version: "0.1.0".to_string(),
        namespace: None,
        out_dir: Some(out_dir.clone()),
    })
    .expect("migrate");
    assert!(yaml.contains("kind: Plugin"), "{yaml}");
    assert_eq!(path.as_deref(), Some(out_dir.join("plugin.yaml").as_path()));

    // The generated manifest is what `orbit plugin validate` accepts, and the
    // v1 tool names survive.
    std::fs::copy(plugin_dir.join("legacy-tool"), out_dir.join("legacy-tool"))
        .expect("copy backend");
    let report = validate_plugin_dir(&fixture.runtime, &out_dir, false).expect("validate migrated");
    assert_eq!(report.tools, ["legacy.recommend", "legacy.status"]);
}
