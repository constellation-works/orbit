use serde_json::Value;

/// Environment marker emitted only for an Orbit-managed activity together
/// with the owning run id. Both values form the managed-run trust boundary:
/// the marker alone is not enough to attribute work to a run or to grant
/// managed-child behavior.
pub(crate) const ORBIT_MANAGED_RUN_CONTEXT_ENV: &str = "ORBIT_MANAGED_RUN_CONTEXT";

/// Return the trusted managed run id from the activity envelope.
///
/// A managed child may have a deliberately narrower process view than its
/// host worker (for example, under Bubblewrap's private PID namespace). Keep
/// the marker and non-blank run id coupled so callers can make decisions at
/// that authority boundary without trusting a standalone environment marker.
pub(crate) fn managed_run_context_run_id_from_env() -> Option<String> {
    let managed = std::env::var(ORBIT_MANAGED_RUN_CONTEXT_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"));
    if !managed {
        return None;
    }

    std::env::var("ORBIT_RUN_ID")
        .ok()
        .and_then(|value| non_empty(&value).map(ToOwned::to_owned))
}

/// Whether this process is a trusted child of an Orbit-managed run.
pub(crate) fn managed_run_context_from_env() -> bool {
    managed_run_context_run_id_from_env().is_some()
}

/// Managed provenance sufficient to route a child to its dispatching
/// workspace and registry. This does not grant job-run authority.
pub(crate) fn managed_dispatch_context_from_env() -> bool {
    let managed = std::env::var(ORBIT_MANAGED_RUN_CONTEXT_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"));
    managed
        && (managed_run_context_from_env()
            || std::env::var("ORBIT_SESSION_ID")
                .ok()
                .is_some_and(|value| non_empty(&value).is_some()))
}

/// Logical workspace selector carried by a managed child (`ORBIT_WORKSPACE`).
///
/// Honored only with Orbit's managed marker and a non-blank run or invocation
/// identity. Source-inspection providers have an invocation session but no job
/// run. A standalone process that merely inherits the selector must not bind.
pub fn managed_workspace_selector_from_env() -> Option<String> {
    if !managed_dispatch_context_from_env() {
        return None;
    }
    std::env::var("ORBIT_WORKSPACE")
        .ok()
        .and_then(|value| non_empty(&value).map(ToOwned::to_owned))
}

/// Extract the singular task id from run/activity input shapes that are meant
/// to identify exactly one task.
pub(crate) fn singular_task_id_from_input(input: &Value) -> Option<&str> {
    input
        .get("task_id")
        .and_then(Value::as_str)
        .and_then(non_empty)
        .or_else(|| {
            input
                .get("task")
                .and_then(|task| task.get("id"))
                .and_then(Value::as_str)
                .and_then(non_empty)
        })
        .or_else(|| {
            let items = input.get("task_ids")?.as_array()?;
            if items.len() == 1 {
                items.first()?.as_str().and_then(non_empty)
            } else {
                None
            }
        })
}

pub(crate) fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
