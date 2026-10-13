/// The permission change goes to the descriptor that was type-checked, so a
/// final component swapped for a symlink after the open cannot redirect it.
#[cfg(unix)]
#[test]
fn private_permission_hardening_follows_the_opened_file_across_a_swap() {
    use std::os::unix::fs::PermissionsExt;

    let mode = |path: &std::path::Path| {
        std::fs::symlink_metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let sidecar = dir.path().join("swapped.db-wal");
    let moved = dir.path().join("moved.db-wal");
    let target = dir.path().join("unrelated.txt");
    for file in [&sidecar, &target] {
        std::fs::write(file, b"bytes").expect("write fixture");
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644))
            .expect("make fixture permissive");
    }

    let opened = crate::fs::io::open_regular_file_no_follow(&sidecar).expect("open sidecar");
    std::fs::rename(&sidecar, &moved).expect("move the opened file away");
    std::os::unix::fs::symlink(&target, &sidecar).expect("swap in a symlink");
    crate::fs::io::set_private_file_permissions_for_open_file(&opened).expect("harden opened");

    assert_eq!(mode(&moved), 0o600, "the opened file is hardened");
    assert_eq!(mode(&target), 0o644, "the swapped-in target is untouched");

    let error = crate::fs::io::open_regular_file_no_follow(&sidecar)
        .expect_err("a no-follow open must refuse the swapped-in symlink");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
    assert_eq!(mode(&target), 0o644, "a refused open changes nothing");
}
