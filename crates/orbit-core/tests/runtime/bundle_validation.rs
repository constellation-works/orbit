//! `validate_bundles` enforces its allowed-ID set through the public runtime.
//!
//! An explicit empty `known_task_ids` allows no task, so only an empty plan
//! passes. A nonempty set admits its members and refuses every other ID.

use orbit_core::OrbitRuntime;
use orbit_engine::{DispatchError, RuntimeHost};
use orbit_tools::ToolContext;
use serde_json::{Value, json};

fn runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root =
        tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).expect("tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global orbit dir");
    std::fs::create_dir_all(&workspace).expect("workspace orbit dir");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
    (root, runtime)
}

fn validate(
    runtime: &OrbitRuntime,
    bundles: Value,
    known_task_ids: Value,
) -> Result<Value, DispatchError> {
    runtime.run_deterministic(
        "validate_bundles",
        &json!({}),
        &json!({
            "bundles": bundles,
            "max_bundle_size": 5,
            "known_task_ids": known_task_ids,
        }),
        ToolContext::default(),
    )
}

fn assert_unknown_id(error: DispatchError, id: &str) {
    match error {
        DispatchError::DeterministicActionFailed { action, message } => {
            assert_eq!(action, "validate_bundles");
            assert!(
                message.contains(&format!("unknown task_id {id}")),
                "{message}"
            );
        }
        other => panic!("expected DeterministicActionFailed, got {other:?}"),
    }
}

#[test]
fn empty_allowed_set_refuses_every_nonempty_bundle() {
    let (_root, runtime) = runtime();

    let passed =
        validate(&runtime, json!([]), json!([])).expect("an empty plan names no unknown task_id");
    assert_eq!(passed["bundle_count"], 0);

    let error = validate(&runtime, json!([["unknown"]]), json!([]))
        .expect_err("an empty allowed set must not admit a nonempty bundle");
    assert_unknown_id(error, "unknown");
}

#[test]
fn nonempty_allowed_set_admits_only_its_members() {
    let (_root, runtime) = runtime();

    let passed = validate(&runtime, json!([["known"]]), json!(["known"]))
        .expect("a member of the allowed set is admitted");
    assert_eq!(passed["bundle_count"], 1);

    let error = validate(&runtime, json!([["unknown"]]), json!(["known"]))
        .expect_err("a non-member of the allowed set is refused");
    assert_unknown_id(error, "unknown");
}
