//! Operator decision to renew a review lineage's budget.
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};
use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

pub struct OrbitTaskReviewResetTool;
impl Tool for OrbitTaskReviewResetTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }
    fn schema(&self) -> ToolSchema {
        let param = |name: &str, ty: &str, required: bool, description: &str| ToolParam {
            name: name.into(),
            param_type: ty.into(),
            required,
            description: description.into(),
        };
        ToolSchema {
            name: "orbit.task.review_reset".into(),
            description: "Record an operator decision closing the open review attempt and renewing one explicitly selected lineage budget while preserving every attempt. Requires trusted operator authority; unavailable to managed runs. Inspect the returned ledger after a lost reply before retrying.".into(),
            builtin: true,
            parameters: vec![
                param("workspace", "string", false, "Workspace selector from discovery. Omitted, a bound session uses its workspace and an unbound one resolves the task id through this host's task registry"),
                param("id", "string", true, "Task in the selected lineage"),
                param("lineage_key", "string", true, "Exact lineage key from the admission refusal or review manifest"),
                param("reason", "string", true, "Required explanation retained in the decision history"),
                param("adopt_configured_budget", "boolean", false, "Explicitly adopt current configured review limits; default retains the captured limits"),
            ],
        }
    }
    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        if ctx
            .orbit_host
            .as_ref()
            .is_some_and(|host| host.task_scope().run_id.is_some())
        {
            return Err(OrbitError::CapabilityDenied(
                "managed runs cannot reset review budgets".into(),
            ));
        }
        super::super::reject_unknown_tool_arguments(&input, &self.schema())?;
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskReviewReset)
    }
}
