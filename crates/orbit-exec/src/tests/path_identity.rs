//! A granted path is resolved once, and the directory a rule binds to is the
//! one that resolution named — sibling layout under src/tests/.

use super::super::path_identity::{
    create_write_root, lexical_normalize, physical_with_missing_tail,
};

#[test]
fn an_existing_path_resolves_to_its_canonical_self() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().join("tree/leaf");
    std::fs::create_dir_all(&dir).expect("create tree");

    assert_eq!(
        physical_with_missing_tail(&dir),
        dir.canonicalize().expect("canonicalize")
    );
}

#[cfg(unix)]
#[test]
fn a_missing_tail_is_read_under_the_physical_ancestor_not_the_spelled_one() {
    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    let state = temp.path().join("state");
    std::fs::create_dir_all(&real).expect("real");
    std::fs::create_dir_all(&state).expect("state");
    std::os::unix::fs::symlink(&real, state.join("alias")).expect("symlink");

    assert_eq!(
        physical_with_missing_tail(&state.join("alias/absent")),
        real.canonicalize().expect("canonicalize").join("absent"),
        "an existing symbolic link ancestor decides where an absent tail lives"
    );
}

#[test]
fn a_parent_dir_no_existing_ancestor_can_resolve_falls_back_to_the_lexical_reading() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("absent/deeper/../../sibling");

    assert_eq!(physical_with_missing_tail(&path), lexical_normalize(&path));
}

#[cfg(unix)]
#[test]
fn creating_a_granted_write_root_binds_the_resolved_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    let state = temp.path().join("state");
    std::fs::create_dir_all(&real).expect("real");
    std::fs::create_dir_all(&state).expect("state");
    std::os::unix::fs::symlink(&real, state.join("alias")).expect("symlink");

    let created = create_write_root(&state.join("alias/child")).expect("create");
    assert_eq!(
        created,
        real.canonicalize().expect("canonicalize").join("child"),
        "the compiled path is the one a containment check would have judged"
    );
    assert!(
        real.join("child").is_dir(),
        "the directory was created once"
    );
}

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

#[test]
fn an_existing_write_root_is_returned_without_being_recreated() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().join("existing");
    std::fs::create_dir_all(&dir).expect("existing");
    std::fs::write(dir.join("keep.txt"), "kept").expect("file");

    assert_eq!(
        create_write_root(&dir).expect("existing root"),
        dir.canonicalize().expect("canonicalize")
    );
    assert!(dir.join("keep.txt").exists(), "contents are untouched");
}
