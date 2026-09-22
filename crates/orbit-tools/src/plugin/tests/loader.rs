use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_types::plugin::{MANIFEST_FILE_NAME, PluginProvenance};

use super::super::backend::PluginBackendSpec;
use super::super::loader::{
    LoadedPlugin, first_party_source, fs_write_root_covers, load_plugin_dir,
    refuse_covering_fs_write_roots,
};

const MANIFEST: &str = "\
schemaVersion: 2
kind: Plugin
metadata:
  name: demo
  version: 1.0.0
  description: Fixture.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: hello
      description: Hello.
      execution_kind: read_only
";

fn write_plugin(root: &Path) {
    std::fs::create_dir_all(root.join("bin")).expect("mkdir");
    std::fs::write(root.join(MANIFEST_FILE_NAME), MANIFEST).expect("manifest");
    std::fs::write(root.join("bin/backend.sh"), "#!/bin/sh\n").expect("backend");
}

fn backend_spec(
    plugin: &LoadedPlugin,
    global_root: &Path,
    state_dir: PathBuf,
) -> PluginBackendSpec {
    let grants = plugin.manifest.required_grants();
    PluginBackendSpec {
        provenance: PluginProvenance {
            name: plugin.namespace().to_string(),
            version: plugin.manifest.metadata.version.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            grants: grants
                .iter()
                .map(|grant| grant.as_str().to_string())
                .collect(),
        },
        plugin_root: plugin.root.clone(),
        state_dir,
        global_root: global_root.to_path_buf(),
        command: plugin.backend_command.clone(),
        args: plugin.manifest.spec.backend.args.clone(),
        timeout_ms: plugin.manifest.spec.backend.timeout_ms,
        sandbox: plugin.manifest.spec.backend.sandbox,
        permissions: plugin.manifest.spec.permissions.clone(),
        programs: plugin.manifest.spec.requires.programs.clone(),
        config_values: BTreeMap::new(),
        grants,
    }
}

#[test]
fn load_plugin_dir_accepts_a_tree_with_no_symbolic_links() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let plugin = load_plugin_dir(temp.path()).expect("load");
    assert_eq!(plugin.namespace(), "demo");
}

#[test]
fn load_refuses_resolved_schema_properties_with_colliding_cli_flags() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let manifest = temp.path().join(MANIFEST_FILE_NAME);
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "      execution_kind: read_only\n",
            "      execution_kind: read_only\n      input_schema:\n        type: object\n        properties:\n          task_id: { type: string }\n          taskId: { type: string }\n",
        ),
    )
    .expect("write manifest");

    let error = load_plugin_dir(temp.path())
        .expect_err("colliding flags refuse load")
        .to_string();
    assert!(
        error.contains("task_id") && error.contains("taskId"),
        "the load diagnostic names both colliding properties: {error}"
    );
}

/// Write `MANIFEST` with `body` spliced into its single tool.
fn write_plugin_with_tool_keys(root: &Path, keys: &str) {
    write_plugin(root);
    let manifest = root.join(MANIFEST_FILE_NAME);
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "      execution_kind: read_only\n",
            &format!("      execution_kind: read_only\n{keys}"),
        ),
    )
    .expect("write manifest");
}

/// A `$ref` schema is only properties once it is read from the plugin root,
/// so the CLI checks run there too: otherwise the adapter silently drops both
/// colliding flags and the tool loses them with no diagnostic anywhere.
#[test]
fn a_ref_schema_is_held_to_the_cli_flag_and_positional_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin_with_tool_keys(
        temp.path(),
        "      input_schema: { $ref: schemas/hello.json }\n",
    );
    std::fs::create_dir_all(temp.path().join("schemas")).expect("mkdir");
    let schema = temp.path().join("schemas/hello.json");
    std::fs::write(
        &schema,
        r#"{"type":"object","properties":{"task_id":{"type":"string"},"taskId":{"type":"string"}}}"#,
    )
    .expect("schema");

    let error = load_plugin_dir(temp.path())
        .expect_err("colliding flags behind a $ref refuse load")
        .to_string();
    assert!(
        error.contains("task_id") && error.contains("taskId"),
        "the load diagnostic names both colliding properties: {error}"
    );

    std::fs::write(
        &schema,
        r#"{"type":"object","properties":{"query":{"type":"string"}}}"#,
    )
    .expect("schema");
    load_plugin_dir(temp.path()).expect("a $ref schema with distinct flags loads");

    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin_with_tool_keys(
        temp.path(),
        "      input_schema: { $ref: schemas/hello.json }\n      cli: { positional: [subject] }\n",
    );
    std::fs::create_dir_all(temp.path().join("schemas")).expect("mkdir");
    std::fs::write(
        temp.path().join("schemas/hello.json"),
        r#"{"type":"object","properties":{"query":{"type":"string"}}}"#,
    )
    .expect("schema");
    let error = load_plugin_dir(temp.path())
        .expect_err("a positional outside the resolved schema refuses load")
        .to_string();
    assert!(
        error.contains("hello") && error.contains("subject"),
        "the refusal names the tool and the positional: {error}"
    );
}

/// §4.9: an unresolvable `$ref` refuses the plugin at load. A nested one is
/// found by compiling the schema, which is also where an invalid keyword is
/// found — neither may be left to fail every call instead.
#[test]
fn a_schema_that_cannot_compile_refuses_the_plugin_at_load() {
    for (keys, needle) in [
        (
            "      input_schema:\n        type: object\n        properties:\n          query: { $ref: '#/definitions/missing' }\n",
            "input_schema",
        ),
        (
            "      output_schema:\n        type: object\n        properties:\n          count: { type: whatever }\n",
            "output_schema",
        ),
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        write_plugin_with_tool_keys(temp.path(), keys);
        let error = load_plugin_dir(temp.path())
            .expect_err("a schema that does not compile refuses load")
            .to_string();
        assert!(
            error.contains(needle) && error.contains("hello"),
            "the refusal names the field and the tool: {error}"
        );
    }

    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin_with_tool_keys(
        temp.path(),
        "      input_schema:\n        type: object\n        properties:\n          query: { $ref: '#/definitions/term' }\n        definitions:\n          term: { type: string }\n      output_schema:\n        type: object\n        properties:\n          count: { type: integer }\n",
    );
    let plugin = load_plugin_dir(temp.path()).expect("resolvable schemas load");
    assert!(plugin.tools[0].output_schema.is_some());
}

#[test]
fn a_nested_unresolvable_ref_in_an_output_schema_refuses_load() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin_with_tool_keys(
        temp.path(),
        "      output_schema:\n        type: object\n        properties:\n          result:\n            type: object\n            properties:\n              value: { $ref: '#/$defs/missing' }\n",
    );

    let error = load_plugin_dir(temp.path())
        .expect_err("a nested unresolved output ref must refuse load")
        .to_string();
    assert!(
        error.contains("output_schema") && error.contains("hello"),
        "the refusal names the schema field and tool: {error}"
    );
}

#[test]
fn a_conformance_case_for_an_undeclared_tool_refuses_load() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let manifest = temp.path().join(MANIFEST_FILE_NAME);
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace("  tools:\n", "  tests: [tests/*.yaml]\n  tools:\n"),
    )
    .expect("declare conformance tests");
    std::fs::create_dir_all(temp.path().join("tests")).expect("create tests directory");
    std::fs::write(
        temp.path().join("tests/undeclared.yaml"),
        "schemaVersion: 1\nkind: PluginTest\ntests:\n  - name: typo\n    tool: missing\n    expect:\n      output: {}\n",
    )
    .expect("write conformance case");

    let error = load_plugin_dir(temp.path())
        .expect_err("an undeclared conformance tool must refuse the plugin directory")
        .to_string();
    assert!(
        error.contains("missing") && error.contains("does not declare"),
        "the refusal names the undeclared tool: {error}"
    );
}

#[cfg(unix)]
#[test]
fn load_plugin_dir_refuses_an_installed_tree_that_contains_a_symlink() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let secret = temp.path().parent().expect("parent").join("secret");
    std::fs::write(&secret, "SECRET-CONTENT-OUTSIDE-PLUGIN-TREE").expect("secret");
    std::os::unix::fs::symlink(&secret, temp.path().join("env")).expect("symlink");

    let error = load_plugin_dir(temp.path())
        .expect_err("a planted symlink must refuse load")
        .to_string();
    assert!(
        error.contains("env"),
        "refusal must name the offending entry: {error}"
    );
    assert!(
        error.contains("symbolic link"),
        "refusal must say why: {error}"
    );
}

#[test]
fn fs_write_root_refuses_protected_global_paths_but_allows_plugin_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let plugin_root = global_root.join("plugins/demo/1.0.0");
    let plugin_state = global_root.join("state/plugins/demo");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    std::fs::create_dir_all(&plugin_state).expect("plugin state");

    assert_eq!(
        fs_write_root_covers(
            &plugin_root,
            &plugin_root,
            &global_root,
            &plugin_state,
            None,
        ),
        Some("plugin install root")
    );
    assert_eq!(
        fs_write_root_covers(
            Path::new("/"),
            &plugin_root,
            &global_root,
            &plugin_state,
            None,
        ),
        Some("plugin install root")
    );
    assert_eq!(
        fs_write_root_covers(
            &global_root,
            &plugin_root,
            &global_root,
            &plugin_state,
            None,
        ),
        Some("plugin install root")
    );
    for protected in [
        global_root.join("bin"),
        global_root.join("plugins/.grants"),
        global_root.join("plugins/other/1.0.0"),
        plugin_state.join("../../../plugins/.grants"),
        plugin_root.join("cache"),
    ] {
        assert_eq!(
            fs_write_root_covers(&protected, &plugin_root, &global_root, &plugin_state, None,),
            Some("protected path beneath Orbit global root"),
            "{} must stay read-only",
            protected.display()
        );
    }
    for allowed in [&plugin_state, &plugin_state.join("cache")] {
        assert_eq!(
            fs_write_root_covers(allowed, &plugin_root, &global_root, &plugin_state, None,),
            None,
            "{} is inside this plugin's writable state tree",
            allowed.display()
        );
    }
}

/// A backend with a writable `{{plugin_state}}` can plant a symbolic link in
/// its own state tree, so the guard cannot read a declared root by name: a
/// root whose tail does not exist yet is judged where its existing ancestors
/// physically live. Otherwise `state/plugins/demo/alias/9.0.0` reads as
/// plugin state while it materialises a new version tree inside the
/// plugin's protected install namespace [ORB-12799].
#[cfg(unix)]
#[test]
fn fs_write_root_refuses_an_absent_tail_below_a_symlink_into_the_global_root() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let install_root = global_root.join("plugins/demo");
    let plugin_root = install_root.join("1.0.0");
    let plugin_state = global_root.join("state/plugins/demo");
    for directory in [
        &plugin_root,
        &plugin_state,
        &global_root.join("bin"),
        &global_root.join("plugins/.grants"),
        &global_root.join("plugins/other/1.0.0"),
    ] {
        std::fs::create_dir_all(directory).expect("fixture directory");
    }

    for (alias, target) in [
        ("install", install_root.clone()),
        ("bin", global_root.join("bin")),
        ("grants", global_root.join("plugins/.grants")),
        ("other", global_root.join("plugins/other")),
    ] {
        let link = plugin_state.join(alias);
        symlink(&target, &link).expect("state alias");
        let absent = link.join("9.0.0");
        assert_eq!(
            fs_write_root_covers(&absent, &plugin_root, &global_root, &plugin_state, None),
            Some("protected path beneath Orbit global root"),
            "{} reaches {} through an alias in writable plugin state",
            absent.display(),
            target.display()
        );
        assert!(
            !absent.exists(),
            "the guard decides a path without creating it"
        );
    }

    // The alias to this plugin's own install root is the only one that could
    // be mistaken for its own tree; it is refused as the install namespace it
    // physically is, not as plugin state.
    assert!(!install_root.join("9.0.0").exists());

    for allowed in [
        plugin_state.join("cache"),
        plugin_state.join("cache/deeper/still-absent"),
    ] {
        assert_eq!(
            fs_write_root_covers(&allowed, &plugin_root, &global_root, &plugin_state, None),
            None,
            "{} is an absent directory inside the real plugin state tree",
            allowed.display()
        );
    }
}

#[test]
fn load_refuses_a_manifest_whose_fs_write_covers_the_plugin_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let manifest = temp.path().join(MANIFEST_FILE_NAME);
    let body = std::fs::read_to_string(&manifest).expect("read");
    std::fs::write(
        &manifest,
        body.replace(
            "  backend:\n",
            "  permissions:\n    fs:\n      write: [\"{{plugin_root}}\"]\n  backend:\n",
        ),
    )
    .expect("covering write");
    let plugin = load_plugin_dir(temp.path()).expect("load");
    let global_root = temp.path().join("global");
    let spec = backend_spec(
        &plugin,
        &global_root,
        global_root.join("state/plugins/demo"),
    );
    let error = refuse_covering_fs_write_roots(&spec, None)
        .expect_err("plugin-root write is refused")
        .to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]") && error.contains("plugin install root"),
        "{error}"
    );
}

#[test]
fn validate_and_registration_refuse_workspace_metadata_but_allow_a_sibling() {
    let global = tempfile::tempdir().expect("global tempdir");
    let global_root = global.path().join("global");
    let plugin_state = global_root.join("state/plugins/demo");

    for declared in [
        "{{workspace}}",
        "{{workspace}}/.orbit/routines",
        "{{workspace}}/.git/hooks",
    ] {
        let temp = tempfile::tempdir().expect("plugin tempdir");
        write_plugin(temp.path());
        let manifest = temp.path().join(MANIFEST_FILE_NAME);
        let body = std::fs::read_to_string(&manifest).expect("read manifest");
        std::fs::write(
            &manifest,
            body.replace(
                "  backend:\n",
                &format!("  permissions:\n    fs:\n      write: [\"{declared}\"]\n  backend:\n"),
            ),
        )
        .expect("write permission");
        let plugin = load_plugin_dir(temp.path()).expect("load");
        let spec = backend_spec(&plugin, &global_root, plugin_state.clone());
        let error = refuse_covering_fs_write_roots(&spec, None)
            .expect_err("workspace metadata write must be refused before call time")
            .to_string();
        assert!(
            error.contains("spec.permissions.fs.write[0]")
                && error.contains(".orbit")
                && error.contains(".git"),
            "{declared}: {error}"
        );
    }

    let allowed = tempfile::tempdir().expect("allowed plugin tempdir");
    write_plugin(allowed.path());
    let manifest = allowed.path().join(MANIFEST_FILE_NAME);
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "  backend:\n",
            "  permissions:\n    fs:\n      write: [\"{{workspace}}/.orbit-graph\"]\n  backend:\n",
        ),
    )
    .expect("write permission");
    let plugin = load_plugin_dir(allowed.path()).expect("load");
    let spec = backend_spec(&plugin, &global_root, plugin_state);
    refuse_covering_fs_write_roots(&spec, None)
        .expect("a similarly named workspace directory is not Orbit metadata");
}

/// A marker planted in the *path* of an unrelated host must not verify: only
/// a parsed host and organisation should, not a substring anywhere in the URL.
#[test]
fn first_party_source_parses_host_and_org_rather_than_matching_a_substring() {
    assert!(
        !first_party_source("git+https://evil.test/github.com/constellation-works/x.git"),
        "the marker sits in an unrelated host's path, not its authority, and must not verify"
    );
    assert!(
        first_party_source("git+git@github.com:constellation-works/x.git"),
        "the SCP-like git@host:org/repo form must still verify"
    );
    assert!(
        first_party_source("git+https://github.com/constellation-works/x.git"),
        "a genuine constellation-works GitHub URL must still verify"
    );
}

/// Directory sources cannot establish first-party provenance because their
/// Git metadata is controlled by the directory author.
#[test]
fn first_party_source_refuses_directory_sources() {
    let temp = tempfile::tempdir().expect("tempdir");
    assert!(
        !first_party_source(temp.path().to_str().expect("utf8 directory source")),
        "a directory source is never first-party"
    );
}
