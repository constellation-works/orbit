//! The task noun's MCP surface table.
//!
//! The migrated task tools are served by the generic operation tool from the
//! registry in `orbit_common::governance::task`. What stays here is what the
//! spec does not hold: `show` and `list` have a dashboard bounded view, and
//! `show` a versioned snapshot, that other host actions serve under their own
//! checks; `add` and `update` have a guarded write mode with its own input
//! contract and schema conditionals, and `update` refuses an implementation
//! activity's `blocked` status before any field check.

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use orbit_common::governance::task::{TASK_CLI_OPTIONS_PARAM, TaskVerb};
use orbit_common::protocol::tool_input::{reject_unknown_tool_fields, required_string};
use serde_json::Value;

use super::super::domain_control::{bounded, bounded_read};
use super::super::operation::GuardedModes;
use super::super::{apply_session_orchestrator_default, resolve_workspace_argument};
use super::guarded;
use crate::{OrbitBuiltinAction, ToolContext};

impl GuardedModes for TaskVerb {
    fn preempt(self, ctx: &ToolContext, input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            TaskVerb::Add => {
                guarded::is_guarded(input).then(|| guarded::write(ctx, input.clone(), true))
            }
            TaskVerb::Update => {
                // [ORB-14269] Before both the guarded edit and the ordinary
                // update. An implementation activity used to set `blocked`
                // mid-run; the executor never re-reads status, so later
                // steps still ran.
                if implementation_activity_sets_blocked(ctx, input) {
                    return Some(Err(OrbitError::InvalidInput(
                        "an implementation activity cannot set status to blocked; return result.blocker as {kind, evidence} so the step boundary records the blocker and stops the run"
                            .to_string(),
                    )));
                }
                if guarded::is_guarded(input) {
                    return Some(guarded::write(ctx, input.clone(), false));
                }
                // The spec reads JSON null as absent; an ordinary update
                // refuses a null crew rather than keeping the current one.
                (input.get("crew") == Some(&Value::Null)).then(|| {
                    Err(OrbitError::InvalidInput(
                        "`crew` must be a string; omit it to keep the current crew or use an empty string to redraw"
                            .to_string(),
                    ))
                })
            }
            TaskVerb::Show | TaskVerb::List | TaskVerb::Eligible | TaskVerb::Lint => None,
        }
    }

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
            TaskVerb::Add | TaskVerb::Update | TaskVerb::Eligible | TaskVerb::Lint => None,
        }
    }

    fn shape_input(self, ctx: &ToolContext, input: &mut Value) -> Result<(), OrbitError> {
        match self {
            TaskVerb::Add => {
                // The operator command leaves its text to the domain's own
                // checks, and its description defaults to empty; MCP callers
                // must title and describe the task.
                if input.get(TASK_CLI_OPTIONS_PARAM).is_none() {
                    required_string(input, &["title"], "title")?;
                    required_string(input, &["description"], "description")?;
                }
                required_string(input, &["complexity"], "complexity")?;
                resolve_workspace_argument(ctx, input, "orbit.task.add")?;
                apply_session_orchestrator_default(ctx, input);
                Ok(())
            }
            TaskVerb::Update
            | TaskVerb::Show
            | TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Lint => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            TaskVerb::Add => Some(guarded::extend_create_schema),
            TaskVerb::Update => Some(guarded::extend_update_schema),
            TaskVerb::Show | TaskVerb::List | TaskVerb::Eligible | TaskVerb::Lint => None,
        }
    }
}

/// `implement_one` and a resolved `agent_implement` target. Deny mode stamps
/// the step id as the activity name; the catalog name is accepted too.
fn implementation_activity_sets_blocked(ctx: &ToolContext, input: &Value) -> bool {
    let Some(activity) = ctx
        .tool_deny_policy
        .as_ref()
        .map(|policy| policy.activity.as_str())
    else {
        return false;
    };
    if activity != "implement_one" && activity != "agent_implement" {
        return false;
    }
    input
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("blocked"))
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
