use orbit_common::OrbitError;

use super::support::fresh_runtime;

/// A `register_inactive` builtin: availability is fixed at registration and
/// is never read from the store's enabled flag [ORB-12122].
const REGISTRY_INACTIVE_BUILTIN: &str = "orbit.task.reject";

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
fn disable_refuses_a_registry_inactive_builtin_the_same_way_as_enable() {
    let runtime = fresh_runtime();

    let error = runtime
        .disable_tool(REGISTRY_INACTIVE_BUILTIN)
        .expect_err("disable must refuse a registry-inactive builtin");
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "unexpected error variant: {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("inactive on the agent tool surface")
    );
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
