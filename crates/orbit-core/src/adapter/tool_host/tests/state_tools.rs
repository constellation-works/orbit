use std::path::{Path, PathBuf};

use orbit_tools::OrbitTaskScope;
use orbit_types::workflow::PipelineState;
use serde_json::json;
use tempfile::tempdir;

use super::super::state_tools;

fn create_run(orbit_root: &Path, job_id: &str, run_id: &str, marker: &str) -> PathBuf {
    let run_dir = orbit_root
        .join("state")
        .join("job-runs")
        .join(job_id)
        .join(run_id);
    std::fs::create_dir_all(&run_dir).expect("create run dir");
    let state = PipelineState::new(
        run_id.to_string(),
        job_id.to_string(),
        json!({ "marker": marker }),
    );
    let content = serde_json::to_string_pretty(&state).expect("serialize state");
    std::fs::write(run_dir.join("state.json"), content).expect("write state");
    run_dir
}

fn scope_for(orbit_root: &Path, run_id: Option<&str>) -> OrbitTaskScope {
    OrbitTaskScope {
        orbit_root: Some(orbit_root.to_path_buf()),
        task_id: None,
        run_id: run_id.map(ToOwned::to_owned),
    }
}

#[test]
fn state_get_rejects_absolute_state_dir_outside_current_workspace() {
    let current = tempdir().expect("current tempdir");
    let other = tempdir().expect("other tempdir");
    let current_orbit_root = current.path().join(".orbit");
    let other_orbit_root = other.path().join(".orbit");
    create_run(&current_orbit_root, "job-test", "jrun-current", "current");
    let other_run_dir = create_run(&other_orbit_root, "job-test", "jrun-current", "other");

    let error = state_tools::get(
        &scope_for(&current_orbit_root, Some("jrun-current")),
        json!({ "state_dir": other_run_dir.display().to_string() }),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("outside"), "{error}");
}

#[test]
fn state_get_rejects_other_run_state_dir_in_current_workspace() {
    let temp = tempdir().expect("tempdir");
    let orbit_root = temp.path().join(".orbit");
    create_run(&orbit_root, "job-test", "jrun-current", "current");
    let other_run_dir = create_run(&orbit_root, "job-test", "jrun-other", "other");

    let error = state_tools::get(
        &scope_for(&orbit_root, Some("jrun-current")),
        json!({ "state_dir": other_run_dir.display().to_string() }),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("active run 'jrun-current'"), "{error}");
}

#[test]
fn state_get_rejects_traversal_state_dir() {
    let temp = tempdir().expect("tempdir");
    let orbit_root = temp.path().join(".orbit");
    create_run(&orbit_root, "job-test", "jrun-current", "current");
    let traversal = orbit_root
        .join("state")
        .join("job-runs")
        .join("job-test")
        .join("..")
        .join("job-test")
        .join("jrun-current");

    let error = state_tools::get(
        &scope_for(&orbit_root, Some("jrun-current")),
        json!({ "state_dir": traversal.display().to_string() }),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("must not contain `..`"), "{error}");
}

#[test]
fn state_get_rejects_traversal_run_id() {
    let temp = tempdir().expect("tempdir");
    let orbit_root = temp.path().join(".orbit");
    create_run(&orbit_root, "job-test", "jrun-current", "current");

    let error = state_tools::get(
        &scope_for(&orbit_root, None),
        json!({ "run_id": "../jrun-current" }),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("single path component"), "{error}");
}
