//! `orbit plugin migrate`.

use super::super::super::{PluginMigrateRequest, migrate_plugin_sidecars, validate_plugin_dir};
use super::super::fixture::PluginFixture;

#[test]
fn migrate_writes_a_v2_manifest_from_v1_sidecars() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "migrate_writes_a_v2_manifest_from_v1_sidecars",
    ) {
        return;
    }
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
    let plugin_root = out_dir.join(orbit_types::plugin::PLUGIN_DIR_NAME);
    assert_eq!(
        path.as_deref(),
        Some(plugin_root.join("plugin.yaml").as_path())
    );
    assert!(
        !yaml.contains("null") && !yaml.contains("[]"),
        "migration must omit optional and empty fields: {yaml}"
    );
    assert!(yaml.contains("command: bin/legacy-tool"), "{yaml}");
    assert!(
        !yaml.contains("publisher:") && !yaml.contains("origin:"),
        "migration must not claim first-party provenance: {yaml}"
    );

    // The generated manifest is what `orbit plugin validate` accepts, and the
    // v1 tool names survive. Migration places the backend in the plugin root
    // even though its source and sidecars were elsewhere.
    assert!(plugin_root.join("bin/legacy-tool").is_file());
    let report = validate_plugin_dir(&fixture.runtime, &out_dir, false).expect("validate migrated");
    assert_eq!(report.tools, ["legacy.recommend", "legacy.status"]);
}
