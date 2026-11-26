//! Managed-run environment marker parsing (`ORBIT_MANAGED_RUN_CONTEXT`, run/task IDs); imports `std` and `orbit-common` only.

/// Environment marker emitted with the owning run ID for a managed activity.
pub use orbit_common::env_vars::ORBIT_MANAGED_RUN_CONTEXT as ORBIT_MANAGED_RUN_CONTEXT_ENV;

/// Read the managed run ID declared by this process for routing and attribution.
/// Children can change their environment, so this ID cannot authenticate a resident.
pub fn managed_run_context_run_id_from_env(env: &orbit_common::Env) -> Option<String> {
    let managed = env
        .orbit_managed_run_context()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"));
    if !managed {
        return None;
    }

    env.orbit_run_id()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|value| non_empty(value).map(ToOwned::to_owned))
}

/// The task a managed run's environment names (`ORBIT_TASK_ID`), honored
/// only with the managed marker and a non-blank run id, like
/// [`managed_run_context_run_id_from_env`].
pub fn managed_run_context_task_id_from_env(env: &orbit_common::Env) -> Option<String> {
    managed_run_context_run_id_from_env(env)?;
    env.orbit_task_id()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|value| non_empty(value).map(ToOwned::to_owned))
}

/// Whether this process declares a managed run context.
pub fn managed_run_context_from_env(env: &orbit_common::Env) -> bool {
    managed_run_context_run_id_from_env(env).is_some()
}

/// Managed provenance sufficient to route a child to its dispatching
/// workspace and registry. This does not grant job-run authority.
pub fn managed_dispatch_context_from_env(env: &orbit_common::Env) -> bool {
    let managed = env
        .orbit_managed_run_context()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"));
    managed
        && (managed_run_context_from_env(env)
            || env
                .orbit_session_id()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|value| non_empty(value).is_some()))
}

/// Logical workspace selector carried by a managed child (`ORBIT_WORKSPACE`).
///
/// Honored only with Orbit's managed marker and a non-blank run or invocation
/// identity. Source-inspection providers have an invocation session but no job
/// run. A standalone process that merely inherits the selector must not bind.
pub fn managed_workspace_selector_from_env(env: &orbit_common::Env) -> Option<String> {
    if !managed_dispatch_context_from_env(env) {
        return None;
    }
    env.orbit_workspace()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|value| non_empty(value).map(ToOwned::to_owned))
}

pub fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
