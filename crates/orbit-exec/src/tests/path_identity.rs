//! A granted path is resolved once, and the directory a rule binds to is the
//! one that resolution named — sibling layout under src/tests/.

use super::super::path_identity::create_write_root;

#[cfg(unix)]
#[test]
fn a_granted_write_root_standing_as_a_dangling_symlink_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = temp.path().join("base");
    std::fs::create_dir_all(&base).expect("base");
    let root = base.join("dangling");
    std::os::unix::fs::symlink(temp.path().join("nowhere"), &root).expect("symlink");

    let error = create_write_root(&root)
        .expect_err("a link standing where a directory is granted must not be followed")
        .to_string();
    assert!(error.contains("symbolic link"), "{error}");
}
