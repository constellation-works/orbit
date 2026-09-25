//! Sibling tests for `paths.rs`: the pin file is read through a validated
//! root and a no-follow leaf.

use orbit_types::plugin::{PIN_FILE_NAME, PluginPinFile};

use super::super::paths::read_pin_file;

#[test]
fn read_pin_file_preserves_valid_fixed_leaf_reads() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        root.path().join(PIN_FILE_NAME),
        "schemaVersion: 1\nplugins: []\n",
    )
    .expect("write pin file");

    assert_eq!(
        read_pin_file(root.path()).expect("read pin file"),
        Some(PluginPinFile::default())
    );
}

#[cfg(unix)]
#[test]
fn read_pin_file_does_not_follow_a_symlinked_leaf() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("tempdir");
    let orbit_dir = root.path().join(".orbit");
    std::fs::create_dir(&orbit_dir).expect("create workspace state directory");
    let outside = root.path().join(PIN_FILE_NAME);
    std::fs::write(&outside, "schemaVersion: 1\nplugins: []\n").expect("write target pin file");
    symlink(&outside, orbit_dir.join(PIN_FILE_NAME)).expect("link pin file outside state root");

    assert_eq!(
        read_pin_file(&orbit_dir).expect("read pin file"),
        None,
        "the fixed pin filename must not redirect through a symlink"
    );
}
