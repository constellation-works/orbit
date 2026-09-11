use orbit_common::OrbitError;
use orbit_types::tool::StoredTool;
use serde_json::json;

use super::support::fresh_runtime;

/// A `register_inactive` builtin: it is absent from the agent surface but can
/// still be reached through the administrative `run_tool` path.
const REGISTRY_INACTIVE_BUILTIN: &str = "orbit.task.locks";

#[test]
fn enable_refuses_a_registry_inactive_builtin_instead_of_reporting_success() {
    let runtime = fresh_runtime();

    let before = runtime
        .show_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("show tool");
    assert!(!before.active);

    let error = runtime
        .enable_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect_err("enable must refuse a registry-inactive builtin");
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "unexpected error variant: {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("inactive on the agent tool surface")
    );
    assert!(
        !error
            .to_string()
            .contains("without changing whether the tool can run")
    );

    let after = runtime
        .show_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("show tool");
    assert!(!after.active);
    assert_eq!(
        before.enabled, after.enabled,
        "enabled flag must be untouched"
    );
}

#[test]
fn disable_allows_a_registry_inactive_builtin_to_be_disabled() {
    let runtime = fresh_runtime();

    runtime
        .disable_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("disable should persist the administrative state");

    let tool = runtime
        .show_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("show tool");
    assert!(!tool.active);
    assert!(!tool.enabled);
}

#[test]
fn enable_restores_a_stored_disabled_registry_inactive_builtin_for_run_tool() {
    let runtime = fresh_runtime();
    let schema = runtime
        .show_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("show tool");
    runtime
        .stores()
        .tools()
        .insert_tool(&StoredTool {
            name: schema.name.clone(),
            path: String::new(),
            description: schema.description,
            enabled: false,
            builtin: schema.builtin,
            parameters: schema.parameters,
        })
        .expect("seed disabled tool row");

    runtime
        .enable_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("enable should restore a stored disabled tool");
    let enabled = runtime
        .show_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect("show tool");
    assert!(!enabled.active);
    assert!(enabled.enabled);

    let error = runtime
        .ensure_tool_agent_facing(REGISTRY_INACTIVE_BUILTIN)
        .expect_err("inactive tool must remain unavailable to agents");
    assert!(
        error
            .to_string()
            .contains("inactive on the agent tool surface")
    );

    runtime
        .run_tool(REGISTRY_INACTIVE_BUILTIN, json!({}))
        .expect("run_tool must reach the restored builtin");
}

#[test]
fn enable_and_disable_round_trip_for_an_active_builtin() {
    let runtime = fresh_runtime();
    const ACTIVE_BUILTIN: &str = "orbit.task.list";

    runtime.disable_tool(ACTIVE_BUILTIN).expect("disable");
    let disabled = runtime.show_tool(ACTIVE_BUILTIN).expect("show tool");
    assert!(disabled.active);
    assert!(!disabled.enabled);

    runtime.enable_tool(ACTIVE_BUILTIN).expect("enable");
    let enabled = runtime.show_tool(ACTIVE_BUILTIN).expect("show tool");
    assert!(enabled.active);
    assert!(enabled.enabled);
}
