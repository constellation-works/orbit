//! The workflow-run noun's MCP surface table.
//!
//! Every workflow tool is served by the generic operation tool from the
//! registry in `orbit_common::governance::workflow`. What stays here is the
//! MCP-side behaviour the spec does not hold: managed runs are refused before
//! input is checked, the run reads have a bounded view, and the auto-drain
//! status action is a read the desktop projection serves.

use orbit_common::OrbitError;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use orbit_common::governance::workflow::WorkflowVerb;
use orbit_common::protocol::tool_input::{reject_unknown_tool_fields, required_string};
use serde_json::{Value, json};

use super::domain_control::{bounded, bounded_read, ensure_operator_leaf};
use super::operation::{GuardedModes, operation_tool_schema};
use crate::{OrbitBuiltinAction, ToolContext};

impl GuardedModes for WorkflowVerb {
    fn preempt(self, ctx: &ToolContext, input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            WorkflowVerb::Auto => ensure_operator_leaf(ctx)
                .and_then(|()| {
                    orbit_common::governance::workflow::validate_auto_concurrency_input(input)
                })
                .err()
                .map(Err),
            WorkflowVerb::Ship | WorkflowVerb::RunResume => ensure_not_managed(ctx).err().map(Err),
            WorkflowVerb::RunList | WorkflowVerb::RunShow => None,
        }
    }

    fn guarded_mode(
        self,
        ctx: &ToolContext,
        input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        let input = input.as_value();
        match self {
            WorkflowVerb::Auto => auto_status(ctx, input),
            WorkflowVerb::RunList => match bounded(input) {
                Ok(true) => Some(bounded_run_list(ctx, input.clone())),
                Ok(false) => input.get("include_catalog").is_some().then(|| {
                    Err(OrbitError::InvalidInput(
                        "include_catalog requires view:bounded".into(),
                    ))
                }),
                Err(error) => Some(Err(error)),
            },
            WorkflowVerb::RunShow => match bounded(input) {
                Ok(true) => Some(bounded_read(
                    ctx,
                    input.clone(),
                    "run",
                    &["workspace", "view", "id", "log_offset", "limit", "model"],
                )),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            },
            WorkflowVerb::RunResume | WorkflowVerb::Ship => None,
        }
    }

    fn shape_input(self, _ctx: &ToolContext, input: &mut Value) -> Result<(), OrbitError> {
        match self {
            WorkflowVerb::Auto => {
                // Status consumes these selectors above. Keep the control actions'
                // existing refusal of fields that only the readiness read accepts.
                let allowed = self
                    .spec()
                    .mcp_params()
                    .map(|(param, _)| param.name)
                    .filter(|name| !["task", "limit", "allow_crew"].contains(name))
                    .collect::<Vec<_>>();
                reject_unknown_tool_fields(input, &allowed)
            }
            WorkflowVerb::RunList
            | WorkflowVerb::RunResume
            | WorkflowVerb::RunShow
            | WorkflowVerb::Ship => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            WorkflowVerb::Auto
            | WorkflowVerb::RunList
            | WorkflowVerb::RunResume
            | WorkflowVerb::RunShow
            | WorkflowVerb::Ship => None,
        }
    }
}

/// A managed run finishes its own leaf mandate; follow-up dispatch is its
/// operator's.
fn ensure_not_managed(ctx: &ToolContext) -> Result<(), OrbitError> {
    if ctx
        .orbit_host
        .as_ref()
        .is_some_and(|host| host.task_scope().run_id.is_some())
    {
        return Err(OrbitError::CapabilityDenied(
            "managed runs cannot dispatch or resume workflow runs; finish the current leaf mandate and let its operator submit follow-up work"
                .to_string(),
        ));
    }
    Ok(())
}

/// The auto-drain control's explicit workspace, and its status action, which
/// parses selection settings and is served as the drain readiness read. The other
/// actions go to the drain control.
fn auto_status(ctx: &ToolContext, input: &Value) -> Option<Result<Value, OrbitError>> {
    if let Err(error) = required_string(input, &["workspace"], "workspace") {
        return Some(Err(error));
    }
    match required_string(input, &["action"], "action") {
        Ok(action) if action == "status" => {}
        Ok(_) => return None,
        Err(error) => return Some(Err(error)),
    }
    Some(
        reject_unknown_tool_fields(
            input,
            &[
                "workspace",
                "action",
                "model",
                "task",
                "concurrency",
                "limit",
                "allow_crew",
            ],
        )
        .and_then(|()| {
            let mut input = input.clone();
            if let Some(object) = input.as_object_mut() {
                object.remove("action");
            }
            input["scope"] = json!("drain");
            super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopRead)
        }),
    )
}

/// The bounded run list, optionally with the no-input job catalog beside it.
fn bounded_run_list(ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
    let include_catalog = input
        .get("include_catalog")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    super::reject_unknown_tool_arguments(
        &input,
        &operation_tool_schema(WorkflowVerb::RunList.spec()),
    )?;
    let mut run_input = input.clone();
    if let Some(object) = run_input.as_object_mut() {
        object.remove("include_catalog");
    }
    let runs = bounded_read(
        ctx,
        run_input,
        "runs",
        &["workspace", "view", "offset", "limit", "status", "model"],
    )?;
    if !include_catalog {
        return Ok(runs);
    }
    reject_unknown_tool_fields(
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
    let catalog = bounded_read(
        ctx,
        catalog_input,
        "jobs",
        &["workspace", "view", "offset", "limit", "model"],
    )?;
    Ok(json!({"workspace": runs["workspace"], "runs": runs, "catalog": catalog}))
}
