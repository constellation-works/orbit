use std::path::Path;
use std::process::Command;

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
fn fs_write_root_covers_the_plugin_and_global_roots_and_not_their_children() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_root = temp.path().join("plugins/demo/1.0.0");
    let global_root = temp.path().join("global");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    std::fs::create_dir_all(&global_root).expect("global root");

    assert_eq!(
        fs_write_root_covers(&plugin_root, &plugin_root, &global_root),
        Some("plugin install root")
    );
    assert_eq!(
        fs_write_root_covers(Path::new("/"), &plugin_root, &global_root),
        Some("plugin install root")
    );
    assert_eq!(
        fs_write_root_covers(&global_root, &plugin_root, &global_root),
        Some("Orbit global root")
    );
    assert_eq!(
        fs_write_root_covers(&plugin_root.join("cache"), &plugin_root, &global_root),
        None
    );
    assert_eq!(
        fs_write_root_covers(
            &global_root.join("state/plugins/demo"),
            &plugin_root,
            &global_root
        ),
        None
    );
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

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout)
        .expect("git output")
        .trim()
        .into()
}

/// A marker planted in the *path* of an unrelated host must not verify: only
/// a parsed host and organisation should, not a substring anywhere in the URL.
#[test]
fn first_party_source_parses_host_and_org_rather_than_matching_a_substring() {
    let elsewhere = Path::new("/nonexistent");

    assert!(
        !first_party_source(
            "git+https://evil.test/github.com/constellation-works/x.git",
            elsewhere,
        ),
        "the marker sits in an unrelated host's path, not its authority, and must not verify"
    );
    assert!(
        first_party_source("git+git@github.com:constellation-works/x.git", elsewhere),
        "the SCP-like git@host:org/repo form must still verify"
    );
    assert!(
        first_party_source(
            "git+https://github.com/constellation-works/x.git",
            elsewhere,
        ),
        "a genuine constellation-works GitHub URL must still verify"
    );
}

/// The `git+<url>` branch and the local-checkout `git remote get-url origin`
/// branch both resolve through [`first_party_source`] into the same parser,
/// so they cannot drift: exercise the same three shapes against a real
/// `origin` remote.
#[test]
fn first_party_source_checks_the_origin_remote_through_the_same_parser() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_root = temp.path();
    git(repo_root, &["init", "--quiet"]);
    git(
        repo_root,
        &[
            "remote",
            "add",
            "origin",
            "https://evil.test/github.com/constellation-works/x.git",
        ],
    );
    assert!(
        !first_party_source("local", repo_root),
        "an origin remote whose path merely contains the marker must not verify"
    );

    git(
        repo_root,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:constellation-works/x.git",
        ],
    );
    assert!(
        first_party_source("local", repo_root),
        "an origin remote in the SCP-like form must verify"
    );

    git(
        repo_root,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/constellation-works/x.git",
        ],
    );
    assert!(
        first_party_source("local", repo_root),
        "a genuine constellation-works origin remote must verify"
    );
}
