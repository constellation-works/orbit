//! Sibling tests for `sandbox_mask.rs`: the host prepares the trees and the
//! sentinel, and a process inside the sandbox recognizes the mask.

use std::path::Path;

use super::super::sandbox_mask::{
    PLUGIN_MASK_SENTINEL_DIR, PLUGIN_MASK_SENTINEL_FILE, plugin_masked_trees, plugin_trees_masked,
};

/// Lay the Linux mask by hand: what a masked tree looks like from inside.
fn plant_sentinel(tree: &Path) {
    std::fs::create_dir_all(tree).expect("tree");
    std::fs::write(tree.join(PLUGIN_MASK_SENTINEL_FILE), b"masked").expect("sentinel");
}

#[cfg(unix)]
#[test]
fn prepare_creates_private_trees_and_the_sentinel_idempotently() {
    use std::os::unix::fs::PermissionsExt;

    use super::super::sandbox_mask::prepare_plugin_mask;

    let root = tempfile::tempdir().expect("tempdir");
    let global = root.path().canonicalize().expect("canonical root");

    let first = prepare_plugin_mask(&global).expect("prepare");
    assert_eq!(first.trees, plugin_masked_trees(&global).to_vec());
    for tree in &first.trees {
        let mode = std::fs::metadata(tree).expect("tree").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", tree.display());
    }
    assert_eq!(first.sentinel, global.join(PLUGIN_MASK_SENTINEL_DIR));
    let sentinel_file = first.sentinel.join(PLUGIN_MASK_SENTINEL_FILE);
    assert!(sentinel_file.is_file());
    assert_eq!(
        std::fs::read_dir(&first.sentinel)
            .expect("sentinel")
            .count(),
        1,
        "the sentinel holds only its marker"
    );

    let again = prepare_plugin_mask(&global).expect("prepare again");
    assert_eq!(again, first, "a second launch reuses what is there");
    assert!(
        !plugin_trees_masked(&global),
        "on the host the trees are the real directories"
    );
}

/// A mask laid over a link would hide the link and leave its target readable.
#[cfg(unix)]
#[test]
fn prepare_refuses_a_linked_tree_or_sentinel() {
    use super::super::sandbox_mask::prepare_plugin_mask;

    let root = tempfile::tempdir().expect("tempdir");
    let global = root.path().canonicalize().expect("canonical root");
    let elsewhere = global.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::create_dir_all(global.join("state")).expect("state");
    std::os::unix::fs::symlink(&elsewhere, global.join("state/plugins")).expect("link");

    assert!(prepare_plugin_mask(&global).is_err());
    assert_eq!(
        std::fs::read_dir(&elsewhere).expect("elsewhere").count(),
        0,
        "nothing is created through the link"
    );

    let root = tempfile::tempdir().expect("tempdir");
    let global = root.path().canonicalize().expect("canonical root");
    let sentinel = global.join(PLUGIN_MASK_SENTINEL_DIR);
    std::fs::create_dir_all(&sentinel).expect("sentinel dir");
    std::os::unix::fs::symlink(
        global.join("planted"),
        sentinel.join(PLUGIN_MASK_SENTINEL_FILE),
    )
    .expect("link");

    assert!(prepare_plugin_mask(&global).is_err());
    assert!(!global.join("planted").exists());
}

#[test]
fn either_tree_standing_in_for_the_sentinel_marks_the_process_masked() {
    for tree in 0..2 {
        let root = tempfile::tempdir().expect("tempdir");
        let trees = plugin_masked_trees(root.path());
        assert!(
            !plugin_trees_masked(root.path()),
            "a fresh root is not masked"
        );
        plant_sentinel(&trees[tree]);
        assert!(plugin_trees_masked(root.path()));
    }
}

/// On macOS the profile denies the tree itself; the same refusal shows as a
/// permission error on the path.
#[cfg(unix)]
#[test]
fn an_unreadable_tree_marks_the_process_masked() {
    use std::os::unix::fs::PermissionsExt;

    // SAFETY: `geteuid` only reads the calling process's credentials.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let tree = plugin_masked_trees(root.path())[1].clone();
    std::fs::create_dir_all(&tree).expect("tree");
    std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let masked = plugin_trees_masked(root.path());

    std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o700)).expect("restore");
    assert!(masked);
}
