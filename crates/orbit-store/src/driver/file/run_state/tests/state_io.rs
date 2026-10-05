// Migrated from state_io.rs per ORB-00231
use super::super::*;

use tempfile::tempdir;

fn create_run_dir(orbit_root: &std::path::Path, job_id: &str, run_id: &str) -> std::path::PathBuf {
    let run_dir = orbit_root
        .join("state")
        .join("job-runs")
        .join(job_id)
        .join(run_id);
    std::fs::create_dir_all(&run_dir).expect("create run dir");
    run_dir
}

#[test]
fn resolve_active_run_state_dir_rejects_traversal_run_id() {
    let temp = tempdir().expect("tempdir");
    let orbit_root = temp.path().join(".orbit");
    create_run_dir(&orbit_root, "job-test", "jrun-current");

    let error = resolve_active_run_state_dir(&orbit_root, "../jrun-current")
        .unwrap_err()
        .to_string();

    assert!(error.contains("single path component"), "{error}");
}

#[test]
fn validate_active_run_state_dir_rejects_absolute_path_outside_workspace() {
    let current = tempdir().expect("current tempdir");
    let other = tempdir().expect("other tempdir");
    let current_orbit_root = current.path().join(".orbit");
    let other_orbit_root = other.path().join(".orbit");
    create_run_dir(&current_orbit_root, "job-test", "jrun-current");
    let other_run_dir = create_run_dir(&other_orbit_root, "job-test", "jrun-current");

    let error = validate_active_run_state_dir(&current_orbit_root, &other_run_dir, "jrun-current")
        .unwrap_err()
        .to_string();

    assert!(error.contains("outside"), "{error}");
}

#[test]
fn validate_active_run_state_dir_rejects_traversal_state_dir() {
    let temp = tempdir().expect("tempdir");
    let orbit_root = temp.path().join(".orbit");
    create_run_dir(&orbit_root, "job-test", "jrun-current");
    let traversal = orbit_root
        .join("state")
        .join("job-runs")
        .join("job-test")
        .join("..")
        .join("job-test")
        .join("jrun-current");

    let error = validate_active_run_state_dir(&orbit_root, &traversal, "jrun-current")
        .unwrap_err()
        .to_string();

    assert!(error.contains("must not contain `..`"), "{error}");
}
