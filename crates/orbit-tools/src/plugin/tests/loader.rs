use std::path::Path;

use orbit_types::plugin::MANIFEST_FILE_NAME;

use super::super::loader::{
    first_party_source, fs_write_root_covers, load_plugin_dir, refuse_covering_fs_write_roots,
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
    let error = refuse_covering_fs_write_roots(
        &plugin,
        &global_root,
        &global_root.join("state/plugins/demo"),
    )
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
        let error = refuse_covering_fs_write_roots(&plugin, &global_root, &plugin_state)
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
    refuse_covering_fs_write_roots(&plugin, &global_root, &plugin_state)
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
