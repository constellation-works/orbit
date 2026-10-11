use std::io;

use tempfile::TempDir;

use crate::fs::io::remove_path_if_exists;

#[cfg(unix)]
#[test]
fn remove_path_if_exists_unlinks_a_directory_symlink_without_removing_its_target() {
    let temp = TempDir::new().expect("tempdir");
    let target = temp.path().join("target");
    std::fs::create_dir(&target).expect("target directory");
    let target_file = target.join("keep");
    std::fs::write(&target_file, b"preserved").expect("target file");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");

    remove_path_if_exists(&link).expect("remove directory symlink");

    let error = std::fs::symlink_metadata(&link).expect_err("link must be removed");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(
        std::fs::read(target_file).expect("target preserved"),
        b"preserved"
    );
}
