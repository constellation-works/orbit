//! [ORB-14748] A child pipeline that ended `held` is settled but not failed:
//! `pipeline_success_guard` passes it, so a gate wrapping a held delivery
//! does not end failed while the task stays in progress. Real failures still
//! fail the guard.

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};
use tempfile::TempDir;

fn open() -> (TempDir, OrbitRuntime) {
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    (root, runtime)
}

fn guard(runtime: &OrbitRuntime, input: Value) -> Result<Value, String> {
    runtime
        .run_deterministic(
            "pipeline_success_guard",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .map_err(|error| error.to_string())
}

#[test]
fn held_child_passes_the_success_guard_and_is_counted() {
    let (_root, runtime) = open();
    let held = json!({"status": "held", "run_id": "jrun-held"});

    let single = guard(&runtime, json!({"result": held})).expect("a held child is not a failure");
    assert_eq!(single["succeeded"], true, "{single}");
    assert_eq!(single["held_count"], 1, "{single}");

    let batch = guard(
        &runtime,
        json!({"results": [held, {"status": "success", "run_id": "jrun-ok"}]}),
    )
    .expect("a held child beside a success is not a failure");
    assert_eq!(batch["checked_count"], 2, "{batch}");
    assert_eq!(batch["held_count"], 1, "{batch}");
}

#[test]
fn a_failed_sibling_still_fails_the_guard_when_another_child_is_held() {
    let (_root, runtime) = open();
    let error = guard(
        &runtime,
        json!({"results": [
            {"status": "held", "run_id": "jrun-held"},
            {"status": "failed", "run_id": "jrun-bad", "error": "boom"},
        ]}),
    )
    .expect_err("a failed child fails the guard");
    assert!(error.contains("jrun-bad"), "{error}");
    assert!(!error.contains("jrun-held"), "{error}");
}

#[test]
fn recording_mode_counts_a_held_child_separately_from_failures() {
    let (_root, runtime) = open();
    let output = guard(
        &runtime,
        json!({"allow_non_success": true, "results": [
            {"status": "held", "run_id": "jrun-held"},
            {"status": "success", "run_id": "jrun-ok"},
            {"status": "failed", "run_id": "jrun-bad"},
        ]}),
    )
    .expect("terminal outcomes are recorded");
    assert_eq!(output["held_count"], 1, "{output}");
    assert_eq!(output["succeeded_count"], 1, "{output}");
    assert_eq!(output["non_success_count"], 1, "{output}");
}
