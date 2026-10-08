use crate::adapter::tool_host::test_support::{
    invalid_input_message, run_tool_as_operator, test_runtime, unmanaged_tool_env_guard,
};
use serde_json::{Value, json};

#[test]
fn files_shape_reservations_reject_outside_workspace_before_persisting() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo_root) = test_runtime();

    let error = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "files": ["file:/outside-workspace/lib.rs"],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect_err("outside-workspace selector is rejected");
    let message = invalid_input_message::<Value>(Err(error));
    assert!(
        message.contains("must remain inside workspace"),
        "{message}"
    );

    let locks = runtime
        .run_tool("orbit.task.locks", json!({}))
        .expect("list task locks");
    assert_eq!(locks["total_reservations"], 0);
}
