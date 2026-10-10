use super::*;

// Deterministic interleaving admission: the CLI boundary cannot force an
// opener to queue while doctor owns the lock. Guards the orphaned-inode race.
#[cfg(unix)]
#[test]
fn stale_lock_cleanup_preserves_mutual_exclusion_for_queued_openers() {
    use std::os::unix::fs::MetadataExt;

    use crate::doctor::task::remove_stale_lock_file_after_acquire;

    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &stale_lock_cleanup_preserves_mutual_exclusion_for_queued_openers,
    )) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let lock_path = temp.path().join("state/layout.lock");
    write_holder_lock(&lock_path, reaped_child_pid(), "crashed layout upgrade");
    let original = fs::metadata(&lock_path).expect("original lock metadata");
    let mut queued = None;

    assert!(
        remove_stale_lock_file_after_acquire(&lock_path, || {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("open queued descriptor while cleanup holds the lock");
            assert_eq!(
                file.try_lock_exclusive()
                    .expect_err("queued opener must be refused while cleanup holds the lock")
                    .kind(),
                std::io::ErrorKind::WouldBlock
            );
            queued = Some(file);
        })
        .expect("clean stale holder")
    );

    let queued = queued.expect("queued descriptor");
    queued
        .try_lock_exclusive()
        .expect("queued descriptor acquires after cleanup releases");
    let fresh = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open lock after cleanup");
    assert_eq!(
        fresh
            .try_lock_exclusive()
            .expect_err("ORB-14069: a new opener must not acquire alongside the queued descriptor")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    let preserved = fresh.metadata().expect("preserved lock metadata");
    assert_eq!(
        (preserved.dev(), preserved.ino()),
        (original.dev(), original.ino())
    );
    assert_eq!(preserved.len(), 0, "cleanup clears only the holder record");
    assert!(orbit_store::read_lock_holder(&lock_path).is_none());
    drop(queued);
    fresh
        .try_lock_exclusive()
        .expect("new opener acquires after the queued descriptor releases");
}

#[cfg(unix)]
#[test]
fn retired_graph_cleanup_unlinks_boundaries_without_following_them() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &retired_graph_cleanup_unlinks_boundaries_without_following_them,
    )) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = split_root_runtime(&temp);
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("create outside");
    let outside_marker = outside.join("keep.db");
    fs::write(&outside_marker, b"keep").expect("write outside marker");
    let local_graph = runtime.local_root().join("graph");
    let shared_graph = runtime.shared_root().join("knowledge/graph");
    fs::create_dir_all(shared_graph.parent().expect("knowledge parent"))
        .expect("create knowledge parent");
    std::os::unix::fs::symlink(&outside, &local_graph).expect("link local graph");
    std::os::unix::fs::symlink(&outside, &shared_graph).expect("link shared graph");

    assert_eq!(
        runtime
            .remove_retired_graph_state()
            .expect("remove graph links"),
        2
    );
    assert!(
        outside_marker.exists(),
        "cleanup must not follow graph symlinks"
    );
    assert!(fs::symlink_metadata(local_graph).is_err());
    assert!(fs::symlink_metadata(shared_graph).is_err());
}
