use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;

use super::super::disable::ToolDisableArgs;
use super::super::enable::ToolEnableArgs;
use crate::command::Execute;

/// A `register_inactive` builtin: availability is fixed at registration and
/// is never read from the store's enabled flag [ORB-12122].
const REGISTRY_INACTIVE_BUILTIN: &str = "orbit.task.reject";

#[test]
fn tool_enable_refuses_a_registry_inactive_builtin_at_the_cli_boundary() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");

    let error = ToolEnableArgs {
        name: REGISTRY_INACTIVE_BUILTIN.to_string(),
    }
    .execute(&runtime)
    .expect_err("orbit tool enable must refuse a registry-inactive builtin");

    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "unexpected error variant: {error:?}"
    );
    let message = error.to_string();
    assert!(message.contains(REGISTRY_INACTIVE_BUILTIN));
    assert!(message.contains("inactive on the agent tool surface"));

    let tool = runtime
        .show_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("show tool");
    assert!(
        !tool.active,
        "the tool must remain inactive after the refusal"
    );
}

#[test]
fn tool_disable_refuses_a_registry_inactive_builtin_at_the_cli_boundary() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");

    let error = ToolDisableArgs {
        name: REGISTRY_INACTIVE_BUILTIN.to_string(),
    }
    .execute(&runtime)
    .expect_err("orbit tool disable must refuse a registry-inactive builtin");

    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "unexpected error variant: {error:?}"
    );
}
