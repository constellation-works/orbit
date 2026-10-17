use super::*;
#[cfg(unix)]
use orbit_common::fs::io::create_dir_symlink;
use orbit_store::workflow::layout::upgrade_workspace_layout;
use std::fs;

#[cfg(unix)]
#[test]
fn legacy_cleanup_does_not_follow_a_symlinked_tasks_parent() {
    if !support::isolated("layout::legacy_cleanup_does_not_follow_a_symlinked_tasks_parent") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().expect("outside tempdir");
    let target = outside.path().join("ORB-00001");
    fs::create_dir(&target).expect("create outside entry");
    create_dir_symlink(outside.path(), &temp.path().join("tasks"))
        .expect("create symlinked parent");

    fs::create_dir_all(temp.path().join("state")).unwrap();
    fs::write(temp.path().join("state/layout.version"), "2\n").unwrap();
    upgrade_workspace_layout(temp.path()).expect("cleanup symlinked parent");

    assert!(temp.path().join("tasks").is_symlink());
    assert!(target.is_dir());
}
