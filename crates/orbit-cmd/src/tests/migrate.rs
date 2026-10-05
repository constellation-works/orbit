//! Sibling tests for `command/migrate.rs` — the versioned `.orbit/` upgrade
//! surface and the workspace-open layout pre-flight [ORB-10012].

use std::fs;
use std::path::PathBuf;

use orbit_core::OrbitRuntime;

use crate::migrate_dry_run_at;

struct Roots {
    _temp: tempfile::TempDir,
    global_root: PathBuf,
    workspace_root: PathBuf,
}

fn temp_roots() -> Roots {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo").join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    Roots {
        _temp: temp,
        global_root,
        workspace_root,
    }
}

#[test]
fn workspace_layout_newer_than_binary_refuses_to_open() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &workspace_layout_newer_than_binary_refuses_to_open,
    )) {
        return;
    }

    let roots = temp_roots();
    let state_dir = roots.workspace_root.join("state");
    fs::create_dir_all(&state_dir).expect("mkdir state");
    fs::write(state_dir.join("layout.version"), "99\n").expect("write marker");

    let Err(error) = OrbitRuntime::from_roots(&roots.global_root, &roots.workspace_root) else {
        panic!("open must refuse a newer layout");
    };
    let message = error.to_string();
    assert!(message.contains("layout version 99"), "{message}");
    assert!(message.contains("upgrade orbit"), "{message}");

    // The dry-run surface still inspects it and flags the mismatch.
    let status =
        migrate_dry_run_at(&roots.global_root, &roots.workspace_root).expect("dry run status");
    assert_eq!(status.layout_version, 99);
    assert!(status.newer_than_binary());
    assert!(status.pending_layout.is_empty());
}
