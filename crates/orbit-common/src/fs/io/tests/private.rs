use std::io;

use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn private_append_rejects_a_final_symlink() {
    let root = tempfile::tempdir().expect("root tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let outside_file = outside.path().join("outside.log");
    std::fs::write(&outside_file, b"unchanged").expect("outside file");
    let link = root.path().join("log");
    std::os::unix::fs::symlink(&outside_file, &link).expect("symlink");

    let error = crate::fs::io::append_private_file(&link)
        .expect_err("private append must reject a symlink target");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        std::fs::read(&outside_file).expect("outside file remains"),
        b"unchanged"
    );
}
#[cfg(unix)]
#[test]
fn private_open_repairs_a_permissive_existing_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("shared.lock");
    std::fs::write(&path, b"").expect("create lock file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("make the file permissive");

    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    let _file = crate::fs::io::open_private_file(&path, &mut options).expect("open private");

    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o600, "a permissive file is restricted to its owner");
}
