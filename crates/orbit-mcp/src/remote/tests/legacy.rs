//! Retired caller-authorization leftovers are reported, never enforced.

use super::super::legacy::{
    LEGACY_ACCEPTANCE_DIR, LEGACY_CALLERS_FILE, ignored_caller_authorization_paths,
    warn_ignored_caller_authorization,
};

#[test]
fn a_machine_with_no_leftovers_reports_nothing() {
    let root = tempfile::tempdir().expect("global root");

    assert!(ignored_caller_authorization_paths(root.path()).is_empty());
    // Warning on an empty set would make every clean startup noisy.
    warn_ignored_caller_authorization(root.path());
}

/// [ORB-12564] A destination upgraded from the destination-side model keeps a
/// file whose whole purpose was to refuse callers. Naming it is the entire
/// obligation — nothing here parses it, and nothing fails.
#[test]
fn both_retired_paths_are_reported_without_being_read() {
    let root = tempfile::tempdir().expect("global root");
    std::fs::write(
        root.path().join(LEGACY_CALLERS_FILE),
        "this is not even valid toml = [",
    )
    .expect("leftover callers file");
    std::fs::create_dir_all(root.path().join(LEGACY_ACCEPTANCE_DIR)).expect("leftover acceptance");

    let ignored = ignored_caller_authorization_paths(root.path());

    assert_eq!(
        ignored,
        vec![
            root.path().join(LEGACY_CALLERS_FILE),
            root.path().join(LEGACY_ACCEPTANCE_DIR),
        ]
    );
    warn_ignored_caller_authorization(root.path());
}
