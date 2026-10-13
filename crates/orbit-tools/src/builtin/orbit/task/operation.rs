//! The task noun's MCP surface table.
//!
//! The migrated task tools are served by the generic operation tool from the
//! registry in `orbit_common::governance::task`. What stays here is the one
//! MCP-side fact the spec does not hold: `show` and `list` have a dashboard
//! bounded view, and `show` a versioned snapshot, that other host actions
//! serve under their own checks.

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use orbit_common::governance::task::TaskVerb;
use orbit_common::protocol::tool_input::{reject_unknown_tool_fields, required_string};
use serde_json::Value;

use super::super::domain_control::{bounded, bounded_read};
use super::super::operation::GuardedModes;
use crate::{OrbitBuiltinAction, ToolContext};

impl GuardedModes for TaskVerb {
    fn guarded_mode(
        self,
        ctx: &ToolContext,
        input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        let input = input.as_value();
        match self {
            TaskVerb::Show => match bounded(input) {
                Ok(true) => Some(bounded_read(
                    ctx,
                    input.clone(),
                    "task",
                    &[
                        "workspace",
                        "view",
                        "id",
                        "limit",
                        "comments_offset",
                        "history_offset",
                        "artifacts_offset",
                        "model",
                    ],
                )),
                Ok(false) => (input.get("snapshot") == Some(&Value::Bool(true)))
                    .then(|| snapshot(ctx, input.clone())),
                Err(error) => Some(Err(error)),
            },
            TaskVerb::List => match bounded(input) {
                Ok(false) => None,
                Ok(true) => Some(bounded_read(
                    ctx,
                    input.clone(),
                    "tasks",
                    &[
                        "workspace",
                        "view",
                        "offset",
                        "limit",
                        "search",
                        "status",
                        "priority",
                        "model",
                    ],
                )),
                Err(error) => Some(Err(error)),
            },
            TaskVerb::Eligible | TaskVerb::Lint => None,
        }
    }
}

/// The bounded versioned snapshot: one task, an opaque revision and the
/// actions available, for an explicit workspace.
fn snapshot(ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
    reject_unknown_tool_fields(&input, &["workspace", "id", "snapshot", "model"])?;
    required_string(&input, &["workspace"], "workspace")?;
    if let Some(object) = input.as_object_mut() {
        object.remove("snapshot");
    }
    super::super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopTaskSnapshot)
}
