//! Retired deterministic stubs fail through the host the engine calls.
//!
//! The engine name-resolution fixture used to assert `revert_on_red` itself,
//! but its host returned `DeterministicActionNotRegistered` for every action.
//! Production dispatch returns `DeterministicActionFailed` and must not
//! report `Ok` for a red head.

use orbit_core::OrbitRuntime;
use orbit_engine::{DispatchError, RuntimeHost};
use orbit_tools::ToolContext;
use serde_json::json;

use super::dispatch_admission::isolated;

#[test]
fn revert_on_red_fails_instead_of_reporting_success() {
    if !isolated("retired_stubs::revert_on_red_fails_instead_of_reporting_success") {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global orbit dir");
    std::fs::create_dir_all(&workspace).expect("workspace orbit dir");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    let error = runtime
        .run_deterministic(
            "revert_on_red",
            &json!({}),
            &json!({
                "commit_sha": "deadbeef",
                "branch": "agent-main",
                "reason": "coverage",
            }),
            ToolContext::default(),
        )
        .expect_err("a red head must not be reported as reverted");

    match error {
        DispatchError::DeterministicActionFailed { action, .. } => {
            assert_eq!(action, "revert_on_red");
        }
        other => panic!("expected DeterministicActionFailed, got {other:?}"),
    }
}
