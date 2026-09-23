//! Sibling tests for `run_input.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use serde_json::json;

use super::super::run_input::{managed_workspace_selector_from_env, singular_task_id_from_input};

#[test]
fn singular_task_id_accepts_single_entry_task_ids() {
    let input = json!({ "task_ids": [" ORB-00073 "] });

    assert_eq!(singular_task_id_from_input(&input), Some("ORB-00073"));
}

#[test]
fn singular_task_id_rejects_multi_task_input() {
    let input = json!({ "task_ids": ["ORB-00073", "ORB-00078"] });

    assert_eq!(singular_task_id_from_input(&input), None);
}

#[test]
fn managed_workspace_selector_requires_the_full_trust_boundary() {
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", Some("jrun-workspace-selector")),
        ("ORBIT_SESSION_ID", None),
        ("ORBIT_WORKSPACE", Some(" ws_orbit ")),
    ]);
    assert_eq!(
        managed_workspace_selector_from_env().as_deref(),
        Some("ws_orbit")
    );
}

#[test]
fn managed_inspection_session_binds_without_a_job_run() {
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", None),
        ("ORBIT_SESSION_ID", Some("inspection-invocation")),
        ("ORBIT_WORKSPACE", Some("ws_orbit")),
    ]);
    assert_eq!(
        managed_workspace_selector_from_env().as_deref(),
        Some("ws_orbit")
    );
}

#[test]
fn managed_marker_without_a_run_or_inspection_session_does_not_bind() {
    let _env = orbit_common::test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", None),
        ("ORBIT_SESSION_ID", None),
        ("ORBIT_WORKSPACE", Some("ws_orbit")),
    ]);
    assert_eq!(managed_workspace_selector_from_env(), None);
}

#[test]
fn managed_workspace_selector_ignores_an_unmanaged_or_blank_value() {
    let _unmanaged = orbit_common::test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", None),
        ("ORBIT_RUN_ID", None),
        ("ORBIT_SESSION_ID", Some("inspection-invocation")),
        ("ORBIT_WORKSPACE", Some("ws_orbit")),
    ]);
    assert_eq!(managed_workspace_selector_from_env(), None);
    drop(_unmanaged);

    let _blank = orbit_common::test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", None),
        ("ORBIT_SESSION_ID", Some("   ")),
        ("ORBIT_WORKSPACE", Some("   ")),
    ]);
    assert_eq!(managed_workspace_selector_from_env(), None);
}
