//! `orbit plugin validate` semantics against the design's manifest example
//! and the documented rejections (docs/design/plugins/1_scope.md §2).
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use orbit_tools::plugin::{
    PluginLoadError, PluginValidationPolicy, load_plugin_dir, load_sidecar_manifest,
    migrate_sidecars, validate_loaded_plugin,
};
use orbit_types::plugin::{PluginBackendType, PluginMcpScope, SemverRange};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy the design example into a scratch dir so a test can edit it.
fn scratch_example() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = fixtures().join("plugins/graph-example");
    copy_tree(&source, temp.path());
    temp
}

fn copy_tree(source: &Path, target: &Path) {
    for entry in std::fs::read_dir(source).expect("read fixture dir") {
        let entry = entry.expect("entry");
        let destination = target.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            std::fs::create_dir_all(&destination).expect("mkdir");
            copy_tree(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), &destination).expect("copy");
        }
    }
}

fn manifest_field(error: PluginLoadError) -> String {
    match error {
        PluginLoadError::Manifest(error) => error.field,
        other => panic!("expected a manifest error, got: {other}"),
    }
}

fn rewrite_manifest(root: &Path, from: &str, to: &str) {
    let path = root.join("plugin.yaml");
    let raw = std::fs::read_to_string(&path).expect("read manifest");
    assert!(raw.contains(from), "fixture no longer contains {from:?}");
    std::fs::write(&path, raw.replace(from, to)).expect("write manifest");
}

#[test]
fn design_example_loads_and_validates_with_unused_sections_tolerated() {
    let plugin = load_plugin_dir(&fixtures().join("plugins/graph-example")).expect("loads");
    validate_loaded_plugin(&plugin, &PluginValidationPolicy::host_default()).expect("valid");

    assert_eq!(plugin.namespace(), "graph");
    assert!(plugin.manifest.spec.web.is_some(), "web: is parsed");
    assert!(
        plugin.manifest.spec.definitions.is_some(),
        "definitions: is parsed"
    );
    let verbs: Vec<&str> = plugin.tools.iter().map(|tool| tool.verb.as_str()).collect();
    assert_eq!(verbs, ["recommend", "status", "maintain"]);
    let recommend = &plugin.tools[0];
    assert_eq!(recommend.mcp_scope, PluginMcpScope::Workspace);
    assert!(
        recommend
            .parameters
            .iter()
            .any(|param| param.name == "repository" && param.required),
        "$ref input schema flattened into parameters: {:?}",
        recommend.parameters
    );
    assert!(recommend.output_schema.is_some());
    assert_eq!(plugin.manifest_digest.len(), 64);
    // Not first-party: no `origin`, so tools are `graph.<verb>`.
    assert_eq!(plugin.tool_name("recommend", false), "graph.recommend");
}

#[test]
fn design_example_supports_the_shipped_orbit_host() {
    let plugin = load_plugin_dir(&fixtures().join("plugins/graph-example")).expect("loads");
    let requirement = plugin
        .manifest
        .spec
        .requires
        .orbit
        .as_deref()
        .expect("design example declares its host requirement");
    let range = SemverRange::parse(requirement).expect("host requirement parses");
    let host_version = env!("CARGO_PKG_VERSION")
        .parse()
        .expect("crate version is semver");

    assert!(
        range.matches(&host_version),
        "design example must support this shipped Orbit host"
    );
}

#[test]
fn rejects_unknown_keys_naming_the_field() {
    let temp = scratch_example();
    rewrite_manifest(
        temp.path(),
        "network: none",
        "network: none\n    colour: blue",
    );
    let error = load_plugin_dir(temp.path()).unwrap_err();
    assert_eq!(manifest_field(error), "colour");
}

#[test]
fn rejects_a_ref_escaping_the_plugin_root() {
    let temp = scratch_example();
    let outside = temp.path().parent().expect("parent").join("outside.json");
    std::fs::write(&outside, "{\"type\":\"object\"}").expect("write outside schema");
    rewrite_manifest(
        temp.path(),
        "input_schema:  { $ref: schemas/recommend.request.json }",
        "input_schema:  { $ref: ../outside.json }",
    );
    let error = load_plugin_dir(temp.path()).unwrap_err();
    assert_eq!(
        manifest_field(error.clone()),
        "spec.tools[0].input_schema.$ref"
    );
    assert!(
        error.to_string().contains("escapes the plugin root"),
        "{error}"
    );
}

#[test]
fn rejects_first_party_namespace_without_a_verified_origin() {
    let temp = scratch_example();
    rewrite_manifest(
        temp.path(),
        "  publisher: constellation-works\n",
        "  publisher: constellation-works\n  origin: orbit\n",
    );
    let plugin = load_plugin_dir(temp.path()).expect("loads");
    let policy = PluginValidationPolicy::host_default();
    let error = validate_loaded_plugin(&plugin, &policy).unwrap_err();
    assert_eq!(error.field, "metadata.origin");
    assert!(error.message.contains("orbit.graph.*"), "{error}");

    validate_loaded_plugin(&plugin, &policy.with_first_party_verified(true))
        .expect("a verified first-party source may claim orbit.graph.*");
    assert_eq!(plugin.tool_name("recommend", true), "orbit.graph.recommend");
}

#[test]
fn rejects_namespace_collisions_with_builtin_tools_and_cli_commands() {
    let policy = PluginValidationPolicy::host_default();

    let temp = scratch_example();
    rewrite_manifest(temp.path(), "  name: graph ", "  name: proc  ");
    let plugin = load_plugin_dir(temp.path()).expect("loads");
    let error = validate_loaded_plugin(&plugin, &policy).unwrap_err();
    assert_eq!(error.field, "metadata.name");
    assert!(error.message.contains("proc.spawn"), "{error}");

    let temp = scratch_example();
    rewrite_manifest(temp.path(), "  name: graph ", "  name: task  ");
    let plugin = load_plugin_dir(temp.path()).expect("loads");
    let error = validate_loaded_plugin(&plugin, &policy).unwrap_err();
    assert_eq!(error.field, "metadata.name");
    assert!(error.message.contains("`orbit task` command"), "{error}");
}

#[test]
fn rejects_other_schema_versions_and_accepts_both_backend_types() {
    let temp = scratch_example();
    rewrite_manifest(temp.path(), "schemaVersion: 2", "schemaVersion: 1");
    assert_eq!(
        manifest_field(load_plugin_dir(temp.path()).unwrap_err()),
        "schemaVersion"
    );

    let temp = scratch_example();
    rewrite_manifest(temp.path(), "type: exec ", "type: mcp  ");
    let plugin = load_plugin_dir(temp.path()).expect("the mcp backend loads");
    assert_eq!(
        plugin.manifest.spec.backend.backend_type,
        PluginBackendType::Mcp
    );
}

#[test]
fn rejects_a_template_variable_outside_the_allowed_set() {
    let temp = scratch_example();
    rewrite_manifest(
        temp.path(),
        "write: [\"{{workspace}}/.orbit-graph\", \"{{plugin_state}}\"]",
        "write: [\"{{home}}/.orbit-graph\"]",
    );
    let error = load_plugin_dir(temp.path()).unwrap_err();
    assert_eq!(
        manifest_field(error.clone()),
        "spec.permissions.fs.write[0]"
    );
    assert!(error.to_string().contains("{{home}}"), "{error}");
}

#[test]
fn rejects_a_missing_backend_command() {
    let temp = scratch_example();
    std::fs::remove_file(temp.path().join("bin/orbit-graph")).expect("remove backend");
    let error = load_plugin_dir(temp.path()).unwrap_err();
    assert_eq!(manifest_field(error), "spec.backend.command");
}

#[test]
fn migrate_folds_the_orbit_graph_sidecars_into_one_v2_manifest() {
    let dir = fixtures().join("orbit-graph-sidecars");
    let mut sidecars = Vec::new();
    for name in [
        "orbit-graph-recommend.orbit-tool.yaml",
        "orbit-graph-status.orbit-tool.yaml",
        "orbit-graph-maintain.orbit-tool.yaml",
    ] {
        sidecars.push(load_sidecar_manifest(&dir.join(name)).expect("sidecar"));
    }
    let manifest = migrate_sidecars(&sidecars, "bin/orbit-graph", "0.4.1", None).expect("migrate");
    assert_eq!(manifest.metadata.name, "graph");
    assert_eq!(manifest.metadata.publisher, None);
    assert_eq!(manifest.metadata.origin, None);
    assert!(
        !manifest.claims_first_party_namespace(),
        "migration cannot carry a v1 name's unverified first-party claim into v2"
    );
    manifest.validate_structure().expect("structure");

    // Write it out and load it through the same path `orbit plugin validate` uses.
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("bin")).expect("bin");
    std::fs::write(temp.path().join("bin/orbit-graph"), "#!/bin/sh\n").expect("stub");
    std::fs::write(
        temp.path().join("plugin.yaml"),
        serde_yaml::to_string(&manifest).expect("serialize"),
    )
    .expect("write manifest");
    let loaded = load_plugin_dir(temp.path()).expect("migrated manifest loads");
    validate_loaded_plugin(&loaded, &PluginValidationPolicy::host_default())
        .expect("migrated manifest validates without first-party provenance");

    let names: Vec<String> = loaded
        .tools
        .iter()
        .map(|tool| loaded.tool_name(&tool.verb, false))
        .collect();
    assert_eq!(
        names,
        ["graph.recommend", "graph.status", "graph.maintain"],
        "the migrated local tree gets the unreserved graph.* namespace"
    );
    let recommend = &loaded.tools[0];
    let v1 = &sidecars[0].parameters;
    assert_eq!(recommend.parameters.len(), v1.len());
    for param in v1 {
        let migrated = recommend
            .parameters
            .iter()
            .find(|candidate| candidate.name == param.name)
            .unwrap_or_else(|| panic!("parameter {} survived migration", param.name));
        assert_eq!(migrated.required, param.required, "{}", param.name);
        assert_eq!(migrated.description, param.description, "{}", param.name);
    }
}
