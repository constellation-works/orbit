//! Admitted unit tests: platform-kernel disk reads and combinatorial pure CPU
//! window selection with injected ticks/time/waits. No load generation.
use orbit_core::runtime::host_resource::default_host_resource_probe;
use std::path::Path;

#[test]
fn missing_descendant_reports_its_existing_ancestor_filesystem() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "missing_descendant_reports_its_existing_ancestor_filesystem",
    ) {
        return;
    }

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
    if !crate::admission_audit::isolate(
        module_path!(),
        "relative_path_is_refused_instead_of_resolved_against_the_working_directory",
    ) {
        return;
    }

    assert!(
        disk_percent(Path::new("crates/orbit-core")).is_none(),
        "a relative probe path must be refused, never resolved against the working directory"
    );
}

fn disk_percent(path: &Path) -> Option<f64> {
    default_host_resource_probe()
        .sample(&[path.to_path_buf()])
        .disks[0]
        .used_percent
}
