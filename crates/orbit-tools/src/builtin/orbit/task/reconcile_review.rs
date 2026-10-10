//! Operator reconciliation of an already-merged foreign delivery head.
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};
use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

pub struct OrbitTaskReconcileReviewTool;
impl Tool for OrbitTaskReconcileReviewTool {
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
            name: "orbit.task.reconcile_review".into(),
            description: "Reconcile the already-merged head of a recovered follower delivery whose pull request merged at a head other than the handed-off candidate: run required validation and an independent read-only review of exactly that head and record owner-held evidence the desktop review can complete from. `inspect` shows eligibility, `submit` admits a run (resubmitting the same request_key replays it), `status` reports outcomes and next steps, `accept_baseline` records an operator disposition of a failure the base already had. Requires trusted operator authority; unavailable to managed runs.".into(),
            builtin: true,
            parameters: vec![
                param("workspace", "string", false, "Workspace selector from discovery. Omitted, a bound session uses its workspace and an unbound one resolves the task id through this host's task registry"),
                param("action", "string", true, "inspect, submit, status, or accept_baseline"),
                param("id", "string", true, "Task in review whose foreign delivery merged"),
                param("request_key", "string", false, "submit: operator-chosen key; resubmitting it replays the same reconciliation"),
                param("reconciliation_id", "string", false, "status, accept_baseline: the reconciliation to act on"),
                param("command", "string", false, "accept_baseline: the required command whose failure reproduced at the base"),
                param("remediation_commit", "string", false, "accept_baseline: commit on the landing branch, landed on top of the delivery's merge or squash commit, that remediates that failure"),
                param("reason", "string", false, "accept_baseline: why the baseline failure is accepted"),
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
                "managed runs cannot reconcile reviews".into(),
            ));
        }
        super::super::reject_unknown_tool_arguments(&input, &self.schema())?;
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskReconcileReview)
    }
}
