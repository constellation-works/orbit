//! Sibling tests for `backend.rs`: every phase that builds a plugin backend
//! derives the same effective config and filesystem roots.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_tools::plugin::{load_plugin_dir, refuse_covering_fs_write_roots, render_fs_roots};
use orbit_types::plugin::{
    InstalledPlugin, PluginGrant, PluginGrantSet, PluginProvenance, PluginTemplateVars,
};

use super::super::backend::{build_plugin_backend, plugin_backend};
use super::super::paths::plugin_state_dir;

fn write_plugin_with_typed_config_roots(root: &Path, name: &str) {
    std::fs::create_dir_all(root.join("bin")).expect("create bin dir");
    std::fs::create_dir_all(root.join("schemas")).expect("create schema dir");
    std::fs::write(root.join("bin/backend.sh"), "#!/bin/sh\n").expect("write backend");
    std::fs::write(
        root.join("schemas/config.json"),
        "{\n  \"type\": \"object\",\n  \"properties\": {\n    \
         \"directory\": { \"type\": \"string\" },\n    \
         \"port\": { \"type\": \"integer\" },\n    \
         \"enabled\": { \"type\": \"boolean\" }\n  }\n}\n",
    )
    .expect("write config schema");
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\nspec:\n  \
             backend:\n    type: exec\n    command: bin/backend.sh\n  config:\n    schema: \
             schemas/config.json\n    defaults: {{ directory: default, port: 7, enabled: true }}\n  \
             permissions:\n    fs:\n      read:\n        - \"{{{{config.directory}}}}/read\"\n        \
             - \"{{{{config.port}}}}/read\"\n        - \"{{{{config.enabled}}}}/read\"\n        - \
             relative/read\n      write:\n        - \"{{{{config.directory}}}}/write\"\n        - \
             \"{{{{config.port}}}}/write\"\n        - \"{{{{config.enabled}}}}/write\"\n        - \
             relative/write\n  tools:\n    - name: hello\n      execution_kind: read_only\n      \
             mcp_scope: workspace\n"
        ),
    )
    .expect("write manifest");
}

#[test]
fn typed_config_and_relative_fs_roots_match_validate_registration_call_and_conformance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_root = temp.path().join("plugin");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("workspace");
    let state_dir = plugin_state_dir(&global_root, "rooted");
    for dir in [&global_root, &workspace_root, &state_dir] {
        std::fs::create_dir_all(dir).expect("create phase root");
    }
    write_plugin_with_typed_config_roots(&plugin_root, "rooted");
    let plugin = load_plugin_dir(&plugin_root).expect("load manifest");

    // The string and boolean are effective operator overrides; the integer
    // stays a typed manifest default. Every phase must stringify all three.
    let config = BTreeMap::from([(
        "rooted".to_string(),
        serde_json::json!({"directory": "../configured", "enabled": false}),
    )]);
    let section = super::super::config::plugin_config_section(&plugin, &config);
    assert_eq!(
        section.as_value(),
        &serde_json::json!({"directory": "../configured", "enabled": false, "port": 7}),
        "the backend's own view of the section keeps every JSON type"
    );
    let config_values = section.rendered_values();
    assert_eq!(
        config_values,
        BTreeMap::from([
            ("directory".to_string(), "../configured".to_string()),
            ("enabled".to_string(), "false".to_string()),
            ("port".to_string(), "7".to_string()),
        ])
    );

    let grants = PluginGrantSet::from_grants([PluginGrant::Fs]);
    let provenance = || PluginProvenance {
        name: "rooted".to_string(),
        version: plugin.manifest.metadata.version.clone(),
        manifest_digest: plugin.manifest_digest.clone(),
        grants: vec!["fs".to_string()],
    };

    // `validate_plugin_dir` and conformance both construct their backends
    // through this builder. Registration additionally derives the same
    // effective values from the installed row and resolved config.
    let validate = build_plugin_backend(
        &plugin,
        provenance(),
        &state_dir,
        &global_root,
        grants.clone(),
        section.clone(),
        BTreeMap::new(),
    );
    refuse_covering_fs_write_roots(validate.spec(), None).expect("validate roots");

    let installed = InstalledPlugin {
        name: "rooted".to_string(),
        version: plugin.manifest.metadata.version.clone(),
        source: "fixture".to_string(),
        install_path: plugin_root.to_string_lossy().into_owned(),
        archive_digest: None,
        manifest_digest: plugin.manifest_digest.clone(),
        enabled: true,
        grants: vec!["fs".to_string()],
        first_party: false,
        certified_orbit_version: None,
        installed_at: String::new(),
        updated_at: String::new(),
    };
    let registration = plugin_backend(&global_root, &installed, &plugin, &config);
    refuse_covering_fs_write_roots(registration.spec(), None).expect("registration roots");

    let conformance = build_plugin_backend(
        &plugin,
        provenance(),
        &state_dir,
        &global_root,
        grants,
        section,
        BTreeMap::new(),
    );
    refuse_covering_fs_write_roots(conformance.spec(), None).expect("conformance roots");

    let render = |backend: &orbit_tools::plugin::PluginBackend| {
        let spec = backend.spec();
        let vars = PluginTemplateVars {
            workspace: Some(workspace_root.to_string_lossy().into_owned()),
            plugin_root: spec.plugin_root.to_string_lossy().into_owned(),
            plugin_state: spec.state_dir.to_string_lossy().into_owned(),
            config: spec.config_values(),
        };
        render_fs_roots(spec, &vars).expect("render roots")
    };
    let validate_roots = render(&validate);
    let registration_roots = render(&registration);
    let conformance_roots = render(&conformance);

    let call_profile = registration
        .spec()
        .sandbox_profile(Some(&workspace_root))
        .expect("call-time profile");
    let validate_profile = validate
        .spec()
        .sandbox_profile(Some(&workspace_root))
        .expect("validate call-time profile");
    let conformance_profile = conformance
        .spec()
        .sandbox_profile(Some(&workspace_root))
        .expect("conformance call-time profile");
    let expected_call_reads = std::iter::once(plugin_root.clone())
        .chain(registration_roots.read.iter().cloned())
        .chain(std::iter::once(state_dir.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        call_profile.read, expected_call_reads,
        "call time includes the plugin root, declared roots, then its own state"
    );
    assert_eq!(
        call_profile.read.last(),
        Some(&state_dir),
        "the plugin's exact own-state root is last at call time"
    );
    assert_eq!(
        call_profile
            .read
            .iter()
            .filter(|root| *root == &state_dir)
            .count(),
        1,
        "the plugin's own-state root appears only once at call time"
    );
    assert_eq!(call_profile.write, registration_roots.write);
    assert_eq!(validate_profile.read, call_profile.read);
    assert_eq!(conformance_profile.read, call_profile.read);
    assert_eq!(validate_roots, registration_roots);
    assert_eq!(registration_roots, conformance_roots);

    assert_eq!(
        registration_roots.read,
        vec![
            plugin_root.join("../configured/read"),
            plugin_root.join("7/read"),
            plugin_root.join("false/read"),
            plugin_root.join("relative/read"),
        ]
    );
    assert_eq!(
        registration_roots.write,
        vec![
            plugin_root.join("../configured/write"),
            plugin_root.join("7/write"),
            plugin_root.join("false/write"),
            plugin_root.join("relative/write"),
        ]
    );
}
