//! A workspace whose `.orbit/` layout is newer than this binary: the runtime
//! refuses to open it, while `orbit_cmd::migrate_dry_run_at` still inspects it.

use std::fs;

use orbit_core::OrbitRuntime;

#[test]
fn workspace_layout_newer_than_binary_refuses_to_open() {
    if crate::isolated_test::run_in_child(
        "layout_version::workspace_layout_newer_than_binary_refuses_to_open",
    ) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo").join(".orbit");
    let state_dir = workspace_root.join("state");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&state_dir).expect("mkdir state");
    fs::write(state_dir.join("layout.version"), "99\n").expect("write marker");

    let Err(error) = OrbitRuntime::from_roots(&global_root, &workspace_root) else {
        panic!("open must refuse a newer layout");
    };
    let message = error.to_string();
    assert!(message.contains("layout version 99"), "{message}");
    assert!(message.contains("upgrade orbit"), "{message}");

    // The dry-run surface still inspects it and flags the mismatch.
    let status =
        orbit_cmd::migrate_dry_run_at(&global_root, &workspace_root).expect("dry run status");
    assert_eq!(status.layout_version, 99);
    assert!(status.newer_than_binary());
    assert!(status.pending_layout.is_empty());
}
