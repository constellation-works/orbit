//! The auto-task noun's MCP surface table.
//!
//! Every auto-task tool is served by the generic operation tool from the
//! registry in `orbit_common::governance::auto_task`. What stays here is the
//! one MCP-side fact the spec does not hold: three verbs have a guarded mode
//! that another host action serves under stricter checks.

use orbit_common::OrbitError;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use orbit_common::protocol::tool_input::{reject_unknown_tool_fields, required_string};
use serde_json::{Value, json};

use super::operation::{GuardedModes, operation_tool_schema};
use crate::{OrbitBuiltinAction, ToolContext};

impl GuardedModes for AutoTaskVerb {
    fn guarded_mode(
        self,
        ctx: &ToolContext,
        input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        let input = input.as_value();
        match self {
            AutoTaskVerb::List => match super::domain_control::bounded(input) {
                Ok(false) => None,
                Ok(true) => Some(super::domain_control::bounded_read(
                    ctx,
                    input.clone(),
                    "auto_tasks",
                    &["workspace", "view", "offset", "limit", "model"],
                )),
                Err(error) => Some(Err(error)),
            },
            AutoTaskVerb::Mint => input
                .get("acknowledge_unconditional")
                .is_some()
                .then(|| acknowledged_mint(ctx, input.clone())),
            AutoTaskVerb::Update => input
                .get("expected_enabled")
                .is_some()
                .then(|| checked_toggle(ctx, input.clone())),
            AutoTaskVerb::Add
            | AutoTaskVerb::Show
            | AutoTaskVerb::Toggle
            | AutoTaskVerb::Delete
            | AutoTaskVerb::Restore
            | AutoTaskVerb::Recover
            | AutoTaskVerb::Reset => None,
        }
    }
}

/// An unconditional mint the caller acknowledged, served as an operator
/// automation control.
fn acknowledged_mint(ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
    super::domain_control::ensure_operator_leaf(ctx)?;
    super::reject_unknown_tool_arguments(
        &input,
        &operation_tool_schema(AutoTaskVerb::Mint.spec()),
    )?;
    required_string(&input, &["workspace"], "workspace")?;
    input["action"] = json!("mint");
    input["kind"] = json!("auto_task");
    super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopAutomation)
}

/// The checked toggle is its own operation: it changes `enabled` and nothing
/// else, so a refused compare leaves no partial edit.
fn checked_toggle(ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
    super::domain_control::ensure_operator_leaf(ctx)?;
    reject_unknown_tool_fields(
        &input,
        &["name", "enabled", "expected_enabled", "workspace", "model"],
    )?;
    required_string(&input, &["workspace"], "workspace")?;
    if input.get("enabled").is_none() {
        return Err(OrbitError::InvalidInput(
            "`expected_enabled` requires the desired `enabled` state".to_string(),
        ));
    }
    input["action"] = json!("toggle");
    input["kind"] = json!("auto_task");
    super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopAutomation)
}
