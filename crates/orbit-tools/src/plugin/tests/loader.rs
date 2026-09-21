use std::path::Path;

use orbit_types::plugin::MANIFEST_FILE_NAME;

use super::super::loader::load_plugin_dir;

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
