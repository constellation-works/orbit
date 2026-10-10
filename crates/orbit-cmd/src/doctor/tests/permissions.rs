//! Unit admission: ownership safety and deterministic filesystem disappearance injection.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;

use crate::doctor::permissions::scan_with_hook;

#[test]
fn repair_refuses_final_and_parent_symlink_swaps_after_scanning() {
    use crate::doctor::permissions::tighten_directory;
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("fixture");
    let root = temp.path().canonicalize().expect("canonical fixture");
    let outside = root.join("outside");
    let outside_child = outside.join("child");
    fs::create_dir_all(&outside_child).expect("outside directory");
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o777)).unwrap();
    fs::set_permissions(&outside_child, fs::Permissions::from_mode(0o777)).unwrap();
    let scanned = root.join("scanned");
    fs::create_dir_all(scanned.join("child")).expect("scanned directory");
    fs::set_permissions(&scanned, fs::Permissions::from_mode(0o777)).unwrap();
    let (_, writable) = scan_with_hook(&scanned, &mut |_| {}).expect("scan before swap");
    assert!(writable.iter().any(|(path, _)| path == &scanned));
    fs::rename(&scanned, root.join("retained")).expect("move original directory");
    symlink(&outside, &scanned).expect("swap a directory for a link");
    for path in [&scanned, &scanned.join("child")] {
        assert!(
            tighten_directory(path).is_err(),
            "the repair must refuse symlinks at every component, including a parent swapped after scanning"
        );
    }
    for path in [&outside, &outside_child] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o777,
            "a raced symlink must never redirect chmod outside the scanned tree"
        );
    }
}

#[test]
fn ownership_scan_never_visits_or_reports_worktree_or_target_contents() {
    let temp = tempfile::tempdir().expect("fixture");
    let state = temp.path().join("state");
    let worktree = state.join("worktrees/run");
    let target = worktree.join("target");
    for index in 0..10_000 {
        let child = target.join(index.to_string());
        fs::create_dir_all(&child).expect("fake target tree");
        fs::set_permissions(child, fs::Permissions::from_mode(0o777))
            .expect("writable Cargo directory");
    }
    let owned = state.join("owned");
    let other_target = owned.join("target");
    fs::create_dir_all(other_target.join("nested")).expect("target outside a worktree");
    fs::set_permissions(&owned, fs::Permissions::from_mode(0o777))
        .expect("writable Orbit directory");
    fs::set_permissions(&other_target, fs::Permissions::from_mode(0o777))
        .expect("writable Cargo directory");
    let (visited, writable) = scan_with_hook(&state, &mut |_| {}).expect("scan");
    assert!(visited.contains(&owned));
    assert!(writable.iter().any(|(path, _)| path == &owned));
    assert!(
        !visited
            .iter()
            .any(|path| path.starts_with(&worktree) || path.starts_with(&other_target)),
        "Orbit's ownership boundary must exclude all 10k Cargo directories and any target directory"
    );
    assert!(
        !writable
            .iter()
            .any(|(path, _)| path.starts_with(&worktree) || path.starts_with(&other_target)),
        "non-owned checkout and Cargo modes must never appear as permission offenders"
    );
}

#[test]
fn disappearing_entries_are_skipped_at_metadata_and_read_dir_boundaries() {
    let temp = tempfile::tempdir().expect("fixture");
    let missing = temp.path().join("already-gone");
    assert!(
        scan_with_hook(&missing, &mut |_| {})
            .expect("missing entry is skipped")
            .1
            .is_empty()
    );
    let disappearing = temp.path().join("disappearing");
    fs::create_dir(&disappearing).expect("fixture directory");
    fs::set_permissions(&disappearing, fs::Permissions::from_mode(0o777))
        .expect("writable disappearing directory");
    let mut injected = false;
    let (_, writable) = scan_with_hook(&disappearing, &mut |path| {
        assert_eq!(path, disappearing);
        fs::remove_dir(path).expect("force disappearance after stat and before read_dir");
        injected = true;
    })
    .expect("a directory vanishing between stat and read_dir must not make doctor fail");
    assert!(injected, "the deterministic TOCTOU injection must execute");
    assert!(
        writable.is_empty(),
        "a vanished directory must not remain a permission offender"
    );
}
