use super::*;

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
