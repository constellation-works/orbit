//! Authorization policy data, caller capability resolution, and execution policy bundle.

use std::collections::BTreeSet;

use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, CallerProvenance,
};
use orbit_config::{CodexExecutionPolicy, ExecutionEnvPolicy};
use orbit_policy::PolicyEngine;
use orbit_types::tool::{McpCapability, ToolSessionContext};

/// Canonical tool name of the delegated-human decision submission, named once for its admission.
pub(crate) const DECISION_SUBMIT_OPERATION_ID: &str = "orbit.decision.submit";

/// Canonical tool name of the decision digest. It shares the submission's
/// callback admission: the row exists for the registered channel's plugin,
/// which holds a callback session rather than a capability.
pub(crate) const DECISION_DIGEST_OPERATION_ID: &str = "orbit.decision.digest";

/// Canonical tool name of the explicit ship submission, named once for its admission.
pub(crate) const WORKFLOW_SHIP_OPERATION_ID: &str = "orbit.workflow.ship";

/// Canonical tool name of a delivery pull request's landing.
pub(crate) const WORKFLOW_LAND_OPERATION_ID: &str = "orbit.workflow.land";

/// Canonical tool name of a delivery pull request's base-branch refresh.
pub(crate) const WORKFLOW_REFRESH_OPERATION_ID: &str = "orbit.workflow.refresh";

/// Canonical tool name of `orbit.task.recover` action `repilot`.
pub(crate) const TASK_REPILOT_OPERATION_ID: &str = "orbit.task.repilot";

/// Canonical tool name of `orbit.task.recover` action `requeue`.
pub(crate) const TASK_REQUEUE_OPERATION_ID: &str = "orbit.task.requeue";

/// Canonical tool name of `orbit.task.recover` action `resume_delivery`.
pub(crate) const TASK_RESUME_DELIVERY_OPERATION_ID: &str = "orbit.task.resume_delivery";

/// The operator-row tools a resident cycle reaches through its mandate's
/// rights instead: ship, re-pilot and requeue under `dispatch`, land under
/// `merge`, refresh under `refresh`, and delivery recovery under `recover`. Every other operator row, the other
/// recover actions included, holds a cycle to the operator capability.
const RESIDENT_CYCLE_RIGHT_GATED_OPERATIONS: [&str; 6] = [
    WORKFLOW_SHIP_OPERATION_ID,
    WORKFLOW_LAND_OPERATION_ID,
    WORKFLOW_REFRESH_OPERATION_ID,
    TASK_REPILOT_OPERATION_ID,
    TASK_REQUEUE_OPERATION_ID,
    TASK_RESUME_DELIVERY_OPERATION_ID,
];

/// Policy engine and child-execution policies held by [`crate::base::Base`].
#[derive(Clone)]
pub(crate) struct OrbitPolicyContext {
    pub(crate) policy: PolicyEngine,
    pub(crate) execution_env_policy: ExecutionEnvPolicy,
    pub(crate) codex_execution_policy: CodexExecutionPolicy,
}

impl OrbitPolicyContext {
    pub(crate) fn new(
        policy: PolicyEngine,
        execution_env_policy: ExecutionEnvPolicy,
        codex_execution_policy: CodexExecutionPolicy,
    ) -> Self {
        Self {
            policy,
            execution_env_policy,
            codex_execution_policy,
        }
    }
}

/// Identity facts retained after operator admission.
#[derive(Debug)]
pub struct OperatorAuthorizer {
    pub provenance: CallerProvenance,
    /// Caller label an SSH-originated MCP session forwarded, for attribution
    /// in the durable admission. Never a grant [ORB-12564].
    pub remote_caller_machine_id: Option<String>,
}

/// Whether `tool` is an operator-row tool a resident cycle reaches through
/// its mandate's rights rather than the operator capability.
pub fn is_resident_cycle_right_gated_operation(tool: &str) -> bool {
    RESIDENT_CYCLE_RIGHT_GATED_OPERATIONS.contains(&tool)
}

/// The read-only workflow status call a live resident may use in scope.
pub fn is_resident_drain_status_call(tool: &str, input: &serde_json::Value) -> bool {
    tool == "orbit.workflow.auto"
        && input
            .get("action")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|action| action.trim() == "status")
}

/// Routing hint for a nested client. The resident broker authenticates the
/// caller and resolves the target workspace against the live cycle's grant.
pub fn route_resident_delivery_recovery_to_broker(env: &orbit_common::Env, tool: &str) -> bool {
    tool == TASK_RESUME_DELIVERY_OPERATION_ID
        && env.orbit_plugin_broker().is_some()
        && crate::base::managed_env::managed_run_context_run_id_from_env(env).is_some()
}

/// What a caller the chokepoint has already admitted actually holds.
///
/// A handler that has to draw one more distinction *inside* an authorized call
/// asks here instead of reading `effective_capabilities` off the session. A
/// session's asserted set is not where every surface keeps its authority:
/// `orbit tool run` builds a session with no capabilities at all and expresses
/// the caller's authority in the process envelope, so a raw session read
/// refuses the owner on the owner's own machine [ORB-12582].
///
/// The resolution is the shared one, in the shared order: a session that
/// asserts grants is taken at its word, and the process envelope is consulted
/// only for a session that asserts nothing. An MCP session is therefore never
/// widened by whatever process happens to host its server — and a session that
/// asserts nothing does not reach a handler that calls this, because the
/// governed floor on that handler's tool refused it first.
pub fn resolved_caller_capabilities(
    env: &orbit_common::Env,
    session: &ToolSessionContext,
) -> BTreeSet<McpCapability> {
    CallerCapabilities::resolve(&CallerEnvelope::from_process_env(env, session))
        .grants()
        .clone()
}
