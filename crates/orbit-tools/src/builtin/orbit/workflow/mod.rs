use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitWorkflowShipTool;
pub struct OrbitWorkflowRunShowTool;
pub struct OrbitWorkflowRunDeliveryTool;
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
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.workflow.run.show".to_string(),
            description: "Fetch one durable workflow run by ID.".to_string(),
            parameters: std::iter::once(run_id_param())
                .chain(super::domain_control::bounded_params(&[
                    ("log_offset", "integer", "Bounded log record offset"),
                    ("limit", "integer", "Bounded detail page size"),
                ]))
                .collect(),
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        if super::domain_control::bounded(&input)? {
            return super::domain_control::bounded_read(
                ctx,
                input,
                "run",
                &["workspace", "view", "id", "log_offset", "limit", "model"],
            );
        }
        execute(ctx, input, OrbitBuiltinAction::WorkflowRunShow)
    }
}

impl Tool for OrbitWorkflowRunDeliveryTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.workflow.run.delivery".to_string(),
            description: "Report what one task delivery run committed and landed for one task \
                 of this workspace, from the host's own commit and merge step records. Returns \
                 typed status, base/head and landed commit SHAs, PR number, timestamps and \
                 provenance only; missing or inconsistent evidence is reported as unavailable, \
                 never inferred. Full run details stay on the operator-only `run.show`."
                .to_string(),
            parameters: vec![
                ToolParam {
                    name: "run_id".to_string(),
                    description: "Job run ID that delivered the task.".to_string(),
                    param_type: "string".to_string(),
                    required: true,
                },
                ToolParam {
                    name: "task_id".to_string(),
                    description: "Task the run was submitted with, in this workspace.".to_string(),
                    param_type: "string".to_string(),
                    required: true,
                },
            ],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        execute(ctx, input, OrbitBuiltinAction::WorkflowRunDelivery)
    }
}

impl Tool for OrbitWorkflowRunListTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

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
            ].into_iter().chain(super::domain_control::bounded_params(&[("offset", "integer", "Bounded list offset"),("status", "string", "Bounded run state filter"),("include_catalog", "boolean", "Bounded view only: include a paginated no-input job catalog with last-run metadata alongside runs")])).collect(),
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        if super::domain_control::bounded(&input)? {
            let include_catalog = input
                .get("include_catalog")
                .map(|v| {
                    v.as_bool().ok_or_else(|| {
                        OrbitError::InvalidInput("include_catalog must be a boolean".into())
                    })
                })
                .transpose()?
                .unwrap_or(false);
            super::reject_unknown_tool_arguments(&input, &self.schema())?;
            let mut run_input = input.clone();
            if let Some(object) = run_input.as_object_mut() {
                object.remove("include_catalog");
            }
            let runs = super::domain_control::bounded_read(
                ctx,
                run_input,
                "runs",
                &["workspace", "view", "offset", "limit", "status", "model"],
            )?;
            if include_catalog {
                orbit_common::protocol::tool_input::reject_unknown_tool_fields(
                    &input,
                    &[
                        "workspace",
                        "view",
                        "offset",
                        "limit",
                        "include_catalog",
                        "model",
                    ],
                )?;
                let mut catalog_input = input;
                if let Some(object) = catalog_input.as_object_mut() {
                    object.remove("include_catalog");
                }
                let catalog = super::domain_control::bounded_read(
                    ctx,
                    catalog_input,
                    "jobs",
                    &["workspace", "view", "offset", "limit", "model"],
                )?;
                return Ok(json!({"workspace":runs["workspace"], "runs":runs, "catalog":catalog}));
            }
            return Ok(runs);
        }
        if input.get("include_catalog").is_some() {
            return Err(OrbitError::InvalidInput(
                "include_catalog requires view:bounded".into(),
            ));
        }
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
