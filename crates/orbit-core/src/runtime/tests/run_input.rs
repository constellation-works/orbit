//! Sibling tests for `run_input.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use super::super::run_input::managed_workspace_selector_from_env;

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
