//! Provider configuration and activity instructions at the engine boundary.

use orbit_engine::DispatchError;
use orbit_types::workflow::TagInstruction;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::tag_instructions::TagInstructionFailure;
use crate::runtime::engine::paths::codex_workspace_write_writable_dirs;

pub(super) fn agent_provider_config(
    runtime: &OrbitRuntime,
) -> std::collections::HashMap<String, String> {
    let mut config = std::collections::HashMap::new();
    let policy = runtime.codex_execution_policy();
    config.insert("sandbox".to_string(), policy.sandbox().to_string());
    if let Some(approval) = policy.approval_policy() {
        config.insert("approval_policy".to_string(), approval.to_string());
    }
    if policy.sandbox() == "workspace-write" {
        config.insert(
            "writable_dirs_json".to_string(),
            serde_json::to_string(&codex_workspace_write_writable_dirs(
                runtime.context.paths(),
            ))
            .unwrap_or_else(|_| "[]".to_string()),
        );
    }
    // Codex's default login shell rereads profiles, and macOS
    // `path_helper` then moves `workflow.validation_env.path` behind
    // `/usr/bin`; keep the composed PATH by running a non-login shell.
    // [ORB-15204]
    if runtime.validation_env_policy().pins_agent_path() {
        config.insert("allow_login_shell".to_string(), "false".to_string());
    }
    config
}

pub(super) fn tag_instructions_for_activity(
    runtime: &OrbitRuntime,
    run_id: &str,
    activity_name: &str,
    input: &Value,
) -> Result<Vec<TagInstruction>, DispatchError> {
    OrbitRuntime::tag_instructions_for_activity(runtime, run_id, activity_name, input).map_err(
        |failure| match failure {
            TagInstructionFailure::Store(error) => DispatchError::CliInvocationFailed(format!(
                "load tag instructions for `{activity_name}` (run {run_id}): {error}"
            )),
            // The same mapping and files refuse identically on every
            // attempt, so a refusal is not retried.
            TagInstructionFailure::Refused(error) => {
                DispatchError::CliInvocationPermanent(error.to_string())
            }
        },
    )
}
