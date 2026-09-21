use orbit_common::test_env::ScopedEnv;

use crate::OrbitRuntime;
use crate::adapter::command::dispatch::take_tool_audit_recorded;

/// Serializes tests that mutate `ORBIT_AGENT_*` or assert environment-derived
/// audit roles so parallel test execution cannot race environment writers.
///
/// Holds the process-wide guard every env-mutating test in this binary
/// shares (not a lock of its own), clears the variables these tests set or
/// assert on, and restores whatever was there when dropped.
pub(super) fn env_guard() -> ScopedEnv {
    orbit_common::test_env::unset([
        "ORBIT_AGENT_NAME",
        "ORBIT_AGENT_MODEL",
        "ORBIT_TASK_ID",
        "ORBIT_RUN_ID",
        crate::runtime::run_input::ORBIT_MANAGED_RUN_CONTEXT_ENV,
        "ORBIT_ACTIVITY_ID",
        "ORBIT_STEP_INDEX",
        crate::adapter::command::dispatch::ORBIT_PLUGIN_ENV,
        "ORBIT_ALLOWED_TOOLS",
    ])
}

pub(super) fn clear_identity_env() {
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var("ORBIT_AGENT_NAME");
        std::env::remove_var("ORBIT_AGENT_MODEL");
    }
}

pub(super) fn set_identity_env(agent: &str, model: &str) {
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::set_var("ORBIT_AGENT_NAME", agent);
        std::env::set_var("ORBIT_AGENT_MODEL", model);
    }
}

pub(super) fn fresh_runtime() -> OrbitRuntime {
    // Reset the dedup signal so cross-test thread-local leakage cannot mask
    // bugs in the per-call set/clear cycle.
    let _ = take_tool_audit_recorded();
    clear_identity_env();
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}
