use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;

use super::super::disable::ToolDisableArgs;
use super::super::enable::ToolEnableArgs;
use crate::command::Execute;
use crate::command::locks::LocksListArgs;

/// A `register_inactive` builtin: it is absent from the agent surface but can
/// still be reached through administrative CLI paths.
const REGISTRY_INACTIVE_BUILTIN: &str = "orbit.task.locks";

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
fn tool_disable_allows_a_registry_inactive_builtin_at_the_cli_boundary() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");

    ToolDisableArgs {
        name: REGISTRY_INACTIVE_BUILTIN.to_string(),
    }
    .execute(&runtime)
    .expect("orbit tool disable should persist the administrative state");

    let disabled = runtime
        .show_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("show tool");
    assert!(!disabled.active);
    assert!(!disabled.enabled);
}

#[test]
fn tool_enable_restores_a_stored_disabled_registry_inactive_builtin() {
    let runtime = OrbitRuntime::in_memory().expect("in-memory runtime");

    ToolDisableArgs {
        name: REGISTRY_INACTIVE_BUILTIN.to_string(),
    }
    .execute(&runtime)
    .expect("disable");

    ToolEnableArgs {
        name: REGISTRY_INACTIVE_BUILTIN.to_string(),
    }
    .execute(&runtime)
    .expect("orbit tool enable should restore the stored state");

    let enabled = runtime
        .show_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("show tool");
    assert!(!enabled.active);
    assert!(enabled.enabled);

    LocksListArgs { json: true }
        .execute(&runtime)
        .expect("CLI lock wrapper should reach the restored inactive builtin");
}
