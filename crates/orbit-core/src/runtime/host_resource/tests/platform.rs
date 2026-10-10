//! Admitted unit tests: platform-kernel behaviour of the disk probe. Reads only
//! the stat of the crate's own directory; nothing is created.
use super::super::platform::disk_percent;
use std::path::Path;

#[test]
fn missing_descendant_reports_its_existing_ancestor_filesystem() {
    let existing = Path::new(env!("CARGO_MANIFEST_DIR"));
    let missing = existing.join("no-such-dir-for-disk-probe/nested");
    assert!(disk_percent(existing).is_some());
    assert!(
        disk_percent(&missing).is_some(),
        "a worktrees directory that does not exist yet must use its nearest existing ancestor"
    );
}

#[test]
fn relative_path_is_refused_instead_of_resolved_against_the_working_directory() {
    assert!(
        disk_percent(Path::new("crates/orbit-core")).is_none(),
        "a relative probe path must be refused, never resolved against the working directory"
    );
}
