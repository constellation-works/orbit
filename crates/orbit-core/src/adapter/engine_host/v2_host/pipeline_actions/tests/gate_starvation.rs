use super::super::*;
use crate::OrbitRuntime;
use serde_json::json;

use super::results::action_failure_message;

#[test]
fn gate_starvation_fail_names_both_conflicting_files_and_unmet_dependencies() {
    // A dependency-starved gate previously reported an empty
    // `conflicting_files` list and named no blocker at all.
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let message = action_failure_message(
        gate_starvation_fail(
            &runtime,
            "gate_starvation_fail",
            &json!({
                "task_ids": ["ORB-2"],
                "conflicts": [],
                "waiting_on_deps": ["ORB-1"],
                "max_wait_seconds": 3600,
            }),
        )
        .expect_err("starvation always fails the run"),
        "gate_starvation_fail",
    );

    assert!(message.contains("gate.starvation"), "{message}");
    assert!(message.contains("ORB-1"), "{message}");
    assert!(message.contains("waiting_on_deps"), "{message}");
}

#[test]
fn gate_starvation_fail_tolerates_a_missing_waiting_on_deps_input() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let message = action_failure_message(
        gate_starvation_fail(
            &runtime,
            "gate_starvation_fail",
            &json!({
                "task_ids": ["ORB-2"],
                "conflicts": [{ "file": "file:src/lib.rs", "held_by": "task", "held_by_id": "ORB-3" }],
            }),
        )
        .expect_err("starvation always fails the run"),
        "gate_starvation_fail",
    );

    assert!(message.contains("file:src/lib.rs"), "{message}");
    assert!(message.contains("waiting_on_deps=[]"), "{message}");
}
