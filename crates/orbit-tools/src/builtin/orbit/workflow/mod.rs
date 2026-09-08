use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitWorkflowShipTool;
pub struct OrbitWorkflowRunShowTool;
pub struct OrbitWorkflowRunListTool;
pub struct OrbitWorkflowRunResumeTool;
pub struct OrbitWorkflowRunWorkersTool;

fn run_id_param() -> ToolParam {
    ToolParam {
        name: "id".to_string(),
        description: "Job run ID.".to_string(),
        param_type: "string".to_string(),
        required: true,
    }
}

fn execute(
    ctx: &ToolContext,
    input: Value,
    action: OrbitBuiltinAction,
) -> Result<Value, OrbitError> {
    if ctx
        .orbit_host
        .as_ref()
        .is_some_and(|host| host.task_scope().run_id.is_some())
        && matches!(
            action,
            OrbitBuiltinAction::WorkflowShip
                | OrbitBuiltinAction::WorkflowRunResume
                | OrbitBuiltinAction::WorkflowRunWorkers
        )
    {
        return Err(OrbitError::CapabilityDenied(
            "managed runs cannot dispatch, resume, or retune workflow runs; finish the current leaf mandate and let its operator submit follow-up work"
                .to_string(),
        ));
    }
    super::execute_host_action(ctx, input, action)
}

impl Tool for OrbitWorkflowShipTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "task_ids".to_string(),
                description: "Explicit task IDs to ship; at least one is required.".to_string(),
                param_type: "string_list".to_string(),
                required: true,
            },
            ToolParam {
                name: "mode".to_string(),
                description:
                    "Optional ship mode (`pr` or `local`); defaults to workspace configuration."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "base".to_string(),
                description: "Optional base branch override.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "allowed_crews".to_string(),
                description: "Optional configured crews this explicit shipment may dispatch; excluded crews are rejected before provider invocation.".to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "claim_token".to_string(),
                description: "Token for this workspace's exclusive claim, required when another \
                     operator holds one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ];
        parameters.extend(super::model_identity_params());
        ToolSchema {
            name: "orbit.workflow.ship".to_string(),
            description: "Submit explicit tasks to the review-only ship workflow and return its durable run ID. This MCP tool does not accept completion authorization; an authorized operator on the owning host can use `orbit run ship <task-id> --complete`."
                .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        execute(ctx, input, OrbitBuiltinAction::WorkflowShip)
    }
}

impl Tool for OrbitWorkflowRunShowTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.workflow.run.show".to_string(),
            description: "Fetch one durable workflow run by ID.".to_string(),
            parameters: vec![run_id_param()],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        execute(ctx, input, OrbitBuiltinAction::WorkflowRunShow)
    }
}

impl Tool for OrbitWorkflowRunListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.workflow.run.list".to_string(),
            description: "List durable workflow runs newest-first with optional bounded filters."
                .to_string(),
            parameters: vec![
                ToolParam {
                    name: "limit".to_string(),
                    description:
                        "Maximum runs to return (default 25; values above 200 are rejected)."
                            .to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                },
                ToolParam {
                    name: "job_id".to_string(),
                    description: "Optional job ID filter.".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
                ToolParam {
                    name: "state".to_string(),
                    description: "Optional concrete run-state filter, or `terminal`.".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
                ToolParam {
                    name: "since".to_string(),
                    description: "Optional RFC 3339 lower bound for run creation time.".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
            ],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        execute(ctx, input, OrbitBuiltinAction::WorkflowRunList)
    }
}

impl Tool for OrbitWorkflowRunResumeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.workflow.run.resume".to_string(),
            description: "Resume a terminal resumable workflow run as a new linked run."
                .to_string(),
            parameters: vec![
                run_id_param(),
                ToolParam {
                    name: "claim_token".to_string(),
                    description:
                        "Token for this workspace's exclusive claim, required when another \
                     operator holds one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`."
                            .to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
            ],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        execute(ctx, input, OrbitBuiltinAction::WorkflowRunResume)
    }
}

impl Tool for OrbitWorkflowRunWorkersTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.workflow.run.workers".to_string(),
            description: "Adjust how many tasks a running workspace drain keeps in flight, \
                 without replacing its run. The run ID, deadline, completion authorization, \
                 and already-dispatched children are preserved; a lower ceiling stops new \
                 admissions until enough children finish and cancels nothing."
                .to_string(),
            parameters: vec![
                run_id_param(),
                ToolParam {
                    name: "concurrency".to_string(),
                    description:
                        "New ceiling on tasks in flight, from 1 to the ship job's own active-run \
                         limit."
                            .to_string(),
                    param_type: "integer".to_string(),
                    required: true,
                },
                ToolParam {
                    name: "reason".to_string(),
                    description: "Optional note recorded with the change.".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
                ToolParam {
                    name: "if_revision".to_string(),
                    description: "Apply only if the run's ceiling is still at this revision, so a \
                         concurrent adjustment is reported rather than overwritten."
                        .to_string(),
                    param_type: "integer".to_string(),
                    required: false,
                },
                ToolParam {
                    name: "claim_token".to_string(),
                    description:
                        "Token for this workspace's exclusive claim, required when another \
                     operator holds one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`."
                            .to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
            ],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        execute(ctx, input, OrbitBuiltinAction::WorkflowRunWorkers)
    }
}

#[cfg(test)]
mod tests;
