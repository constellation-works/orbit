//! Which explicit roots are refused before generation pinning.

use crate::root_check::validate_explicit_root;

#[test]
fn a_file_or_a_path_below_a_file_is_not_a_usable_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("plain-file");
    std::fs::write(&file, "x").expect("write file");

    assert!(validate_explicit_root(&file, "--root").is_err());
    assert!(validate_explicit_root(&file.join("child"), "--root").is_err());
}

#[test]
fn existing_and_creatable_roots_are_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");

    assert!(validate_explicit_root(dir.path(), "--root").is_ok());
    assert!(validate_explicit_root(&dir.path().join("new").join("root"), "--root").is_ok());
}
