//! Sibling tests for `sandbox_mask.rs`: the host prepares the trees and the
//! sentinel, and a process inside the sandbox recognizes the mask.

use super::super::sandbox_mask::{PLUGIN_MASK_SENTINEL_DIR, PLUGIN_MASK_SENTINEL_FILE};

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
